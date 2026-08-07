//! Live-DB proof of Roadmap F's crash-consistency guarantee: below-threshold
//! completeness aborts with ZERO database writes; above-threshold commits a
//! graph equivalent to what today's (pre-Roadmap-F) streaming pipeline would
//! have produced; Stage 8/9 still run, unchanged, after a successful commit.
//!
//! ```bash
//! TEST_DATABASE_URL=postgres://akashic:${PG_PASSWORD}@localhost:5432/akashic \
//! TEST_NEO4J_URI=bolt://localhost:7687 TEST_NEO4J_PASSWORD=akashic_secret \
//! cargo test -p akashic-ingestion --jobs 1 ram_first -- --ignored --nocapture
//! ```
//!
//! **Helper pattern (same drift note as `snapshot_e2e_test.rs`):** `e2e_test.rs`
//! keeps its `FakeEmbedder`/`FakeLlm`/`test_config`/`write_fixture` helpers
//! module-private (not `pub(crate)`), and there is no DB-pool helper — every
//! sibling test inlines `akashic_store_pg::connect` + `Neo4jPool::connect` +
//! schema init directly. This module follows the established
//! `snapshot_e2e_test.rs` precedent: it re-declares those helpers verbatim
//! (copied field-for-field) rather than widening `e2e_test.rs`'s visibility.
//!
//! **Path choice — `start()` not `run_sync()`:** both new tests drive the
//! PRODUCTION path (`start()`, `preserve_existing = false`) rather than the
//! CLI's `run_sync()` (`preserve_existing = true`). `start()` is the only path
//! whose commit gate would (on success) wipe + re-commit the repo, so it is
//! the path where the crash-consistency guarantee actually bites: a
//! below-threshold abort must land BEFORE that wipe. `run_sync()` never wipes
//! at all, so a zero-writes proof over it would be strictly weaker. This also
//! matches the exact invocation of `e2e_ingestion_against_live_dbs` (the
//! established full-pipeline baseline this module's parity test reuses).

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use secrecy::SecretString;
use tokio::sync::{Semaphore, broadcast};
use uuid::Uuid;

use akashic_embed::{EmbeddingProvider, EmbeddingResponse};
use akashic_llm::{LlmProvider, LlmResponse, LlmUsage};
use akashic_store_neo4j::Neo4jPool;

use crate::ingestion::pipeline::{IngestRequest, IngestionPipeline};

/// Same embedding width as `e2e_test.rs::DIM` — must match the live
/// `chunks.embedding` column (`vector(1536)`), else `init_schema`'s
/// `migrate_vector_dims` would treat this as a width change and wipe data.
const DIM: usize = 1536;

const REPO: &str = "e2e-ram-first-below-threshold";
const REPO_PARITY: &str = "e2e-ram-first-parity";

/// Deterministic fake embedder — a copy of `e2e_test.rs::FakeEmbedder`.
struct FakeEmbedder;

#[async_trait]
impl EmbeddingProvider for FakeEmbedder {
    async fn embed(&self, text: &str) -> Result<EmbeddingResponse> {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in text.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let mut v = Vec::with_capacity(DIM);
        let mut state = h | 1;
        for _ in 0..DIM {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let f = ((state % 1000) as f32) / 1000.0 + 0.001;
            v.push(f);
        }
        Ok(EmbeddingResponse {
            vector: v,
            tokens_used: 0,
            model: "fake".into(),
        })
    }

    fn dimensions(&self) -> usize {
        DIM
    }
}

/// Fake LLM returning benign empty JSON — safe through Stage 9 (see the Stage 9
/// risk analysis in `snapshot_e2e_test.rs`'s module doc comment).
struct FakeLlm;

#[async_trait]
impl LlmProvider for FakeLlm {
    async fn generate_json(&self, _prompt: &str) -> Result<LlmResponse> {
        Ok(LlmResponse {
            text: "{}".to_string(),
            usage: LlmUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
            model: "fake".to_string(),
        })
    }
}

