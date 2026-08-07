//! D2 backend integration test bench.
//!
//! Provides `TestEnv::start()` — a single entry point that lazy-init's
//! shared Postgres + Neo4j containers (via testcontainers-modules),
//! resets DB state, runs migrations, builds the production axum router
//! with a deterministic `TestEmbedder` injected, serves to an ephemeral
//! port, and pre-populates one logged-in actor in the auth store.
//!
//! Tests must mark themselves `#[serial_test::serial]` because the
//! containers and the auth_store are shared. Without `#[serial]`, two
//! tests would race on `reset_state` and produce flakes.

#![allow(dead_code)] // each test crate uses a different subset of helpers

pub mod mcp_client;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use neo4rs::Graph;
use secrecy::SecretString;
use sqlx::PgPool;
use tokio::sync::OnceCell;
use uuid::Uuid;

use akashic_config::{
    AiProvider, AlertsConfig, Config, EmbeddingConfig, EmbeddingPrecision, LlmConfig,
};
use akashic_curation::services::CurationServices;
use akashic_embed::{EmbeddingProvider, EmbeddingResponse};
use akashic_identity::services::IdentityServices;
use akashic_ingestion::ingestion::corpus::{CorpusIngestService, NoopCorpusDeriveSpawner};
use akashic_ingestion::ingestion::pipeline::IngestionPipeline;
use akashic_ingestion::services::IngestionServices;
use akashic_llm::{LlmProvider, LlmResponse, LlmUsage};
use akashic_record::{
    AppState,
    auth::AuthStore,
    auth::types::UserInfo,
    build_router,
    migrate::{self, MigrateAction},
    readiness,
};
use akashic_retrieval::services::RetrievalServices;
use akashic_store_neo4j::{
    Neo4jCommunityGraphRepo, Neo4jEdgeRepo, Neo4jGraphReadRepo, Neo4jGraphTraversalRepo,
    Neo4jNoteGraphRepo, Neo4jPool, Neo4jRepoGraphRepo,
};
use akashic_store_pg::repos::PgCorpusStore;
use akashic_store_pg::{
    PgAccountAuditRepo, PgAdminGrantRepo, PgChunkRepo, PgCommunityRepo, PgDeviceFlowRepo,
    PgDocClusterRepo, PgDocumentRepo, PgIngestionJobRepo, PgModuleRepo, PgNoteHealthRepo,
    PgNoteRepo, PgSagaExecutorRepo, PgSagaRepo, PgSourceRepo, PgSymbolRepo,
};

use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::{
    neo4j::{Neo4j, Neo4jImage},
    postgres::Postgres,
};

// ──────────────────────────────────────────────────────────────────────────
// TestEmbedder — deterministic, EmbeddingProvider-shaped
// ──────────────────────────────────────────────────────────────────────────

/// Deterministic 1536-dim embedding provider.
///
/// Maps `text` → blake3 hash → 32 bytes → broadcast into 1536 f32 slots
/// each in `[-1.0, 1.0)`. Equal inputs produce equal vectors; distinct
/// inputs produce vectors that differ in at least one of the first 32
/// repeating positions. Sufficient for golden-path tests that need an
/// embedding to exist but don't care about geometric proximity.
pub struct TestEmbedder;

#[async_trait]
impl EmbeddingProvider for TestEmbedder {
    async fn embed(&self, text: &str) -> Result<EmbeddingResponse> {
        let hash = blake3::hash(text.as_bytes());
        let bytes = hash.as_bytes();
        let mut vector = vec![0.0_f32; 1536];
        for (i, b) in bytes.iter().enumerate() {
            vector[i % 1536] = (*b as f32 - 128.0) / 128.0;
        }
        Ok(EmbeddingResponse {
            vector,
            tokens_used: 0,
            model: "test-deterministic".to_string(),
        })
    }

    fn dimensions(&self) -> usize {
        1536
    }

    fn provider_label(&self) -> &'static str {
        "test"
    }
}

// ──────────────────────────────────────────────────────────────────────────
// TestLlm — no-op LLM provider (bench never exercises real LLM calls)
// ──────────────────────────────────────────────────────────────────────────

