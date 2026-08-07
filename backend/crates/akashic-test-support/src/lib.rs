//! Test-only `AppState` / PG-pool builders, extracted from `akashic-server`
//! (A2b) so the carved interface crates (`akashic-mcp` / `akashic-http`) can
//! share them without a dependency cycle back to `akashic-server`.

use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;
use tokio::sync::Semaphore;

use akashic_config::{
    AiProvider, AlertsConfig, Config, EmbeddingConfig, EmbeddingPrecision, LlmConfig,
};
use akashic_context::AppState;
use akashic_embed::EmbeddingProvider;
use akashic_identity::store::AuthStore;
use akashic_llm::LlmProvider;
use akashic_store_neo4j::Neo4jPool;

// ── No-op stubs for embedder / LLM (tests that call device_authorization
//    never touch these; we just need them to exist in AppState) ──────────

struct NoOpEmbedder;

#[async_trait]
impl EmbeddingProvider for NoOpEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<akashic_embed::EmbeddingResponse> {
        Ok(akashic_embed::EmbeddingResponse {
            vector: vec![0.0; 384],
            tokens_used: 0,
            model: "noop".into(),
        })
    }
    fn dimensions(&self) -> usize {
        384
    }
}

struct NoOpLlm;

#[async_trait]
impl LlmProvider for NoOpLlm {
    async fn generate_json(&self, _prompt: &str) -> anyhow::Result<akashic_llm::LlmResponse> {
        Ok(akashic_llm::LlmResponse {
            text: "{}".into(),
            usage: akashic_llm::LlmUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
            model: "noop".into(),
        })
    }
}

/// Standalone PostgreSQL pool for `#[cfg(test)]` setup/teardown SQL.
///
/// `AppState` no longer carries a `pg` pool (A2a), so integration tests that
/// seed / clean rows acquire their own pool here. Same DATABASE_URL fallback
/// as [`build_app_state`].
pub async fn test_pg_pool() -> sqlx::PgPool {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        format!(
            "postgres://{}:{}@localhost:5433/akashic",
            "akashic", "akashic_secret"
        )
    });
    sqlx::PgPool::connect(&url).await.expect("PgPool::connect")
}