/// Copy of `e2e_test.rs::test_config`. Keeps `ingest_completeness_threshold` at
/// the default `1.0`, which is exactly what makes a single unreadable file
/// trip the commit gate in the below-threshold test.
fn test_config(database_url: &str, neo4j_uri: &str) -> akashic_config::Config {
    use akashic_config::{
        AiProvider, AlertsConfig, Config, EmbeddingConfig, EmbeddingPrecision, LlmConfig,
    };
    Config {
        neo4j_uri: neo4j_uri.to_string(),
        neo4j_user: std::env::var("TEST_NEO4J_USER").unwrap_or_else(|_| "neo4j".into()),
        neo4j_password: SecretString::from(
            std::env::var("TEST_NEO4J_PASSWORD").unwrap_or_else(|_| "changeme".into()),
        ),
        database_url: database_url.to_string(),
        embedding: EmbeddingConfig {
            provider: AiProvider::Local,
            api_key: None,
            model: "fake".into(),
            base_url: None,
        },
        llm: LlmConfig {
            provider: AiProvider::Local,
            api_key: None,
            model: "fake".into(),
            base_url: None,
        },
        // Every directory its own module: never roll up, never split.
        module_max_files: 1000,
        module_min_files: 1,
        api_host: "127.0.0.1".into(),
        gitlab_webhook_secret: None,
        gitlab_url: "http://localhost".into(),
        gitlab_app_id: String::new(),
        gitlab_app_secret: SecretString::from(String::new()),
        gitlab_redirect_uri: "http://localhost/cb".into(),
        gitlab_web_redirect_uri: "http://localhost/wcb".into(),
        auth_code_ttl_secs: 300,
        api_key_ttl_secs: 86400,
        gitlab_service_token: None,
        frontend_url: "http://localhost:3000".into(),
        cors_extra_origins: vec![],
        cookie_secure: false,
        api_port: 8081,
        ingest_clone_dir: std::env::temp_dir()
            .join("akashic-ram-first-e2e-clone")
            .to_string_lossy()
            .into_owned(),
        ingest_max_file_size: 1_048_576,
        ingest_max_lines: 5000,
        ingest_chunk_max_size: 5120,
        ingest_concurrent_jobs: 1,
        ingest_skip_patterns: vec!["node_modules".into(), ".git".into(), "target".into()],
        ingest_presets_path: None,
        ingest_completeness_threshold: 1.0,
        admin_users: vec![],
        ingest_crawl_max_pages: 10,
        ingest_crawl_delay_ms: 0,
        embedding_precision: EmbeddingPrecision::Float32,
        rate_limit_enabled: false,
        rate_limit_trusted_proxies: vec![],
        rate_limit_allowlist: vec![],
        alerts: AlertsConfig::default(),
        public_base_url: "http://127.0.0.1:0".to_string(),
        mcp_quota_tokens_per_window: 100_000,
        mcp_quota_window_secs: 3600,
        mcp_quota_enabled: false,
        mcp_passthrough_user_cache_ttl_secs: 60,
        oauth_validation_mode: "off".to_string(),
        migrate_on_boot: "false".to_string(),
        ingest_quota_tokens_per_window: 5_000_000,
        ingest_quota_window_secs: 3600,
        ingest_quota_enabled: false,
        mcp_cimd_allow_loopback: false,
    }
}

/// Connect to the live TEST_* Postgres + Neo4j and ensure schema exists — same
/// idempotent preamble every test in `e2e_test.rs` runs.
async fn connect_and_init_schema() -> (sqlx::PgPool, Neo4jPool, akashic_config::Config) {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://akashic@localhost:5432/akashic".into());
    let neo4j_uri =
        std::env::var("TEST_NEO4J_URI").unwrap_or_else(|_| "bolt://localhost:7687".into());
    let cfg = test_config(&database_url, &neo4j_uri);

    let pg = akashic_store_pg::connect(&database_url)
        .await
        .expect("connect Postgres");
    let neo4j = Neo4jPool::connect(&cfg).await.expect("connect Neo4j");

    let vec_type = cfg.vector_type(DIM);
    let cos_ops = cfg.cosine_ops();
    akashic_store_pg::init_schema(&pg, DIM, &vec_type, cos_ops)
        .await
        .expect("pg init_schema");
    akashic_store_neo4j::schema::init_schema(&neo4j, DIM)
        .await
        .expect("neo4j init_schema");

    (pg, neo4j, cfg)
}

/// Delete every trace of `repo` from both DBs — communities first (FK), then
/// chunks/modules/jobs, plus the notes this module seeds. Run at start (for
/// idempotency) and end (cleanup) of each test.
async fn clean_repo(pg: &sqlx::PgPool, neo4j: &Neo4jPool, repo: &str) {
    sqlx::query("DELETE FROM communities WHERE repo_name = $1")
        .bind(repo)
        .execute(pg)
        .await
        .expect("clean communities");
    sqlx::query("DELETE FROM large_chunks WHERE repo_name = $1")
        .bind(repo)
        .execute(pg)
        .await
        .expect("clean large_chunks");
    sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
        .bind(repo)
        .execute(pg)
        .await
        .expect("clean chunks");
    sqlx::query("DELETE FROM modules WHERE repo_name = $1")
        .bind(repo)
        .execute(pg)
        .await
        .expect("clean modules");
    sqlx::query("DELETE FROM ingestion_jobs WHERE repo_name = $1")
        .bind(repo)
        .execute(pg)
        .await
        .expect("clean ingestion_jobs");
    // Notes are NOT touched by `cleanup()`/`clean_old_data` — clear explicitly
    // so the parity test's seeded note doesn't survive across runs.
    sqlx::query("DELETE FROM notes WHERE repo_name = $1")
        .bind(repo)
        .execute(pg)
        .await
        .expect("clean notes");

    use neo4rs::query;
    for label in ["Chunk", "Module", "Community", "Flow"] {
        neo4j
            .execute(
                query(&format!(
                    "MATCH (n:{label} {{repo_name: $r}}) DETACH DELETE n"
                ))
                .param("r", repo),
            )
            .await
            .expect("clean neo4j nodes");
    }
    neo4j
        .execute(query("MATCH (n:Note {repo_name: $r}) DETACH DELETE n").param("r", repo))
        .await
        .expect("clean neo4j notes");
    neo4j
        .execute(query("MATCH (r:Repository {name: $r}) DETACH DELETE r").param("r", repo))
        .await
        .expect("clean neo4j repository");
}