/// No-op LLM provider for the test bench. Every call returns the same
/// canned JSON string. Tests that need real LLM behavior should mock at
/// a higher level (wiremock against an OpenAI-shaped endpoint).
pub struct TestLlm;

#[async_trait]
impl LlmProvider for TestLlm {
    async fn generate_json(&self, _prompt: &str) -> Result<LlmResponse> {
        Ok(LlmResponse {
            text: "{}".to_string(),
            usage: LlmUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
            model: "test-llm".to_string(),
        })
    }

    fn provider_label(&self) -> &'static str {
        "test"
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Shared containers — OnceCell amortizes ~30s Neo4j cold-start
// ──────────────────────────────────────────────────────────────────────────

const PG_USER: &str = "akashic";
const PG_PASSWORD: &str = "test-password-XYZ";
const PG_DB: &str = "akashic_test";
const NEO_USER: &str = "neo4j";
const NEO_PASSWORD: &str = "test-password-XYZ";

static PG_CONTAINER: OnceCell<ContainerAsync<Postgres>> = OnceCell::const_new();
static NEO_CONTAINER: OnceCell<ContainerAsync<Neo4jImage>> = OnceCell::const_new();

async fn pg_container() -> &'static ContainerAsync<Postgres> {
    PG_CONTAINER
        .get_or_init(|| async {
            // Match docker-compose.yml's PG image (pgvector/pgvector:pg16)
            // so the test bench exercises the same `CREATE EXTENSION
            // vector` path as production. The vanilla `postgres:11-alpine`
            // default from testcontainers-modules has no pgvector and
            // would fail migrate::run.
            Postgres::default()
                .with_user(PG_USER)
                .with_password(PG_PASSWORD)
                .with_db_name(PG_DB)
                .with_name("pgvector/pgvector")
                .with_tag("pg16")
                .start()
                .await
                .expect("start Postgres test container — is the Docker daemon running?")
        })
        .await
}

async fn neo_container() -> &'static ContainerAsync<Neo4jImage> {
    NEO_CONTAINER
        .get_or_init(|| async {
            Neo4j::new()
                .with_user(NEO_USER)
                .with_password(NEO_PASSWORD)
                .start()
                .await
                .expect("start Neo4j test container — is the Docker daemon running?")
        })
        .await
}

