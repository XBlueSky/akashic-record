#![cfg(feature = "test-fixtures")]
//! D6 alerts integration smoke. Spins the D2 bench, overrides
//! `state.config.alerts` to point at a mockito server with a 1s tick,
//! spawns the evaluator manually against the bench's `state`, drives the
//! metric backing R1 (`akashic_http_requests_total{status="500"}`)
//! directly via the `metrics` crate, waits for the evaluator's next
//! tick, and asserts mockito received the expected webhook POST.
//!
//! Why not env-var-driven config?
//! `common::TestEnv` constructs `Config` via `build_test_config` (a
//! direct struct literal) rather than `Config::from_env`, so setting
//! `ALERTS_*` env vars before `TestEnv::start()` would NOT propagate.
//! We instead clone the AppState with an overridden `AlertsConfig` and
//! spawn the evaluator ourselves — that exercises the same
//! `start_evaluator` entry point `main.rs` uses, just with a tighter
//! tick for the test's timescale.
//!
//! Why not test `main`'s startup directly?
//! The bench already serves the production router on an ephemeral port;
//! the only piece of `main.rs` we need to re-execute is the single
//! `start_evaluator(state, token)` call. Standing up a second copy of
//! main just to assert this would multiply container cold-start without
//! covering additional surface.

mod common;

use std::time::Duration;

use common::TestEnv;
use tokio_util::sync::CancellationToken;

use akashic_config::AlertsConfig;

#[tokio::test]
#[serial_test::serial]
async fn alerts_r1_5xx_fires_to_webhook() {
    // 0. Install a tracing subscriber so the evaluator's `info!` lines
    //    (`alerts_evaluator_started`, `alerts_fired`) reach the test
    //    stdout under `--nocapture`. Idempotent across reruns — only
    //    the first install per process succeeds; subsequent installs
    //    are silently ignored.
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    // 1. Stand up a mockito server. The evaluator's WebhookSink will
    //    POST to mock_server.url() (no path appended — see
    //    `WebhookSink::send` in src/alerts/sink.rs).
    let mut mock_server = mockito::Server::new_async().await;
    let mock = mock_server
        .mock("POST", "/")
        .with_status(200)
        .with_body("ok")
        .expect_at_least(1)
        .create_async()
        .await;

    // 2. Bring up the bench. The bench installs a process-global
    //    Prometheus recorder via `metrics_exporter_prometheus::install_recorder`;
    //    once installed, all `metrics::counter!` calls from this test
    //    process land in the same recorder that `state.metrics_handle`
    //    renders from.
    let env = TestEnv::start().await;

    // 3. Override the alerts config for this test only and spawn the
    //    evaluator pointing at the mockito server. tick=1s,
    //    cooldown=5s — fast enough to assert within seconds, generous
    //    enough that the first tick is the one the test waits for.
    let mut state = env.state.clone();
    state.config.alerts = AlertsConfig {
        webhook_url: Some(mock_server.url()),
        tick_secs: 1,
        cooldown_secs: 5,
    };
    let shutdown = CancellationToken::new();
    let _handle = akashic_record::alerts::start_evaluator(state, shutdown.clone());

    // 4. Wait for the evaluator's baseline tick. The evaluator:
    //    (a) burns the immediate first tick (evaluator.rs line 53),
    //    (b) on its first REAL tick, calls
    //        `state.observe_counter("akashic_http_5xx_total", total)`
    //        which seeds `(prev=cur=total)` — by design, this returns
    //        zero delta so cold-boot doesn't generate spurious fire
    //        (`prev_counters.get(key).copied().unwrap_or(current)`).
    //
    //    So we must drive the counter AFTER tick (a)+(b) have run.
    //    With tick_secs=1, sleeping ~1.5s lands cleanly between
    //    tick #1 (baseline) and tick #2 (delta observation).
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // 5. Drive R1: spike `akashic_http_requests_total{status="500"}` by
    //    10. The evaluator's NEXT tick reads the new render, calls
    //    observe_counter again, computes `delta = 10 - 0 = 10`,
    //    `per_min = delta * 60 / tick_secs = 600`, which exceeds
    //    R1_THRESHOLD_PER_MIN (5.0/min) → fires.
    for _ in 0..10 {
        metrics::counter!(
            "akashic_http_requests_total",
            "method" => "GET",
            "route" => "/test",
            "status" => "500",
        )
        .increment(1);
    }

    // 6. Wait long enough for the delta-observing tick + sink POST to
    //    finish. 3s headroom over the 1s tick avoids flakes if the
    //    increment lands just after a tick boundary.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // 7. Assert mockito received at least one POST. If this fails, look
    //    at the test's tracing output for the `alerts_evaluator_started`
    //    log line and the `alerts_fired` line — those confirm whether
    //    the evaluator booted and whether the rule produced an alert
    //    before the sink call.
    mock.assert_async().await;

    // Cleanup: cancel the evaluator so it doesn't keep ticking after the
    //          test returns (it would race with the next test's metric
    //          drive otherwise). `_handle` is dropped at scope end.
    shutdown.cancel();
    drop(env);
}