/// Build a real `AppState` against the live Postgres + Neo4j for integration tests.
/// `gitlab_url` lets each test override the upstream (for wiremock / stubs).
pub async fn build_app_state(gitlab_url: String) -> AppState {
    // DATABASE_URL fallback: config.rs tests call clear_prod_env() which removes
    // this env var globally. The hardcoded default matches the dev compose setup
    // and is only used in #[cfg(test)] code.
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        format!(
            "postgres://{}:{}@localhost:5433/akashic",
            "akashic", "akashic_secret"
        )
    });
    let pg = sqlx::PgPool::connect(&url).await.expect("PgPool::connect");
    // Bootstrap auth schema for CI (idempotent — safe to call on an existing DB).
    // We only create the auth-specific tables; the full init_schema (which migrates
    // vector columns) is NOT called here to avoid destructive dim-migration on
    // dev databases that already have a different embedding width.
    akashic_store_pg::init_auth_schema(&pg)
        .await
        .expect("init_auth_schema");
    // Same reasoning for the docs-corpus tables: narrow, idempotent, no vector
    // columns touched.
    akashic_store_pg::init_corpus_schema(&pg)
        .await
        .expect("init_corpus_schema");

    let mut cfg = test_config_minimal();
    cfg.gitlab_url = gitlab_url;

    // Neo4j — use the real local instance (bolt://localhost:7687).
    // If it's not reachable, the test will fail with a connection error,
    // which is intentional — these are integration tests.
    let neo4j_url = std::env::var("NEO4J_URI").unwrap_or_else(|_| "bolt://localhost:7687".into());
    let neo4j_user = std::env::var("NEO4J_USER").unwrap_or_else(|_| "neo4j".into());
    let neo4j_password =
        std::env::var("NEO4J_PASSWORD").unwrap_or_else(|_| "akashic_secret".into());

    // Override config for Neo4j connection
    cfg.neo4j_uri = neo4j_url.clone();
    cfg.neo4j_user = neo4j_user.clone();
    cfg.neo4j_password = SecretString::from(neo4j_password.clone());

    let db = Neo4jPool::connect(&cfg).await.expect("Neo4jPool::connect");

    let auth_store = Arc::new(AuthStore::new(pg.clone(), 60));
    let gitlab_gateway = Arc::new(akashic_gitlab::ReqwestGitLabGateway::new(
        reqwest::Client::new(),
        Arc::new(cfg.clone()),
    ));
    let ingest_semaphore = Arc::new(Semaphore::new(1));
    let event_tx = tokio::sync::broadcast::channel::<akashic_kernel::AppEvent>(256).0;

    // A2a Task 0: build service impls for the 7 new AppState fields.
    // Integration tests don't call these; we use the A1 impls (which bail on
    // unimplemented methods) so AppState is structurally complete.
    use akashic_curation::services::CurationServices;
    use akashic_identity::services::IdentityServices;
    use akashic_ingestion::ingestion::corpus::{CorpusIngestService, NoopCorpusDeriveSpawner};
    use akashic_ingestion::ingestion::pipeline::IngestionPipeline;
    use akashic_ingestion::services::IngestionServices;
    use akashic_retrieval::services::RetrievalServices;
    use akashic_store_neo4j::{
        Neo4jCommunityGraphRepo, Neo4jEdgeRepo, Neo4jGraphReadRepo, Neo4jGraphTraversalRepo,
        Neo4jNoteGraphRepo, Neo4jRepoGraphRepo,
    };
    use akashic_store_pg::repos::PgCorpusStore;
    use akashic_store_pg::{
        PgAccountAuditRepo, PgAdminGrantRepo, PgChunkRepo, PgCommunityRepo, PgDeviceFlowRepo,
        PgDocClusterRepo, PgDocumentRepo, PgIngestionJobRepo, PgModuleRepo, PgNoteHealthRepo,
        PgNoteRepo, PgSagaExecutorRepo, PgSagaRepo, PgSourceRepo, PgSymbolRepo,
    };

    let embedder_arc: Arc<dyn EmbeddingProvider> = Arc::new(NoOpEmbedder);

    let retrieval_svc = Arc::new(RetrievalServices::new(
        Arc::new(PgChunkRepo::new(pg.clone())),
        Arc::new(PgDocumentRepo::new(pg.clone())),
        Arc::new(PgNoteRepo::new(pg.clone())),
        Arc::new(PgNoteHealthRepo::new(pg.clone())),
        Arc::new(PgModuleRepo::new(pg.clone())),
        Arc::new(Neo4jGraphTraversalRepo::new(db.clone())),
        Arc::new(PgSymbolRepo::new(pg.clone())),
        Arc::new(Neo4jEdgeRepo::new(db.clone())),
        Arc::new(PgSagaRepo::new(pg.clone())),
        Arc::new(PgDocClusterRepo::new(pg.clone())),
        Arc::new(Neo4jGraphReadRepo::new(db.clone())),
        Arc::new(akashic_store_neo4j::Neo4jGraphWriteRepo::new(db.clone())),
        Arc::new(PgCommunityRepo::new(pg.clone())),
        embedder_arc.clone(),
        Arc::new(NoOpLlm),
    ));
    let ingestion_pipeline = IngestionPipeline::new(
        pg.clone(),
        db.clone(),
        embedder_arc.clone(),
        Arc::new(NoOpLlm),
        cfg.clone(),
        ingest_semaphore.clone(),
        event_tx.clone(),
    );
    let ingestion_svc = Arc::new(IngestionServices::new(
        ingestion_pipeline,
        Arc::new(PgCommunityRepo::new(pg.clone())),
        Arc::new(Neo4jCommunityGraphRepo::new(db.clone())),
        // A2a Task 8: RepoService repos
        Arc::new(Neo4jGraphReadRepo::new(db.clone())),
        Arc::new(PgSourceRepo::new(pg.clone())),
        Arc::new(PgIngestionJobRepo::new(pg.clone())),
        Arc::new(Neo4jRepoGraphRepo::new(db.clone())),
        Arc::new(PgSagaExecutorRepo::new(pg.clone())),
        pg.clone(),
        gitlab_gateway.clone(),
        cfg.gitlab_url.clone(),
    ));
    let curation_svc = Arc::new(CurationServices::new(
        Arc::new(PgNoteRepo::new(pg.clone())),
        Arc::new(PgNoteHealthRepo::new(pg.clone())),
        Arc::new(Neo4jNoteGraphRepo::new(db.clone())),
        embedder_arc.clone(),
        Arc::new(PgSagaRepo::new(pg.clone())),
        Arc::new(PgSagaExecutorRepo::new(pg.clone())),
    ));
    let identity_svc = Arc::new(IdentityServices::new(
        auth_store.clone(),
        Arc::new(cfg.clone()),
        gitlab_gateway.clone(),
        Arc::new(PgDeviceFlowRepo::new(pg.clone())),
        Arc::new(PgAccountAuditRepo::new(pg.clone())),
    ));
    let corpus_store_for_state: Arc<dyn akashic_domain::ports::corpus::CorpusStore> =
        Arc::new(PgCorpusStore::new(pg.clone()));
    let corpus_ingest_service = Arc::new(CorpusIngestService::new(
        corpus_store_for_state.clone(),
        Arc::new(NoopCorpusDeriveSpawner),
    ));

    AppState {
        embedder: embedder_arc.clone(),
        llm: Arc::new(NoOpLlm),
        config: cfg,
        auth_store,
        ingest_semaphore,
        event_tx,
        oauth_health_cache: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
        shutdown: tokio_util::sync::CancellationToken::new(),
        metrics_handle: test_metrics_handle(),
        // Task 2: no standalone "mcp" probe — MCP is a branch of this same
        // router now, not a separately-probed process/port.
        readiness: akashic_platform::readiness::new_state(&["postgres", "neo4j", "embedding"]),
        raw_embedder: Arc::new(NoOpEmbedder),
        // A2a Task 0: service ports
        search_service: retrieval_svc.clone(),
        navigation_service: retrieval_svc.clone(),
        graph_service: retrieval_svc,
        ingest_service: ingestion_svc.clone(),
        repo_service: ingestion_svc,
        curation_service: curation_svc,
        auth_service: identity_svc,
        admin_grant_repo: Arc::new(PgAdminGrantRepo::new(pg.clone())),
        corpus_ingest_service,
        corpus_store: corpus_store_for_state,
    }
}