/// Resolve (pg_url, neo4j_url, neo4j_user, neo4j_password). Honors env
/// vars when both `TEST_DATABASE_URL` and `TEST_NEO4J_URI` are set
/// (persistent-stack mode); otherwise spins testcontainers.
///
/// Persistent-stack mode is used by docker-compose.test.yml during D3
/// to avoid one PG+Neo4j pair per `cargo test` invocation. Each test's
/// `reset_state` call still TRUNCATEs between runs, so the same stack
/// safely serves many sequential test binaries.
async fn acquire_endpoints() -> (String, String, String, String) {
    match (
        std::env::var("TEST_DATABASE_URL"),
        std::env::var("TEST_NEO4J_URI"),
    ) {
        (Ok(pg), Ok(neo)) => {
            let neo_user = std::env::var("TEST_NEO4J_USER").unwrap_or_else(|_| "neo4j".to_string());
            let neo_password =
                std::env::var("TEST_NEO4J_PASSWORD").unwrap_or_else(|_| "akashic-test".to_string());
            (pg, neo, neo_user, neo_password)
        }
        _ => {
            let pg = pg_container().await;
            let neo = neo_container().await;
            let pg_host = pg.get_host().await.expect("pg host");
            let pg_port = pg.get_host_port_ipv4(5432).await.expect("pg port");
            let pg_url = format!("postgres://{PG_USER}:{PG_PASSWORD}@{pg_host}:{pg_port}/{PG_DB}");
            let neo_host = neo.get_host().await.expect("neo host");
            let neo_port = neo.image().bolt_port_ipv4().expect("neo bolt port");
            let neo4j_url = format!("bolt://{neo_host}:{neo_port}");
            (
                pg_url,
                neo4j_url,
                NEO_USER.to_string(),
                NEO_PASSWORD.to_string(),
            )
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// TestEnv — single bench entry point
// ──────────────────────────────────────────────────────────────────────────

/// The result of `TestEnv::start()`. Each integration test calls `start()`
/// to get a freshly-reset bench with the production router serving on an
/// ephemeral port.
///
/// On drop, the shutdown oneshot fires and the spawned serve task exits.
/// The containers themselves outlive the test (held by static OnceCells)
/// and are torn down at process exit.
pub struct TestEnv {
    pub pg_url: String,
    pub neo4j_url: String,
    pub neo4j_user: String,
    pub neo4j_pass: String,
    pub app_addr: std::net::SocketAddr,
    /// MCP base URL — `http://host:port` (no trailing slash). Task 2: MCP
    /// is a `/mcp` branch of the same app as `app_addr`, not a standalone
    /// listener, so in-process this is just `http://{app_addr}`. Honors
    /// `TEST_MCP_URL` env var to instead point at an external daemon (e.g.
    /// docker-compose.test.yml's backend-test container), which now serves
    /// MCP on its existing REST port mapping (`http://localhost:13001`) —
    /// there is no separate `13002` MCP mapping anymore.
    pub mcp_addr: String,
    /// Pre-populated session API key. Pass as `Authorization: Bearer <token>`
    /// or as the `ak_session` cookie value to hit protected routes.
    pub session_token: String,
    pub actor_user_id: String,
    pub state: AppState,
    /// Standalone Postgres pool for tests that run setup/teardown SQL.
    /// `AppState` no longer carries a `pg` field (A2a), so the bench keeps
    /// its own clone of the pool used to build the services.
    pg_pool: PgPool,
    /// Separate raw `Graph` handle. `AppState` no longer carries a `db`
    /// field (A2a), so this is the bench's direct handle for tests that
    /// just want a `&Graph` without going through `AppState`.
    neo4j_graph: Graph,
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

impl TestEnv {
    /// Spin up (or reuse) the shared PG + Neo4j containers, reset DB
    /// state to a clean schema, build the production router with
    /// TestEmbedder injected, serve on an ephemeral port, and
    /// pre-populate one logged-in actor.
    pub async fn start() -> Self {
        // 1. Acquire endpoints. If TEST_DATABASE_URL / TEST_NEO4J_URI are
        //    set, reuse a persistent stack (e.g. docker-compose.test.yml)
        //    and skip testcontainers entirely — saves ~30s cold-start per
        //    test binary and keeps host memory bounded across many
        //    sequential `cargo test` invocations. Falls back to
        //    testcontainers (D2's original behavior) when env vars unset.
        let (pg_url, neo4j_url, neo_user, neo_password) = acquire_endpoints().await;

        // 2. Build a Config wired to the resolved endpoints.
        let cfg = build_test_config(&pg_url, &neo4j_url, &neo_user, &neo_password);

        // 3. Migrate (idempotent — already-migrated containers are a no-op).
        migrate::run(
            MigrateAction::Up {
                allow_destructive: false,
            },
            &cfg,
        )
        .await
        .expect("migrate up against test containers");

        // 4. Open pools + reset state.
        let pg_pool = PgPool::connect(&pg_url).await.expect("pg pool");
        let neo_graph = Graph::new(&neo4j_url, &neo_user, &neo_password)
            .await
            .expect("neo4j graph");
        reset_state(&pg_pool, &neo_graph)
            .await
            .expect("reset state");

        // 5. Build AppState with TestEmbedder + TestLlm injected. Task 2:
        //    the MCP streamable-http router is no longer a standalone app —
        //    it's a branch (`build_mcp_branch`) merged into the SAME router
        //    `build_router` returns, on the SAME (ephemeral) port. The
        //    Neo4j pool it needs (AppState carries neither pg nor db — A2a)
        //    is opened here, before building either the branch or the
        //    router, so it's available regardless of whether `TEST_MCP_URL`
        //    later overrides where the test CLIENT connects.
        let state = build_test_app_state(cfg.clone(), pg_pool.clone()).await;
        let mcp_db = Neo4jPool::connect(&cfg).await.expect("mcp neo4j pool");
        let mcp_branch =
            akashic_record::mcp::http::build_mcp_branch(state.clone(), pg_pool.clone(), mcp_db);
        let router = build_router(state.clone(), mcp_branch);

        // 6. Serve on ephemeral port. We use `axum::serve(listener, router)`
        //    rather than `into_make_service_with_connect_info`. That leaves
        //    `ConnectInfo<SocketAddr>` unset, but it doesn't matter here:
        //    `build_test_config` sets `rate_limit_enabled = false`, so the
        //    rate-limit layer is never mounted on the test router.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let app_addr = listener.local_addr().expect("local addr");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .ok();
        });

        // Wait until the spawned server actually accepts connections, rather
        // than a fixed sleep that can lose the race on a loaded CI host.
        {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                match tokio::net::TcpStream::connect(app_addr).await {
                    Ok(_) => break,
                    Err(_) if std::time::Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(e) => panic!("test server did not start listening within 5s: {e}"),
                }
            }
        }

        // MCP base URL — Task 2: `build_mcp_branch` is now served by the SAME
        // app on the SAME `app_addr` bound above (it's merged into `router`
        // as a `/mcp` branch, not a standalone listener), so this is just
        // `app_addr` unless `TEST_MCP_URL` overrides to point at an external
        // daemon (e.g. docker-compose.test.yml's backend-test container —
        // see :13001 note on the `mcp_addr` field doc above).
        let mcp_addr =
            std::env::var("TEST_MCP_URL").unwrap_or_else(|_| format!("http://{app_addr}"));

        // 7. Pre-populate a logged-in actor.
        let token = Uuid::new_v4().to_string();
        let actor_user_id = "test-user-id".to_string();
        let user_info = UserInfo {
            username: "test-user".to_string(),
            name: Some("Test User".to_string()),
            avatar_url: None,
        };
        state
            .auth_store
            .insert_session(&token, &user_info, "test-gitlab-token", 3600, Some(42))
            .await
            .expect("insert test session");

        Self {
            pg_url,
            neo4j_url,
            neo4j_user: neo_user,
            neo4j_pass: neo_password,
            app_addr,
            mcp_addr,
            session_token: token,
            actor_user_id,
            state,
            pg_pool,
            neo4j_graph: neo_graph,
            _shutdown: shutdown_tx,
        }
    }

    pub fn pg_pool(&self) -> &PgPool {
        &self.pg_pool
    }

    pub fn neo4j(&self) -> &Graph {
        &self.neo4j_graph
    }

    /// Mint a device-flow `ak_*` MCP bearer token for the bench actor.
    ///
    /// Task 3 (spec §3): every `/mcp` request now requires authentication —
    /// there is no anonymous path left, not even for read tools — so every
    /// contract test that drives `McpClient` needs a real bearer, not just
    /// the write-tool tests that needed one before. `user_id: 42` mirrors
    /// the pre-populated session actor `start()` sets up above
    /// (`insert_session(..., Some(42))`), though the two token kinds
    /// (session cookie vs. MCP bearer) are otherwise unrelated.
    pub async fn mint_mcp_token(&self) -> String {
        let (_id, plaintext) = self
            .state
            .auth_store
            .issue_mcp_token(42, "test-user", Some("test"))
            .await
            .expect("issue mcp token");
        plaintext
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Helpers — Config + AppState construction
// ──────────────────────────────────────────────────────────────────────────

/// Build a `Config` wired to the test containers.
///
/// Path-(a) construction: enumerate every `Config` field with a
/// test-safe default. Chosen over the env-var path because (a) keeps
/// global env state out of the test process, (b) makes the test
/// independent of the caller's `.env`, and (c) compile-time enforces
/// "every new Config field needs a test default" — the file won't build
/// when `config.rs` adds a field.
///
/// The `embedding.provider = OpenAi` choice avoids the candle local
/// model download path inside `migrate::run`'s
/// `embedding::build_provider`. `OpenAiEmbedding::new` is a pure
/// constructor (no network call until `embed()`), and `dimensions()` for
/// `text-embedding-3-small` is 1536 — exactly what `TestEmbedder`
/// produces. Schema gets initialized at 1536; `TestEmbedder` then
/// produces same-shaped vectors when the router runs.
fn build_test_config(
    pg_url: &str,
    neo4j_url: &str,
    neo4j_user: &str,
    neo4j_password: &str,
) -> Config {
    Config {
        // ── Neo4j ─────────────────────────────────────────────────────
        neo4j_uri: neo4j_url.to_string(),
        neo4j_user: neo4j_user.to_string(),
        neo4j_password: SecretString::from(neo4j_password.to_string()),

        // ── PostgreSQL ────────────────────────────────────────────────
        database_url: pg_url.to_string(),

        // ── Embedding (see fn-doc above) ──────────────────────────────
        embedding: EmbeddingConfig {
            provider: AiProvider::OpenAi,
            api_key: Some(SecretString::from("test-embedding-key".to_string())),
            model: "text-embedding-3-small".to_string(),
            base_url: Some("http://127.0.0.1:1".to_string()), // never reached
        },

        // ── LLM (test bench wires TestLlm directly into AppState) ────
        llm: LlmConfig {
            provider: AiProvider::Local,
            api_key: None,
            model: "test-llm".to_string(),
            base_url: None,
        },

        // ── Alerts (D6) — defaults: LogSink mode, 60s tick, 5min cooldown
        alerts: AlertsConfig::default(),

        // ── Module grouping ───────────────────────────────────────────
        module_max_files: 12,
        module_min_files: 3,

        // ── API bind host (bench binds its own ephemeral port — see
        //    `api_port: 0` below; this value is unused by REST tests) ───
        api_host: "127.0.0.1".to_string(),

        // ── GitLab ────────────────────────────────────────────────────
        gitlab_webhook_secret: None,
        gitlab_url: "https://gitlab.test.example.org".to_string(),
        gitlab_app_id: "test-app-id".to_string(),
        gitlab_app_secret: SecretString::from("test-app-secret".to_string()),
        gitlab_redirect_uri: "http://127.0.0.1:0/auth/callback".to_string(),
        gitlab_web_redirect_uri: "http://127.0.0.1:0/auth/web/callback".to_string(),
        auth_code_ttl_secs: 300,
        api_key_ttl_secs: 3600,
        gitlab_service_token: None,

        // ── Frontend / public URL (CORS uses frontend_url) ────────────
        // Must parse as an HeaderValue origin. "http://localhost:0" is
        // valid even though port 0 won't bind anywhere.
        frontend_url: "http://localhost:0".to_string(),
        public_base_url: "http://127.0.0.1:0".to_string(),

        cors_extra_origins: vec![],
        cookie_secure: false,
        api_port: 0, // unused — bench binds ephemeral port itself

        // ── Ingestion ─────────────────────────────────────────────────
        ingest_clone_dir: "/tmp/akashic-test-ingest".to_string(),
        ingest_max_file_size: 102_400,
        ingest_max_lines: 2000,
        ingest_chunk_max_size: 5120,
        ingest_concurrent_jobs: 1,
        ingest_skip_patterns: vec!["node_modules".to_string(), ".git".to_string()],
        ingest_presets_path: None,
        ingest_completeness_threshold: 1.0,
        admin_users: vec![],
        ingest_crawl_max_pages: 10,
        ingest_crawl_delay_ms: 10,
        embedding_precision: EmbeddingPrecision::Float32,

        // ── Rate limit (disabled — bench targets functional behavior) ─
        rate_limit_enabled: false,
        rate_limit_trusted_proxies: vec![],
        rate_limit_allowlist: vec![],

        // ── MCP quota (disabled) ──────────────────────────────────────
        mcp_quota_tokens_per_window: 100_000,
        mcp_quota_window_secs: 3600,
        mcp_quota_enabled: false,

        mcp_passthrough_user_cache_ttl_secs: 60,

        // ── OAuth validation (off — no live GitLab) ──────────────────
        oauth_validation_mode: "off".to_string(),

        // ── Migration policy (resolved separately; we call run directly) ─
        migrate_on_boot: "false".to_string(),

        // ── Ingestion quota (A2d-4, track-only by default) ───────────
        ingest_quota_tokens_per_window: 5_000_000,
        ingest_quota_window_secs: 3600,
        ingest_quota_enabled: false,
    }
}

/// Construct an `AppState` with `TestEmbedder` swapped into both
/// `embedder` and `raw_embedder` slots, and `TestLlm` for the LLM. All
/// other fields use either the production constructors (AuthStore,
/// readiness state) or test-safe defaults.
async fn build_test_app_state(cfg: Config, pg_pool: PgPool) -> AppState {
    // Install a Prometheus recorder for this test bench. `install_recorder`
    // is process-global and cannot be called twice without erroring — every
    // `TestEnv::start()` after the first in a given test BINARY process used
    // to fall back to a freestanding, never-installed `PrometheusHandle`
    // whose `render()` is permanently empty (it never observes the actual
    // global recorder the running app's `metrics::counter!()` calls write
    // into). That was fine while "tests don't scrape metrics" held, but
    // Task 11 (C5) added integration tests that assert on
    // `state.metrics_handle.render()` — so, like
    // `akashic_test_support::test_metrics_handle()`, cache the FIRST
    // successful install in a process-global `OnceLock` and clone that
    // (real, connected) handle for every subsequent test in this process
    // instead of building a disconnected standalone one.
    let metrics_handle = {
        use std::sync::OnceLock;
        static HANDLE: OnceLock<metrics_exporter_prometheus::PrometheusHandle> = OnceLock::new();
        HANDLE
            .get_or_init(|| {
                metrics_exporter_prometheus::PrometheusBuilder::new()
                    .install_recorder()
                    .expect("install test recorder")
            })
            .clone()
    };

    // Wrap a fresh Neo4jPool around the test container. Neo4jPool's only
    // public ctor is `connect(&Config)`; the test-config drives it back
    // to the same container the outer `neo_graph` was opened against.
    // The duplicate bolt connection is the trade-off for not exposing a
    // `from_graph` public ctor in lib code.
    let db = Neo4jPool::connect(&cfg).await.expect("neo4j pool");

    let test_embedder: Arc<dyn EmbeddingProvider> = Arc::new(TestEmbedder);
    let test_llm: Arc<dyn LlmProvider> = Arc::new(TestLlm);
    let auth_store = Arc::new(AuthStore::new(pg_pool.clone(), 60));
    let http_client = reqwest::Client::new();
    let ingest_semaphore = Arc::new(tokio::sync::Semaphore::new(1));
    let event_tx = akashic_record::api::events::create_channel();

    // A2a Task 0 / T4: build service impls for AppState service fields.
    // Integration tests do not exercise GraphService methods directly; they
    // still use inline SQL. Repos wired with real adapters for structural completeness.
    let retrieval_svc = Arc::new(RetrievalServices::new(
        Arc::new(PgChunkRepo::new(pg_pool.clone())),
        Arc::new(PgDocumentRepo::new(pg_pool.clone())),
        Arc::new(PgNoteRepo::new(pg_pool.clone())),
        Arc::new(PgNoteHealthRepo::new(pg_pool.clone())),
        Arc::new(PgModuleRepo::new(pg_pool.clone())),
        Arc::new(Neo4jGraphTraversalRepo::new(db.clone())),
        Arc::new(PgSymbolRepo::new(pg_pool.clone())),
        Arc::new(Neo4jEdgeRepo::new(db.clone())), // A2a-T6: EXPLAINS edge writes for relink_explains
        Arc::new(PgSagaRepo::new(pg_pool.clone())),
        Arc::new(PgDocClusterRepo::new(pg_pool.clone())),
        Arc::new(Neo4jGraphReadRepo::new(db.clone())),
        Arc::new(akashic_store_neo4j::Neo4jGraphWriteRepo::new(db.clone())),
        Arc::new(PgCommunityRepo::new(pg_pool.clone())), // A2a-T5: community reads for global_query
        test_embedder.clone(),
        test_llm.clone(), // A2a-T6: LLM-verified edge creation for relink_explains
    ));
    let ingestion_pipeline = IngestionPipeline::new(
        pg_pool.clone(),
        db.clone(),
        test_embedder.clone(),
        test_llm.clone(),
        cfg.clone(),
        ingest_semaphore.clone(),
        event_tx.clone(),
    );
    let ingestion_svc = Arc::new(IngestionServices::new(
        ingestion_pipeline,
        Arc::new(PgCommunityRepo::new(pg_pool.clone())),
        Arc::new(Neo4jCommunityGraphRepo::new(db.clone())),
        // A2a Task 8: RepoService repos
        Arc::new(Neo4jGraphReadRepo::new(db.clone())),
        Arc::new(PgSourceRepo::new(pg_pool.clone())),
        Arc::new(PgIngestionJobRepo::new(pg_pool.clone())),
        Arc::new(Neo4jRepoGraphRepo::new(db.clone())),
        Arc::new(PgSagaExecutorRepo::new(pg_pool.clone())),
        pg_pool.clone(),
        Arc::new(akashic_gitlab::ReqwestGitLabGateway::new(
            http_client.clone(),
            Arc::new(cfg.clone()),
        )),
        cfg.gitlab_url.clone(),
    ));
    let curation_svc = Arc::new(CurationServices::new(
        Arc::new(PgNoteRepo::new(pg_pool.clone())),
        Arc::new(PgNoteHealthRepo::new(pg_pool.clone())),
        Arc::new(Neo4jNoteGraphRepo::new(db.clone())),
        test_embedder.clone(),
        Arc::new(PgSagaRepo::new(pg_pool.clone())),
        Arc::new(PgSagaExecutorRepo::new(pg_pool.clone())),
    ));
    let identity_svc = Arc::new(IdentityServices::new(
        auth_store.clone(),
        Arc::new(cfg.clone()),
        Arc::new(akashic_gitlab::ReqwestGitLabGateway::new(
            http_client.clone(),
            Arc::new(cfg.clone()),
        )),
        Arc::new(PgDeviceFlowRepo::new(pg_pool.clone())),
        Arc::new(PgAccountAuditRepo::new(pg_pool.clone())),
    ));

    let corpus_store_for_state: Arc<dyn akashic_domain::ports::corpus::CorpusStore> =
        Arc::new(PgCorpusStore::new(pg_pool.clone()));
    let corpus_ingest_service = Arc::new(CorpusIngestService::new(
        corpus_store_for_state.clone(),
        Arc::new(NoopCorpusDeriveSpawner),
    ));

    AppState {
        embedder: test_embedder.clone(),
        llm: test_llm,
        config: cfg,
        auth_store,
        ingest_semaphore,
        event_tx,
        oauth_health_cache: Arc::new(tokio::sync::RwLock::new(None)),
        shutdown: tokio_util::sync::CancellationToken::new(),
        metrics_handle,
        // Task 2: no standalone "mcp" probe — MCP is a branch of this same
        // router now, not a separately-probed process/port.
        readiness: readiness::new_state(&["postgres", "neo4j", "embedding"]),
        raw_embedder: test_embedder,
        // A2a Task 0: service ports
        search_service: retrieval_svc.clone(),
        navigation_service: retrieval_svc.clone(),
        graph_service: retrieval_svc,
        ingest_service: ingestion_svc.clone(),
        repo_service: ingestion_svc,
        curation_service: curation_svc,
        auth_service: identity_svc,
        // Admin trust-chain (delegated admin with cascade revocation).
        admin_grant_repo: Arc::new(PgAdminGrantRepo::new(pg_pool.clone())),
        corpus_ingest_service,
        corpus_store: corpus_store_for_state,
    }
}

/// Truncate every PostgreSQL table the production schema creates, then
/// `MATCH (n) DETACH DELETE n` the Neo4j graph.
///
/// Table list mirrors `db::pg::init_schema` + `db::pg::init_auth_schema`
/// CREATE TABLE statements (sourced via grep). If migrations add or
/// remove tables, this list MUST be updated in lockstep — a missing
/// table here means cross-test bleed; a stale table here means the
/// truncate fails with "relation does not exist".
pub async fn reset_state(pg: &PgPool, neo: &Graph) -> Result<()> {
    sqlx::query(
        "TRUNCATE \
            chunks, large_chunks, notes, modules, ingestion_jobs, sources, \
            documents, sections, doc_clusters, sagas, saga_steps, \
            communities, community_members, \
            sessions, mcp_tokens, device_flow_pending, \
            revoked_passthrough_tokens, audit_log, llm_usage, publish_tokens, \
            corpus_versions, corpus_files, \
            mcp_oauth_codes, mcp_oauth_clients \
         RESTART IDENTITY CASCADE",
    )
    .execute(pg)
    .await?;

    neo.run(neo4rs::query("MATCH (n) DETACH DELETE n")).await?;

    Ok(())
}
