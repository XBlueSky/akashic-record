//! Integration test for C2 graceful shutdown.
//!
//! D2 (2026-05-09) unblocked this test: `TestEnv` (tests/common/mod.rs)
//! provisions PG + Neo4j containers, and an embedded wiremock server
//! stands in for the OpenAI embeddings endpoint so the binary's
//! `init_archetypes` call succeeds without network egress. Runs
//! unattended via `cargo test --test shutdown_drain` (no `--ignored`).

mod common;

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Pick a free localhost TCP port by binding to port 0 and immediately
/// releasing it. Racy in principle (another process could grab the port
/// between drop and re-bind), but acceptable for a `#[serial]` integration
/// test on a controlled CI/dev host.
fn pick_free_port() -> u16 {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port for port-pick");
    listener.local_addr().expect("local_addr").port()
}

#[tokio::test]
#[serial_test::serial]
async fn sigterm_yields_clean_exit_within_cap() {
    let env = common::TestEnv::start().await;

    // Wiremock stub for the OpenAI embeddings endpoint. `main.rs` calls
    // `init_archetypes` during boot, which embeds two archetype strings
    // before the "API server listening" line is logged. Without this
    // stub the binary would either hang or exit non-zero on a network
    // failure, depending on outbound DNS behavior on the test host.
    let mock_openai = MockServer::start().await;
    let zero_vec: Vec<f32> = vec![0.0; 1536];
    let response_body = serde_json::json!({
        "data": [{"embedding": zero_vec}],
        "usage": {"total_tokens": 0},
        "model": "text-embedding-3-small",
    });
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .mount(&mock_openai)
        .await;
    let mock_base_url = mock_openai.uri(); // e.g. http://127.0.0.1:54321

    // Pre-pick a free port for the binary's one TCP listener (Task 2: MCP
    // is now a `/mcp` branch of this same REST app, not a second listener).
    // `#[serial]` keeps this stable for the duration of this test.
    let api_port = pick_free_port();

    let mut child = Command::new(env!("CARGO_BIN_EXE_akashic-server"))
        // ── Database fixtures (from TestEnv) ─────────────────────────
        .env("DATABASE_URL", &env.pg_url)
        .env("NEO4J_URI", &env.neo4j_url)
        .env("NEO4J_USER", &env.neo4j_user)
        .env("NEO4J_PASSWORD", &env.neo4j_pass)
        // ── Embedding provider — wiremock-shaped OpenAI endpoint ─────
        .env("EMBEDDING_PROVIDER", "openai")
        .env("EMBEDDING_API_KEY", "test-bypass-key")
        .env("EMBEDDING_MODEL", "text-embedding-3-small")
        .env("EMBEDDING_BASE_URL", &mock_base_url)
        // ── LLM provider — local no-op (no network) ──────────────────
        .env("LLM_PROVIDER", "local")
        // ── GitLab (validation-off mode tolerates empties) ───────────
        .env("GITLAB_URL", "https://gitlab.test.example.org")
        .env("GITLAB_APP_ID", "test-app-id")
        .env("GITLAB_APP_SECRET", "test-app-secret")
        .env("OAUTH_VALIDATION_MODE", "off")
        // ── Frontend / public URL ────────────────────────────────────
        .env("FRONTEND_URL", "http://localhost:3000")
        .env("COOKIE_SECURE", "false")
        // ── Ports — pre-picked free local ────────────────────────────
        .env("API_HOST", "127.0.0.1")
        .env("API_PORT", api_port.to_string())
        // ── Migrations — TestEnv already ran Up. We pick `true` (not
        //    `false` → Verify) because there is a pre-existing C3 bug in
        //    backend/src/migrate.rs: `PG_TABLES` lists table names
        //    `device_codes` and `passthrough_revoked` that don't match
        //    the actual CREATE TABLE statements (`device_flow_pending`
        //    and `revoked_passthrough_tokens`). Verify would hard-fail.
        //    Re-running `Up` against the already-migrated bench is a
        //    no-op (everything is `CREATE … IF NOT EXISTS`). The verify
        //    bug is out of scope for D2 and tracked separately.
        .env("MIGRATE_ON_BOOT", "true")
        // ── Suppress dotenvy picking up dev .env at the repo root ────
        // (workspace `.env`, if present, would override our settings)
        .env("AKASHIC_ENV", "")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn backend binary");

    let pid = child.id();

    let stdout = child.stdout.take().expect("capture stdout");
    let stderr = child.stderr.take().expect("capture stderr");
    let mut log_lines: Vec<String> = Vec::new();
    let start = Instant::now();

    let mut reader = BufReader::new(stdout);
    let ready = loop {
        if start.elapsed() > Duration::from_secs(90) {
            break false;
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break false,
            Ok(_) => {
                let saw_ready = line.contains("API server listening");
                log_lines.push(line);
                if saw_ready {
                    break true;
                }
            }
            Err(_) => break false,
        }
    };
    if !ready {
        // Drain stderr to surface the real failure mode.
        let mut stderr_buf = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut stderr_buf);
        let _ = child.kill();
        panic!(
            "backend did not reach 'API server listening'; stdout so far:\n{}\nstderr:\n{}",
            log_lines.join(""),
            stderr_buf
        );
    }

    // SIGTERM via libc::kill — works on unix only (matches the Linux container target).
    let kill_result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    assert_eq!(
        kill_result,
        0,
        "kill(SIGTERM) failed: errno {}",
        std::io::Error::last_os_error()
    );

    let shutdown_start = Instant::now();

    // Wait up to 70s for clean exit (60s cap + 10s buffer).
    let exit = loop {
        if shutdown_start.elapsed() > Duration::from_secs(70) {
            let _ = child.kill();
            panic!("backend did not exit within 70s of SIGTERM");
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => panic!("try_wait failed: {e}"),
        }
    };
    let shutdown_dur = shutdown_start.elapsed();

    // Drain remaining stdout + stderr into log_lines for the signature check.
    let mut tail = String::new();
    let _ = reader.read_to_string(&mut tail);
    log_lines.push(tail);
    let mut stderr_buf = String::new();
    let _ = BufReader::new(stderr).read_to_string(&mut stderr_buf);
    log_lines.push(stderr_buf);
    let log = log_lines.join("");

    assert!(
        exit.success(),
        "backend exited with non-zero status: {exit:?}\nlog:\n{log}"
    );

    let required = [
        "shutdown_signal_received",
        "shutdown_drain_started",
        "shutdown_done",
    ];
    for ev in required {
        assert!(log.contains(ev), "log missing event {ev}; full log:\n{log}");
    }
    assert!(
        log.contains("drain_complete") || log.contains("drain_timeout_force_exit"),
        "log missing drain_complete | drain_timeout_force_exit; full log:\n{log}"
    );

    eprintln!("c2 drain duration: {shutdown_dur:?}");
}