/// Build a pipeline with the fake providers, exactly as `e2e_test.rs` does.
fn build_pipeline(
    pg: &sqlx::PgPool,
    neo4j: &Neo4jPool,
    cfg: akashic_config::Config,
) -> IngestionPipeline {
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbedder);
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlm);
    let semaphore = Arc::new(Semaphore::new(1));
    let event_tx = broadcast::channel(256).0;
    IngestionPipeline::new(
        pg.clone(),
        neo4j.clone(),
        embedder,
        llm,
        cfg,
        semaphore,
        event_tx,
    )
}

/// A local-source ingest request for `repo` rooted at `path`.
fn local_req(repo: &str, path: &std::path::Path) -> IngestRequest {
    IngestRequest {
        repo_name: repo.to_string(),
        git_ref: "main".to_string(),
        source: "local".to_string(),
        local_path: Some(path.to_string_lossy().into_owned()),
        user_token: None,
        seed_url: None,
        crawl_depth: None,
        url_pattern: None,
    }
}

/// Poll the `ingestion_jobs` row until it reaches a terminal status
/// (`"done"`/`"failed"`) or the deadline elapses; return `(status, error)`.
async fn poll_terminal(pg: &sqlx::PgPool, job_id: Uuid) -> (String, Option<String>) {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last = String::new();
    loop {
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT status, error_message FROM ingestion_jobs WHERE id = $1")
                .bind(job_id)
                .fetch_optional(pg)
                .await
                .expect("poll job status");
        if let Some((status, err)) = row {
            if status != last {
                eprintln!("ram_first: job status = {status}");
                last = status.clone();
            }
            if status == "done" || status == "failed" {
                return (status, err);
            }
        }
        if Instant::now() > deadline {
            panic!("job {job_id} did not reach a terminal status within 90s (last: {last})");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Fixture for the below-threshold test: two genuinely-valid Rust files (their
/// content is copied verbatim from `e2e_test.rs::write_fixture`, which passing
/// tests prove parses into chunks) plus one file with the SAME `.rs` extension
/// whose bytes are NOT valid UTF-8. `chunk_and_group`'s per-file loop reads
/// each file with `std::fs::read_to_string`, which returns `Err(InvalidData)`
/// on the invalid byte (empirically confirmed: "stream did not contain valid
/// UTF-8"), pushing a `FailedFile`. With 3 discovered files and 1 failure the
/// completeness ratio is 2/3 < 1.0, tripping the commit gate.
fn write_valid_and_broken_fixture(root: &std::path::Path) -> Result<()> {
    let src = root.join("src");
    std::fs::create_dir_all(&src)?;

    // Valid file #1 — `double` calls `add` (verbatim from write_fixture).
    std::fs::write(
        src.join("math.rs"),
        "/// Add two integers together and return the sum.\n\
         pub fn add(a: i32, b: i32) -> i32 {\n    \
             let result = a + b;\n    \
             result\n\
         }\n\n\
         /// Double a value by adding it to itself via `add`.\n\
         pub fn double(x: i32) -> i32 {\n    \
             let doubled = add(x, x);\n    \
             doubled\n\
         }\n",
    )?;

    // Valid file #2 — a padded standalone fn (clears MIN_CHUNK_SIZE).
    std::fs::write(
        src.join("util.rs"),
        "/// Return a constant used elsewhere in the below-threshold fixture.\n\
         pub fn constant_value() -> i32 {\n    \
             let value = 7;\n    \
             value\n\
         }\n",
    )?;

    // Invalid file — ASCII prefix that looks like Rust, then a lone 0xFF byte
    // (never valid in UTF-8) so `read_to_string` rejects the whole file.
    let mut broken =
        b"pub fn broken_unreadable() {\n    // invalid UTF-8 byte follows\n    let _x = 1;\n}\n"
            .to_vec();
    broken.push(0xFF);
    broken.extend_from_slice(&[0xFE, 0x00, 0x80]);
    std::fs::write(src.join("broken.rs"), &broken)?;

    Ok(())
}

/// The exact multi-language fixture `e2e_ingestion_against_live_dbs` ingests —
/// copied byte-for-byte from `e2e_test.rs::write_fixture` so the parity test's
/// graph counts are the SAME established baseline that test already asserts,
/// not numbers invented for this test.
fn write_fixture(root: &std::path::Path) -> Result<()> {
    let src = root.join("src");
    std::fs::create_dir_all(&src)?;

    // Rust: `double` calls `add`.
    std::fs::write(
        src.join("math.rs"),
        "/// Add two integers together and return the sum.\n\
         pub fn add(a: i32, b: i32) -> i32 {\n    \
             let result = a + b;\n    \
             result\n\
         }\n\n\
         /// Double a value by adding it to itself via `add`.\n\
         pub fn double(x: i32) -> i32 {\n    \
             let doubled = add(x, x);\n    \
             doubled\n\
         }\n",
    )?;

    // TypeScript: `run` calls `helper`.
    std::fs::write(
        src.join("util.ts"),
        "// Return a constant helper value used by run().\n\
         export function helper(): number {\n    \
             const value = 1;\n    \
             return value;\n\
         }\n\n\
         // Run the module by delegating to helper().\n\
         export function run(): number {\n    \
             const outcome = helper();\n    \
             return outcome;\n\
         }\n",
    )?;

    // Python: a simple entry-point-ish function.
    std::fs::write(
        src.join("app.py"),
        "def main():\n    \
             \"\"\"Program entry point for the e2e fixture.\"\"\"\n    \
             message = \"hello from app\"\n    \
             print(message)\n",
    )?;

    // Markdown doc with headings.
    std::fs::write(
        root.join("README.md"),
        "# E2E Fixture\n\n## Overview\n\nTiny fixture repo.\n\n## Usage\n\nNothing.\n",
    )?;

    Ok(())
}

/// Insert a note into `repo` referencing `symbols` (which the fixture DOES
/// define) with a non-null embedding, returning its id. Stage 8
/// (`detect_staleness`) computes an embedding-divergence between this note and
/// the chunks named by `symbols`; that query needs those chunks to already
/// exist, so it errors out (aggregate over zero rows → NULL, undecodable as
/// `f64`) unless the chunks are present — i.e. unless Stage 8 runs strictly
/// AFTER the commit landed them. When it succeeds it stamps
/// `last_verified_at`, which the parity test asserts on. (Referencing a symbol
/// the fixture does NOT define hits exactly that zero-row error and silently
/// skips `update_staleness` — a pre-existing curation quirk, unrelated to
/// Roadmap F — which is why this probe references real fixture symbols.)
/// `category = "DECISION"` matches the value the passing
/// `e2e_supersede_note_strict_compensation` test inserts, so it is a
/// known-valid category on the live schema.
async fn seed_staleness_probe_note(pg: &sqlx::PgPool, repo: &str, symbols: &[&str]) -> Uuid {
    use akashic_domain::ports::{NoteInsertRow, NoteRepo};
    use akashic_store_pg::PgNoteRepo;

    let note_repo = PgNoteRepo::new(pg.clone());
    note_repo
        .insert_note(NoteInsertRow {
            repo_name: repo.to_string(),
            branch: "main".to_string(),
            category: "DECISION".to_string(),
            title: "RAM-first parity — Stage 8 staleness probe".to_string(),
            summary: "Note referencing fixture symbols so Stage 8 has real work.".to_string(),
            content: "Seeded so Stage 8 has observable work after the commit.".to_string(),
            facts: vec![],
            related_symbols: symbols.iter().map(|s| s.to_string()).collect(),
            related_files: vec![],
            tags: vec![],
            saga_id: None,
            embedding: vec![0.1_f32; DIM],
        })
        .await
        .expect("insert staleness-probe note")
}

/// Count nodes with `label` (and `repo_name = repo`) in Neo4j.
async fn neo4j_node_count(neo4j: &Neo4jPool, label: &str, repo: &str) -> i64 {
    let rows = neo4j
        .query(
            neo4rs::query(&format!(
                "MATCH (n:{label} {{repo_name: $r}}) RETURN count(n) AS c"
            ))
            .param("r", repo),
        )
        .await
        .expect("count neo4j nodes");
    rows[0].get("c").expect("count value")
}

/// Count PG rows in `table` for `repo` (table name is a hard-coded literal).
async fn pg_row_count(pg: &sqlx::PgPool, table: &str, repo: &str) -> i64 {
    let (c,): (i64,) = sqlx::query_as(&format!(
        "SELECT count(*) FROM {table} WHERE repo_name = $1"
    ))
    .bind(repo)
    .fetch_one(pg)
    .await
    .expect("count pg rows");
    c
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 1 — below-threshold completeness aborts with ZERO writes
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires live Postgres + Neo4j; run with --ignored"]
async fn ingest_aborts_with_zero_writes_when_a_file_fails_to_parse() {
    let (pg, neo4j, cfg) = connect_and_init_schema().await;
    clean_repo(&pg, &neo4j, REPO).await;

    // The default threshold (1.0) is what makes a single failed file abort.
    assert_eq!(
        cfg.ingest_completeness_threshold, 1.0,
        "this test assumes the default completeness threshold of 1.0"
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    write_valid_and_broken_fixture(tmp.path()).expect("write fixture");

    let pipeline = build_pipeline(&pg, &neo4j, cfg);
    let req = local_req(REPO, tmp.path());

    // Production path (`preserve_existing = false`): the commit gate would wipe
    // + re-commit on success. The guarantee under test is that a below-
    // threshold abort lands BEFORE that wipe, so nothing is ever written.
    let job_id = pipeline.start(req).await.expect("start ingestion");
    eprintln!("ram_first: below-threshold job = {job_id}");
    let (status, err) = poll_terminal(&pg, job_id).await;

    assert_eq!(
        status, "failed",
        "below-threshold ingest must fail at the commit gate; error = {err:?}"
    );
    let err = err.unwrap_or_default();
    eprintln!("ram_first: failure message = {err}");
    assert!(
        err.contains("completeness") && err.contains("threshold"),
        "the failure must be the ratio-gate abort, not something else; got: {err}"
    );
    assert!(
        err.contains("broken.rs"),
        "the unreadable file must appear in the failed-files list — proof it hit \
         the FailedFile path, not some unrelated failure; got: {err}"
    );

    // ── ZERO writes in Postgres ──
    for table in ["modules", "chunks", "large_chunks"] {
        let c = pg_row_count(&pg, table, REPO).await;
        assert_eq!(
            c, 0,
            "Postgres `{table}` must have ZERO rows for {REPO} after a below-threshold abort; \
             got {c}"
        );
    }

    // ── ZERO nodes in Neo4j ──
    for label in ["Module", "Chunk"] {
        let c = neo4j_node_count(&neo4j, label, REPO).await;
        assert_eq!(
            c, 0,
            "Neo4j `{label}` nodes must be ZERO for {REPO} after a below-threshold abort; got {c}"
        );
    }

    eprintln!("ram_first: below-threshold abort left ZERO writes across PG + Neo4j — PASSED");
    clean_repo(&pg, &neo4j, REPO).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 2 — full-success ingest matches the pre-RAM-first output shape, and
// Stages 8 + 9 still run after the commit
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires live Postgres + Neo4j; run with --ignored"]
async fn full_success_ingest_matches_pre_ram_first_output_shape() {
    let (pg, neo4j, cfg) = connect_and_init_schema().await;
    clean_repo(&pg, &neo4j, REPO_PARITY).await;

    // Seed a note referencing symbols the fixture DOES define. Stage 8's
    // divergence check needs those chunks to already exist (else its aggregate
    // query returns an undecodable NULL and it bails); so a stamped
    // `last_verified_at` after the run proves Stage 8 both ran AND saw the
    // committed chunks — i.e. it ran strictly AFTER the commit. Stage 8 reads
    // only PG `notes`/`chunks`; the commit gate's `clean_old_data` never
    // touches notes (verified), so the note survives to Stage 8.
    let note_id = seed_staleness_probe_note(&pg, REPO_PARITY, &["add", "double"]).await;

    let tmp = tempfile::tempdir().expect("tempdir");
    write_fixture(tmp.path()).expect("write fixture");

    let pipeline = build_pipeline(&pg, &neo4j, cfg);
    let req = local_req(REPO_PARITY, tmp.path());

    let job_id = pipeline.start(req).await.expect("start ingestion");
    eprintln!("ram_first: parity job = {job_id}");
    let (status, err) = poll_terminal(&pg, job_id).await;
    assert_eq!(
        status, "done",
        "full-success ingest must complete; error = {err:?}"
    );

    // ═══════════════════════════════════════════════════════════════════════
    // Parity baseline — the SAME facts `e2e_ingestion_against_live_dbs` asserts
    // for this exact fixture (cross-referenced from `e2e_test.rs`, not invented
    // here). If the RAM-first commit produced an equivalent graph, all hold.
    // ═══════════════════════════════════════════════════════════════════════
    let chunk_count = pg_row_count(&pg, "chunks", REPO_PARITY).await;
    eprintln!("ram_first: parity chunk count = {chunk_count}");
    assert!(chunk_count > 0, "expected > 0 chunks; got {chunk_count}");

    let names: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT name FROM chunks WHERE repo_name = $1 ORDER BY name")
            .bind(REPO_PARITY)
            .fetch_all(&pg)
            .await
            .expect("fetch chunk names");
    let names: Vec<String> = names.into_iter().map(|(n,)| n).collect();
    eprintln!("ram_first: parity chunk names = {names:?}");
    for expected in ["add", "double", "helper", "run", "main"] {
        assert!(
            names.iter().any(|n| n == expected),
            "expected a chunk named {expected:?} (same as the pre-RAM-first baseline); \
             found {names:?}"
        );
    }

    let module_count = pg_row_count(&pg, "modules", REPO_PARITY).await;
    eprintln!("ram_first: parity module count (PG) = {module_count}");
    assert!(module_count > 0, "expected > 0 modules; got {module_count}");

    let neo_module_count = neo4j_node_count(&neo4j, "Module", REPO_PARITY).await;
    eprintln!("ram_first: parity module count (Neo4j) = {neo_module_count}");
    assert!(
        neo_module_count > 0,
        "expected > 0 Module nodes in Neo4j; got {neo_module_count}"
    );

    let calls = neo4j
        .query(
            neo4rs::query(
                "MATCH (s:Chunk {repo_name: $r})-[c:CALLS]->(t:Chunk {repo_name: $r}) \
                 RETURN count(c) AS c",
            )
            .param("r", REPO_PARITY),
        )
        .await
        .expect("count CALLS edges");
    let call_count: i64 = calls[0].get("c").expect("calls count value");
    eprintln!("ram_first: parity CALLS edge count = {call_count}");
    assert!(
        call_count >= 1,
        "expected >= 1 CALLS edge (double->add and run->helper), same as the baseline; \
         got {call_count}"
    );

    // ═══════════════════════════════════════════════════════════════════════
    // Stage 9 ran AFTER the commit: community detection reads its nodes from
    // the just-committed Neo4j graph, so any Community node existing proves both
    // that the commit landed AND that Stage 9 ran on top of it.
    // ═══════════════════════════════════════════════════════════════════════
    let community_nodes = neo4j_node_count(&neo4j, "Community", REPO_PARITY).await;
    let pg_communities = pg_row_count(&pg, "communities", REPO_PARITY).await;
    eprintln!(
        "ram_first: parity communities — Neo4j nodes = {community_nodes}, PG rows = {pg_communities}"
    );
    assert!(
        community_nodes >= 1,
        "Stage 9 must produce at least one Community node in Neo4j post-commit; got \
         {community_nodes}"
    );

    // ═══════════════════════════════════════════════════════════════════════
    // Stage 8 ran AFTER the commit: the seeded note references committed chunks
    // ("add"/"double"), so its embedding-divergence check could only succeed —
    // and stamp `last_verified_at` via `update_staleness` — once those chunks
    // existed. A non-NULL `last_verified_at` therefore proves Stage 8 both ran
    // and observed the committed graph, i.e. it ran strictly post-commit.
    // ═══════════════════════════════════════════════════════════════════════
    let (verified, staleness_score, reasons): (bool, f64, Vec<String>) = sqlx::query_as(
        "SELECT (last_verified_at IS NOT NULL), staleness_score::float8, \
                coalesce(staleness_reasons, '{}') \
         FROM notes WHERE id = $1",
    )
    .bind(note_id)
    .fetch_one(&pg)
    .await
    .expect("read seeded note staleness");
    eprintln!(
        "ram_first: parity note last_verified_at set = {verified}, staleness_score = \
         {staleness_score}, reasons = {reasons:?}"
    );
    assert!(
        verified,
        "Stage 8 must have run post-commit and stamped last_verified_at on the seeded note \
         (its divergence check needs the committed add/double chunks); last_verified_at was NULL"
    );

    eprintln!(
        "ram_first: full-success parity + post-commit Stage 8/9 — PASSED \
         (chunks={chunk_count}, modules={module_count}, calls={call_count}, \
         communities={community_nodes})"
    );
    clean_repo(&pg, &neo4j, REPO_PARITY).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 3 — the production PG wipe+insert is ATOMIC (Roadmap F Task 9.5).
//
// The headline crash-consistency guarantee is that a hard crash mid-commit can
// never destroy the prior committed graph. The un-crash-testable residual is
// the post-PG-commit Neo4j replay window; but the PART that WAS the real hole —
// the pre-PG-commit window where the old graph is already wiped and the new one
// not yet landed — is now closed by making the wipe and the insert one Postgres
// transaction. This test proves exactly that: inject a failure PART-WAY through
// the insert (after the in-tx wipe has already deleted the repo's rows) and
// assert the original graph is still fully present — i.e. the wipe rolled back
// with the failed insert.
// ═══════════════════════════════════════════════════════════════════════════

const REPO_ATOMIC: &str = "e2e-ram-first-atomic-rollback";

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires live Postgres + Neo4j; run with --ignored"]
async fn import_repo_snapshot_replacing_rolls_back_wipe_on_mid_insert_failure() {
    use akashic_domain::ports::SnapshotPgRepo;
    use akashic_domain::types::{ModuleSnapshotRow, PgRepoSnapshot};
    use akashic_store_pg::repos::snapshot::PgSnapshotRepo;

    let (pg, neo4j, cfg) = connect_and_init_schema().await;
    clean_repo(&pg, &neo4j, REPO_ATOMIC).await;

    // ── Land a real graph via the PRODUCTION start() path ─────────────────
    let tmp = tempfile::tempdir().expect("tempdir");
    write_fixture(tmp.path()).expect("write fixture");
    let pipeline = build_pipeline(&pg, &neo4j, cfg);
    let req = local_req(REPO_ATOMIC, tmp.path());
    let job_id = pipeline.start(req).await.expect("start ingestion");
    let (status, err) = poll_terminal(&pg, job_id).await;
    assert_eq!(status, "done", "seed ingest must complete; error = {err:?}");

    let chunks_before = pg_row_count(&pg, "chunks", REPO_ATOMIC).await;
    let modules_before = pg_row_count(&pg, "modules", REPO_ATOMIC).await;
    assert!(
        chunks_before > 0 && modules_before > 0,
        "seed ingest must land a non-empty graph; chunks={chunks_before}, modules={modules_before}"
    );

    // ── Craft a snapshot whose insert fails PART-WAY through: two module rows
    // share one `id`, so the second INSERT violates the `modules` primary key —
    // but only AFTER the first module already inserted AND after the method's
    // in-transaction wipe already deleted every existing row for the repo. If
    // (and only if) that wipe+insert is one atomic transaction, this failure
    // rolls the wipe back and the original graph survives. ────────────────
    let dup_id = Uuid::new_v4();
    let module = |suffix: &str| ModuleSnapshotRow {
        id: dup_id,
        path: format!("src/atomic_probe_{suffix}.rs"),
        language: Some("rust".into()),
        summary: Some("atomicity probe module".into()),
        exports_count: Some(1),
        file_count: Some(1),
        is_virtual: false,
        embedding: None,
        git_ref: Some("main".into()),
        ingested_at: None,
    };
    let failing_snapshot = PgRepoSnapshot {
        modules: vec![module("a"), module("b")], // same id → PK violation on the 2nd
        chunks: vec![],
        large_chunks: vec![],
        communities: vec![],
        community_members: vec![],
    };

    let repo = PgSnapshotRepo::new(pg.clone());
    let result = repo
        .import_repo_snapshot_replacing(REPO_ATOMIC, &failing_snapshot)
        .await;

    assert!(
        result.is_err(),
        "the duplicate-id snapshot must make import_repo_snapshot_replacing fail mid-insert"
    );
    eprintln!(
        "ram_first: atomic-rollback injected failure = {}",
        result.unwrap_err()
    );

    // ── The ORIGINAL graph is still fully present: the wipe rolled back with
    // the failed insert. Direct proof a mid-commit failure no longer destroys
    // the prior committed graph. ──────────────────────────────────────────
    let chunks_after = pg_row_count(&pg, "chunks", REPO_ATOMIC).await;
    let modules_after = pg_row_count(&pg, "modules", REPO_ATOMIC).await;
    assert_eq!(
        chunks_after, chunks_before,
        "original chunks must survive a rolled-back replacing-import (wipe+insert is atomic); \
         before={chunks_before}, after={chunks_after}"
    );
    assert_eq!(
        modules_after, modules_before,
        "original modules must survive a rolled-back replacing-import (wipe+insert is atomic); \
         before={modules_before}, after={modules_after}"
    );
    // The partially-inserted probe module must NOT have leaked either.
    let (leaked,): (i64,) = sqlx::query_as("SELECT count(*) FROM modules WHERE id = $1")
        .bind(dup_id)
        .fetch_one(&pg)
        .await
        .expect("count leaked probe module");
    assert_eq!(
        leaked, 0,
        "the partially-inserted probe module must have been rolled back, not left behind"
    );

    eprintln!(
        "ram_first: PG wipe+insert atomicity — PASSED (chunks {chunks_before}->{chunks_after}, \
         modules {modules_before}->{modules_after} across a failed replacing-import)"
    );
    clean_repo(&pg, &neo4j, REPO_ATOMIC).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 4 — a zero-match note's staleness is persisted (Task 1's NULL-decode
// fix) AND does not abort the batch for the rest of the repo's notes
// (Task 2's per-note error isolation in `detect_staleness`).
//
// Unlike Test 2, this calls `detect_staleness` directly against a hand-seeded
// repo state instead of driving a full pipeline run — tighter and faster,
// and it isolates the staleness-detection behavior from ingestion entirely.
// ═══════════════════════════════════════════════════════════════════════════

const REPO_STALENESS_ISOLATION: &str = "e2e-ram-first-staleness-isolation";

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires live Postgres + Neo4j; run with --ignored"]
async fn e2e_detect_staleness_persists_symbol_deleted_for_zero_match_note() {
    use akashic_domain::ports::{NoteHealthRepo, NoteRepo};
    use akashic_store_pg::{PgNoteHealthRepo, PgNoteRepo};

    let (pg, neo4j, _cfg) = connect_and_init_schema().await;
    clean_repo(&pg, &neo4j, REPO_STALENESS_ISOLATION).await;

    // ── Seed one REAL chunk `alive_fn` with an embedding ──────────────────
    let chunk_emb = pgvector::Vector::from(vec![0.1_f32; DIM]);
    sqlx::query(
        "INSERT INTO chunks (id, repo_name, module_path, chunk_type, name, content, embedding) \
         VALUES ($1, $2, 'src/lib.rs', 'function', 'alive_fn', 'fn alive_fn() {}', $3)",
    )
    .bind(Uuid::new_v4())
    .bind(REPO_STALENESS_ISOLATION)
    .bind(chunk_emb)
    .execute(&pg)
    .await
    .expect("insert alive_fn chunk");

    // ── Seed two notes: one whose symbol exists, one whose symbol was
    // deleted (no matching chunk at all) — the exact zero-match precondition
    // Task 1 fixed the NULL-decode for. Both get a non-null embedding via
    // `seed_staleness_probe_note`, so `note_gone` genuinely exercises the
    // divergence-query zero-match path, not just the symbol-existence check. ─
    let note_alive_id =
        seed_staleness_probe_note(&pg, REPO_STALENESS_ISOLATION, &["alive_fn"]).await;
    let note_gone_id = seed_staleness_probe_note(&pg, REPO_STALENESS_ISOLATION, &["gone_fn"]).await;

    // ── Call detect_staleness directly (no pipeline) ──────────────────────
    let notes: Arc<dyn NoteRepo> = Arc::new(PgNoteRepo::new(pg.clone()));
    let note_health: Arc<dyn NoteHealthRepo> = Arc::new(PgNoteHealthRepo::new(pg.clone()));

    akashic_curation::notes::health::detect_staleness(
        &notes,
        &note_health,
        REPO_STALENESS_ISOLATION,
    )
    .await
    .expect("detect_staleness must not error on a zero-match note");

    // ── note_gone: staleness persisted, symbol_deleted reason, score > 0 ──
    let (gone_verified, gone_score, gone_reasons): (bool, f64, Vec<String>) = sqlx::query_as(
        "SELECT (last_verified_at IS NOT NULL), staleness_score::float8, \
                coalesce(staleness_reasons, '{}') \
         FROM notes WHERE id = $1",
    )
    .bind(note_gone_id)
    .fetch_one(&pg)
    .await
    .expect("read note_gone staleness");
    eprintln!(
        "ram_first: note_gone last_verified_at set = {gone_verified}, staleness_score = \
         {gone_score}, reasons = {gone_reasons:?}"
    );
    assert!(
        gone_verified,
        "note_gone must have last_verified_at set — the zero-match note's staleness must be \
         persisted, not abort the batch"
    );
    assert!(
        gone_score > 0.0,
        "note_gone must have a positive staleness_score (its only symbol was deleted); got \
         {gone_score}"
    );
    assert!(
        gone_reasons
            .iter()
            .any(|r| r.starts_with("symbol_deleted:")),
        "note_gone must carry a symbol_deleted: reason; got {gone_reasons:?}"
    );

    // ── note_alive: staleness also persisted — proves the batch reached and
    // persisted it too, i.e. the zero-match note did NOT abort the run. ───
    let (alive_verified,): (bool,) =
        sqlx::query_as("SELECT (last_verified_at IS NOT NULL) FROM notes WHERE id = $1")
            .bind(note_alive_id)
            .fetch_one(&pg)
            .await
            .expect("read note_alive staleness");
    eprintln!("ram_first: note_alive last_verified_at set = {alive_verified}");
    assert!(
        alive_verified,
        "note_alive must have last_verified_at set — the batch must reach and persist notes \
         after a zero-match note, proving per-note isolation"
    );

    eprintln!("ram_first: detect_staleness zero-match persistence + per-note isolation — PASSED");
    clean_repo(&pg, &neo4j, REPO_STALENESS_ISOLATION).await;
}