/// C5: shared Prometheus recorder for tests. Installed once per test
/// process via OnceLock so multiple build_app_state() calls reuse it
/// (the recorder install is global and would panic on double-install).
fn test_metrics_handle() -> metrics_exporter_prometheus::PrometheusHandle {
    use std::sync::OnceLock;
    static TEST_HANDLE: OnceLock<metrics_exporter_prometheus::PrometheusHandle> = OnceLock::new();
    TEST_HANDLE
        .get_or_init(|| {
            metrics_exporter_prometheus::PrometheusBuilder::new()
                .install_recorder()
                .expect("install test recorder")
        })
        .clone()
}

/// Minimal `Config` for tests — mirrors the fixture in `config.rs`'s tests.
/// All required fields populated; overrideable per-test via field assignment.
pub fn test_config_minimal() -> Config {
    Config {
        neo4j_uri: "bolt://localhost:7687".into(),
        neo4j_user: "neo4j".into(),
        neo4j_password: SecretString::from("akashic_secret".to_string()),
        database_url: std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            format!(
                "postgres://{}:{}@localhost:5433/akashic",
                "akashic", "akashic_secret"
            )
        }),
        embedding: EmbeddingConfig {
            provider: AiProvider::Local,
            api_key: None,
            model: "text-embedding-3-small".into(),
            base_url: None,
        },
        llm: LlmConfig {
            provider: AiProvider::Local,
            api_key: None,
            model: "gpt-5-nano".into(),
            base_url: None,
        },
        alerts: AlertsConfig::default(),
        module_max_files: 12,
        module_min_files: 3,
        api_host: "0.0.0.0".into(),
        gitlab_webhook_secret: None,
        gitlab_url: "http://unused".into(),
        gitlab_app_id: "test-app-id".into(),
        gitlab_app_secret: SecretString::from("test-app-secret".to_string()),
        gitlab_redirect_uri: "http://localhost:8081/auth/callback".into(),
        gitlab_web_redirect_uri: "http://localhost:8081/auth/web/callback".into(),
        auth_code_ttl_secs: 300,
        api_key_ttl_secs: 86400,
        gitlab_service_token: None,
        frontend_url: "http://localhost:3000".into(),
        public_base_url: "http://localhost:8081".into(),
        cors_extra_origins: vec![],
        cookie_secure: false,
        api_port: 8081,
        ingest_clone_dir: "/tmp/akashic-ingest".into(),
        ingest_max_file_size: 102_400,
        ingest_max_lines: 2000,
        ingest_chunk_max_size: 5120,
        ingest_concurrent_jobs: 1,
        ingest_skip_patterns: vec![],
        ingest_presets_path: None,
        ingest_completeness_threshold: 1.0,
        admin_users: vec![],
        ingest_crawl_max_pages: 100,
        ingest_crawl_delay_ms: 200,
        embedding_precision: EmbeddingPrecision::Float32,
        rate_limit_enabled: false,
        rate_limit_trusted_proxies: vec![],
        rate_limit_allowlist: vec![],
        mcp_quota_tokens_per_window: 100_000,
        mcp_quota_window_secs: 3600,
        mcp_quota_enabled: true,
        mcp_passthrough_user_cache_ttl_secs: 60,
        oauth_validation_mode: "warn".into(),
        migrate_on_boot: "auto".into(),
        ingest_quota_tokens_per_window: 5_000_000,
        ingest_quota_window_secs: 3600,
        ingest_quota_enabled: false,
    }
}
