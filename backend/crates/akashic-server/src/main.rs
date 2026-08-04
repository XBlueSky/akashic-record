use akashic_record::*;

use akashic_domain::ports::corpus::CorpusStore;
use akashic_domain::ports::gitlab::GitLabGateway;
use akashic_domain::ports::{IngestionJobRepo, SagaExecutorRepo};
use akashic_gitlab::ReqwestGitLabGateway;
use akashic_retrieval::graphrag;
use std::sync::Arc;
use std::time::Duration;

// A2a Task 0: repo adapters + service constructors for the 7 Arc<dyn Service> fields.
use akashic_curation::services::CurationServices;
use akashic_identity::services::IdentityServices;
use akashic_ingestion::ingestion::corpus::CorpusIngestService;
use akashic_ingestion::ingestion::corpus_derive::RealCorpusDeriveSpawner;
use akashic_ingestion::ingestion::doc_store::DocStore;
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

#[derive(clap::Parser)]
#[command(name = "akashic-record", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Validate OAuth runtime configuration and exit (0 on Ok, 1 on Warn or Fail).
    CheckOauth,
    /// Schema migration commands.
    Migrate {
        #[command(subcommand)]
        action: MigrateSub,
    },
}

#[derive(clap::Subcommand, Clone, Debug)]
enum MigrateSub {
    /// Apply schema (idempotent CREATE IF NOT EXISTS).
    Up {
        /// Also run destructive operations (vector-column reshape on dim change).
        #[arg(long)]
        allow_destructive: bool,
    },
    /// Read-only check that schema matches binary expectations.
    Verify,
    /// Not supported — see spec §7.
    Down,
}

async fn run_check_oauth() -> anyhow::Result<()> {
    let cfg = Config::from_env()?;
    let gateway = ReqwestGitLabGateway::new(reqwest::Client::new(), Arc::new(cfg));
    let report = gateway.runtime_report().await;
    akashic_gitlab::print_human_report(&report);
    let exit_code = if report.overall == akashic_domain::types::CheckStatus::Ok {
        0
    } else {
        1
    };
    std::process::exit(exit_code);
}

async fn run_migrate_subcommand(sub: MigrateSub) -> anyhow::Result<()> {
    // Subscriber must be installed before any tracing emit.
    observability::install_subscriber();
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let action = match sub {
        MigrateSub::Up { allow_destructive } => migrate::MigrateAction::Up { allow_destructive },
        MigrateSub::Verify => migrate::MigrateAction::Verify,
        MigrateSub::Down => migrate::MigrateAction::Down,
    };
    match migrate::run(action, &cfg).await {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::error!(event = "migrate_subcommand_failed", action = ?action, error = %e);
            // Verify failures get exit 76 (EX_CONFIG); everything else
            // (Up failures, Down which is always an error) gets 70.
            let code = if matches!(action, migrate::MigrateAction::Verify) {
                76
            } else {
                70
            };
            std::process::exit(code);
        }
    }
}

use anyhow::Result;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tracing::info;

use akashic_config::Config;
use akashic_embed::EmbeddingProvider;
use akashic_llm::LlmProvider;
use akashic_record::api::events;
use akashic_record::auth::AuthStore;
use akashic_store_neo4j::Neo4jPool;

#[tokio::main]
async fn main() -> Result<()> {
    // C5: JSON tracing subscriber — flatten event fields, RUST_LOG-controlled.
    observability::install_subscriber();

    // CLI dispatch — early-exit subcommands run before any DB/Neo4j/embedder setup.
    use clap::Parser;
    let cli = Cli::parse();
    match cli.command {
        Some(Command::CheckOauth) => return run_check_oauth().await,
        Some(Command::Migrate { action }) => return run_migrate_subcommand(action).await,
        None => {} // fall through to server boot (existing flow below).
    }

    // C2: install SIGTERM/SIGINT handler. The returned `shutdown` carries a
    // CancellationToken that gets propagated to axum's graceful_shutdown,
    // the MCP proxy, providers, and background tasks. SIGTERM/SIGINT cancel
    // the token; subsystems cooperate via the token to drain cleanly.
    let shutdown = shutdown::install_signal_handler();

    // C5: install Prometheus recorder. The handle is rendered at
    // /api/v1/metrics and stored on AppState so emit sites can use it.
    let metrics_handle = observability::install_metrics_recorder()?;

    // C5: build info gauge — set once at startup, queryable for "which build"
    // is currently running. Commit hash comes from build.rs via
    // option_env!('AKASHIC_BUILD_COMMIT').
    metrics::gauge!(
        "akashic_build_info",
        "version" => env!("CARGO_PKG_VERSION"),
        "commit" => option_env!("AKASHIC_BUILD_COMMIT").unwrap_or("unknown"),
    )
    .set(1.0);

    // Config — typed error path so operators see the full violation list
    // at once and the process exits with status 2 (config error, distinct
    // from generic runtime errors).
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    info!(embedding_provider = ?cfg.embedding.provider, llm_provider = ?cfg.llm.provider, "Configuration loaded");

    // Validate every tree-sitter grammar at startup. A bad/incompatible grammar
    // is a programming error, not a recoverable runtime condition — fail fast
    // here (panic) rather than on the first ingestion that touches the language.
    let langs: Vec<&'static akashic_extraction::LanguageConfig> =
        akashic_extraction::registry::tree_sitter_languages().collect();
    akashic_extraction::parser_pool::validate_languages(&langs);
    info!(grammars = langs.len(), "Tree-sitter grammars validated");

    // B5: startup-time OAuth runtime validation, gated by OAUTH_VALIDATION_MODE.
    {
        use akashic_domain::types::CheckStatus;
        use akashic_gitlab::ValidationMode;
        let mode = ValidationMode::from_env_str(&cfg.oauth_validation_mode);
        match mode {
            ValidationMode::Off => {
                tracing::info!("OAuth runtime validation: skipped (OAUTH_VALIDATION_MODE=off)");
            }
            ValidationMode::Warn | ValidationMode::Strict => {
                let probe_gateway =
                    ReqwestGitLabGateway::new(reqwest::Client::new(), Arc::new(cfg.clone()));
                let report = probe_gateway.runtime_report().await;
                for c in &report.checks {
                    match c.status {
                        CheckStatus::Ok => {
                            tracing::debug!(
                                event = "oauth_check_done",
                                name = c.name,
                                status = "ok",
                                elapsed_ms = c.elapsed_ms,
                            );
                        }
                        CheckStatus::Warn => {
                            tracing::warn!(
                                event = "oauth_check_done",
                                name = c.name,
                                status = "warn",
                                elapsed_ms = c.elapsed_ms,
                                detail = %c.detail,
                                remediation = c.remediation.as_deref().unwrap_or(""),
                            );
                        }
                        CheckStatus::Fail => {
                            tracing::warn!(
                                event = "oauth_check_done",
                                name = c.name,
                                status = "fail",
                                elapsed_ms = c.elapsed_ms,
                                detail = %c.detail,
                                remediation = c.remediation.as_deref().unwrap_or(""),
                            );
                        }
                    }
                }
                if matches!(report.overall, CheckStatus::Ok) {
                    tracing::info!(event = "oauth_runtime_ok", checks = report.checks.len());
                } else {
                    tracing::warn!(
                        event = "oauth_runtime_issues",
                        overall = ?report.overall,
                        n_warn = report.n_warn(),
                        n_fail = report.n_fail(),
                    );
                }
                if mode == ValidationMode::Strict && report.overall == CheckStatus::Fail {
                    tracing::error!(
                        event = "oauth_runtime_strict_exit",
                        n_fail = report.n_fail(),
                        "OAUTH_VALIDATION_MODE=strict and validation failed; exiting with code 70 (EX_SOFTWARE)",
                    );
                    std::process::exit(70);
                }
            }
        }
    }

    // Neo4j
    let db = Neo4jPool::connect(&cfg).await?;

    // PostgreSQL
    let pg = akashic_store_pg::connect(&cfg.database_url).await?;

    // Embedding provider (must init before schemas so we know vector dimensions)
    let raw_embedder: Arc<dyn EmbeddingProvider> =
        Arc::from(akashic_embed::build_provider(&cfg, shutdown.token().clone()).await?);
    info!(dims = raw_embedder.dimensions(), "Embedding provider ready");

    // C6: clone the raw provider for the readiness probe BEFORE it gets
    // moved into the QuotaEmbedding wrapper below.
    let raw_embedder_for_readiness: Arc<dyn EmbeddingProvider> = raw_embedder.clone();

    // Initialize query analyzer embedding archetypes
    graphrag::query_analyzer::init_archetypes(raw_embedder.as_ref()).await?;
    info!("Query analyzer archetypes initialized");

    // LLM provider
    let raw_llm: Arc<dyn LlmProvider> = Arc::from(akashic_llm::build_llm_provider(
        &cfg.llm,
        shutdown.token().clone(),
    )?);
    info!(provider = ?cfg.llm.provider, model = %cfg.llm.model, "LLM provider ready");

    // B3: build Quota over the injected QuotaRepo (the QuotaPort) and wrap the
    // raw providers with the quota-enforcing decorators from akashic-quota.
    let quota = Arc::new(akashic_quota::Quota::new(
        Arc::new(akashic_store_pg::repos::PgQuotaRepo::new(pg.clone())),
        cfg.mcp_quota_tokens_per_window,
        cfg.mcp_quota_window_secs,
        cfg.mcp_quota_enabled,
    ));
    let audit_repo = Arc::new(akashic_store_pg::PgAuditRepo::new(pg.clone()));
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(akashic_quota::QuotaEmbedding {
        inner: raw_embedder.clone(),
        quota: quota.clone(),
        audit: audit_repo.clone(),
    });
    let llm: Arc<dyn LlmProvider> = Arc::new(akashic_quota::QuotaLlm {
        inner: raw_llm,
        quota,
        audit: audit_repo.clone(),
    });

    // A2d-4: separate ingestion quota — generous default budget (5M tokens/hr),
    // enforcement off by default (track-only). The ingestion pipeline receives
    // THIS embedder (not the mcp one) so ingestion spend is attributed to the
    // synthetic system:ingestion actor (user_id=-1) via CURRENT_ACTOR.scope.
    let ingest_quota = Arc::new(akashic_quota::Quota::new(
        Arc::new(akashic_store_pg::repos::PgQuotaRepo::new(pg.clone())),
        cfg.ingest_quota_tokens_per_window,
        cfg.ingest_quota_window_secs,
        cfg.ingest_quota_enabled,
    ));
    let ingest_embedder: Arc<dyn EmbeddingProvider> = Arc::new(akashic_quota::QuotaEmbedding {
        inner: raw_embedder.clone(),
        quota: ingest_quota,
        audit: audit_repo,
    });

    // C3: schema mutation is decoupled from boot. Read MIGRATE_ON_BOOT
    // and dispatch via migrate::run. In production with default `auto`,
    // this is verify-only; in dev (is_production=false) it applies the
    // schema. migrate::run rebuilds its own PG/Neo4j connections — a
    // trivial duplicate of the work already done above; the alternative
    // of threading existing handles couples the API for marginal win.
    let boot_policy = migrate::MigrateOnBoot::from_env_str(&cfg.migrate_on_boot);
    let boot_action = boot_policy.resolve(cfg.is_production());
    if let Err(e) = migrate::run(boot_action, &cfg).await {
        tracing::error!(
            event = "migrate_boot_failed",
            action = ?boot_action,
            policy = ?boot_policy,
            error = %e,
        );
        let code = if matches!(boot_action, migrate::MigrateAction::Verify) {
            76
        } else {
            70
        };
        std::process::exit(code);
    }
    info!(event = "migrate_boot_complete", action = ?boot_action);

    // Log website sources that need re-ingestion for Doc Space migration
    let website_sources: Vec<(String,)> =
        sqlx::query_as("SELECT repo_name FROM sources WHERE source_type = 'website'")
            .fetch_all(&pg)
            .await?;

    if !website_sources.is_empty() {
        tracing::warn!(
            count = website_sources.len(),
            "Website sources need re-ingestion for Doc Space migration: {:?}",
            website_sources
                .iter()
                .map(|(n,)| n.as_str())
                .collect::<Vec<_>>()
        );
    }

    // Mark stale in-progress jobs as failed (backend may have restarted mid-ingestion).
    // SQL moved verbatim into IngestionJobRepo::mark_stale_jobs_failed (A2a Task 9).
    let job_repo = Arc::new(PgIngestionJobRepo::new(pg.clone())) as Arc<dyn IngestionJobRepo>;
    let stale_count = job_repo.mark_stale_jobs_failed().await?;
    if stale_count > 0 {
        tracing::warn!(count = stale_count, "Marked stale ingestion jobs as failed");
    }

    // Clean up stale sagas and expired idempotency keys
    let saga_exec_repo: Arc<dyn SagaExecutorRepo> = Arc::new(PgSagaExecutorRepo::new(pg.clone()));
    saga_exec_repo.cleanup_stale_sagas(30).await?;
    saga_exec_repo.cleanup_expired_idempotency(24).await?;

    // Auth store (in-memory pending artifacts + PostgreSQL-backed sessions)
    let auth_store = Arc::new(AuthStore::new(
        pg.clone(),
        cfg.mcp_passthrough_user_cache_ttl_secs,
    ));
    auth_store.spawn_cleanup_task(Duration::from_mins(1), shutdown.token().clone());
    info!("Auth store initialised, TTL cleanup running every 60s");

    // GitLab gateway — owns the one reqwest::Client (besides embed/llm providers).
    let gitlab_gateway: Arc<dyn GitLabGateway> = Arc::new(ReqwestGitLabGateway::new(
        reqwest::Client::new(),
        Arc::new(cfg.clone()),
    ));

    // Ingestion concurrency limiter
    let ingest_semaphore = Arc::new(Semaphore::new(cfg.ingest_concurrent_jobs));

    // SSE broadcast channel for real-time events
    let event_tx = events::create_channel();

    // C6: build readiness probes + state (poller spawned after AppState).
    let readiness_state = readiness::new_state(&["postgres", "neo4j", "mcp", "embedding"]);
    let probes: Vec<Arc<dyn readiness::Probe>> = vec![
        Arc::new(readiness::probes::PgProbe { pool: pg.clone() }),
        Arc::new(readiness::probes::Neo4jProbe { pool: db.clone() }),
        Arc::new(readiness::probes::McpLoopbackProbe {
            // Slice E: the MCP server now serves streamable-http directly on the
            // public port (no loopback). Probe that port.
            port: cfg.mcp_sse_port,
        }),
        Arc::new(readiness::probes::EmbeddingProbe::new(
            raw_embedder_for_readiness.clone(),
            // Throttle the (possibly billed) upstream embed call to once/min
            // even though the readiness poller ticks every 5s.
            Duration::from_mins(1),
        )),
    ];

    // ── A2a Task 0: construct A1 service impls + wire into AppState ──────────
    //
    // These service structs are HELD on AppState but NOT YET CALLED by any
    // handler — handlers still do inline SQL against state.pg / state.db.
    // Later A2a tasks will thin handlers one domain at a time.

    // RetrievalServices (search_service + navigation_service + graph_service)
    let retrieval_svc = Arc::new(RetrievalServices::new(
        Arc::new(PgChunkRepo::new(pg.clone())),
        Arc::new(PgDocumentRepo::new(pg.clone())),
        Arc::new(PgNoteRepo::new(pg.clone())),
        Arc::new(PgNoteHealthRepo::new(pg.clone())), // A2a-T4: note health for get_project_summary
        Arc::new(PgModuleRepo::new(pg.clone())),
        Arc::new(Neo4jGraphTraversalRepo::new(db.clone())),
        Arc::new(PgSymbolRepo::new(pg.clone())),
        Arc::new(Neo4jEdgeRepo::new(db.clone())), // A2a-T6: EXPLAINS edge writes for relink_explains
        Arc::new(PgSagaRepo::new(pg.clone())),    // A2a-T4: batch saga-status for get_module_graph
        Arc::new(PgDocClusterRepo::new(pg.clone())), // A2a-T4: cluster rows for get_doc_graph
        Arc::new(Neo4jGraphReadRepo::new(db.clone())), // A2a-T4: all Neo4j graph-read queries
        Arc::new(akashic_store_neo4j::Neo4jGraphWriteRepo::new(db.clone())), // C2-T4: global HTTP_CALLS writer
        Arc::new(PgCommunityRepo::new(pg.clone())), // A2a-T5: community reads for global_query
        embedder.clone(),
        llm.clone(), // A2a-T6: LLM-verified edge creation for relink_explains
    ));

    // Task 5/7/8/9 (B2/B3/B4, docs-corpus): corpus ingest service + real
    // derive-job spawner (replaces Task 5's NoopCorpusDeriveSpawner
    // placeholder). The derive job's own DocStore reuses
    // `ingest_embedder`/`llm` (so corpus-derive embedding spend shares the
    // system:ingestion quota budget website ingestion uses, not the MCP
    // one) and `job_repo` (already built above for the stale-job sweep) for
    // its EXPLAINS code_sha lookup. Built BEFORE `ingestion_pipeline` (moved
    // up from its original post-pipeline position) so Task 9's pull
    // bootstrap can wire it into the pipeline via
    // `with_corpus_ingest_service` below; `ingest_embedder` is cloned here
    // (not moved) since the pipeline constructor right after also needs it.
    let corpus_store: Arc<dyn CorpusStore> = Arc::new(PgCorpusStore::new(pg.clone()));
    // Task 10 (C3): AppState's own clone, held past `corpus_store` being
    // moved into `RealCorpusDeriveSpawner::new` below — see AppState's
    // `corpus_store` field doc comment.
    let corpus_store_for_state = corpus_store.clone();
    let corpus_doc_store = Arc::new(DocStore::new(
        pg.clone(),
        db.clone(),
        ingest_embedder.clone(),
        Some(llm.clone()),
    ));
    let corpus_ingest_service = Arc::new(CorpusIngestService::new(
        corpus_store.clone(),
        Arc::new(RealCorpusDeriveSpawner::new(
            corpus_store,
            corpus_doc_store,
            job_repo.clone(),
        )),
    ));

    // IngestionServices (ingest_service + repo_service)
    // A2d-4: pass `ingest_embedder` (quota-wrapped with the ingestion budget)
    // instead of the mcp `embedder` so ingestion token spend is attributed to
    // the system:ingestion actor and tracked (or capped) independently.
    // Task 9 (B4): `with_corpus_ingest_service` wires the pull-bootstrap
    // corpus step (`.akashic/docs.toml` detection) into the git/local
    // ingest branch, sharing the same `CorpusIngestService` the HTTP push
    // path (Task 7) and the derive retry endpoint (Task 8) use.
    let ingestion_pipeline = IngestionPipeline::new(
        pg.clone(),
        db.clone(),
        ingest_embedder.clone(),
        llm.clone(),
        cfg.clone(),
        ingest_semaphore.clone(),
        event_tx.clone(),
    )
    .with_corpus_ingest_service(corpus_ingest_service.clone());
    let ingestion_svc = Arc::new(IngestionServices::new(
        ingestion_pipeline,
        Arc::new(PgCommunityRepo::new(pg.clone())),
        Arc::new(Neo4jCommunityGraphRepo::new(db.clone())),
        // A2a Task 8: RepoService repos
        Arc::new(Neo4jGraphReadRepo::new(db.clone())),
        Arc::new(PgSourceRepo::new(pg.clone())),
        Arc::new(PgIngestionJobRepo::new(pg.clone())),
        Arc::new(Neo4jRepoGraphRepo::new(db.clone())),
        // A2a residual: idempotency-guard SQL behind the SagaExecutorRepo port.
        saga_exec_repo.clone(),
        pg.clone(),
        gitlab_gateway.clone(),
        cfg.gitlab_url.clone(),
    ));

    // CurationServices (curation_service) — A2a-T10: add NoteGraphRepo + embedder
    let curation_svc = Arc::new(CurationServices::new(
        Arc::new(PgNoteRepo::new(pg.clone())),
        Arc::new(PgNoteHealthRepo::new(pg.clone())),
        Arc::new(Neo4jNoteGraphRepo::new(db.clone())),
        embedder.clone(),
        Arc::new(PgSagaRepo::new(pg.clone())),
        Arc::new(PgSagaExecutorRepo::new(pg.clone())),
    ));

    // IdentityServices (auth_service) — A2a-T11: inject DeviceFlowRepo + AccountAuditRepo
    // Arc<Config> is needed by IdentityServices; clone cfg into an Arc.
    let identity_svc = Arc::new(IdentityServices::new(
        auth_store.clone(),
        Arc::new(cfg.clone()),
        gitlab_gateway.clone(),
        Arc::new(PgDeviceFlowRepo::new(pg.clone())),
        Arc::new(PgAccountAuditRepo::new(pg.clone())),
    ));

    // Shared state. The pg/db pools are intentionally NOT on AppState (A2a):
    // every repo adapter is pre-built above and injected into the services, and
    // the MCP proxy / readiness probes / migrate take the pools directly.
    let state = AppState {
        embedder: embedder.clone(),
        llm: llm.clone(),
        config: cfg.clone(),
        auth_store,
        ingest_semaphore,
        event_tx,
        oauth_health_cache: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
        shutdown: shutdown.token().clone(),
        metrics_handle: metrics_handle.clone(),
        readiness: readiness_state.clone(),
        raw_embedder: raw_embedder_for_readiness.clone(),
        // A2a Task 0: service ports (held, not yet called by handlers)
        search_service: retrieval_svc.clone(),
        navigation_service: retrieval_svc.clone(),
        graph_service: retrieval_svc,
        ingest_service: ingestion_svc.clone(),
        repo_service: ingestion_svc,
        curation_service: curation_svc,
        auth_service: identity_svc,
        // Admin trust-chain (delegated admin with cascade revocation).
        admin_grant_repo: Arc::new(PgAdminGrantRepo::new(pg.clone())),
        corpus_ingest_service,
        corpus_store: corpus_store_for_state,
    };

    // C6: start the 5s readiness poller. Per-probe timeout 5s. Exits on
    // shutdown.cancelled() (C2 idiom).
    readiness::spawn_poller(
        readiness_state.clone(),
        probes,
        Duration::from_secs(5),
        Duration::from_secs(5),
        shutdown.token().clone(),
    );

    // D6: alerting evaluator. Reads C5 metrics, evaluates 5 rules every
    // tick, dispatches matched alerts to the webhook (or LogSink fallback).
    let _alerts_handle =
        akashic_record::alerts::start_evaluator(state.clone(), shutdown.token().clone());

    // C2: when shutdown fires, broadcast a final SSE event so subscribers
    // can show a "restarting" toast and delay reconnect. Best-effort —
    // send Err on no subscribers is ignored.
    {
        let event_tx = state.event_tx.clone();
        let goodbye_token = shutdown.token().clone();
        tokio::spawn(async move {
            goodbye_token.cancelled().await;
            let _ = event_tx.send(akashic_record::api::events::AppEvent::ServerShuttingDown {
                reconnect_hint_secs: 30,
            });
        });
    }

    // 1. Start the MCP streamable-http server (Slice E) on the public MCP port.
    //    A single axum router nests rmcp's StreamableHttpService behind the
    //    rate-limit + mcp_auth (sets Authenticated ext + CURRENT_ACTOR quota
    //    scope) + trace layers — no loopback proxy.
    mcp::http::start(
        state.clone(),
        pg.clone(),
        db.clone(),
        shutdown.token().clone(),
    )
    .await?;

    // 2. Build axum router for REST API + GitLab webhook.
    //    D2 (2026-05-09): wiring moved to akashic_record::build_router so
    //    integration tests can serve the production router on an ephemeral
    //    port without duplicating route/middleware order.
    let app = akashic_record::build_router(state);

    // Bind the REST API server on its own configured port
    let api_addr = format!("{}:{}", cfg.mcp_sse_host, cfg.api_port);
    let listener = TcpListener::bind(&api_addr).await?;
    info!(%api_addr, "API server listening (REST + webhook)");
    // Bind with into_make_service_with_connect_info so the allowlist_layer's
    // ConnectInfo<SocketAddr> extractor receives the peer address (review
    // fix #5 / A6 plan Step 5.5).
    use std::net::SocketAddr;
    // C2: spawn axum::serve as its own task so main() can run the drain
    // coordinator around it. axum's graceful_shutdown drains in-flight HTTP
    // requests when the token cancels; the JoinHandle resolves once that's
    // done. We then bound the wait with shutdown::drain_with_cap so a stuck
    // request can't hang the whole shutdown.
    let api_shutdown = shutdown.token().clone();
    let api_handle = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move { api_shutdown.cancelled().await })
        .await;
    });

    // Block here until SIGTERM / SIGINT cancels the shutdown token.
    // Without this wait, drain_with_cap would start its 60s timer at
    // boot, killing the process at T+60s instead of T+60s-after-signal.
    // (C2 had a unit-test-only acceptance; AC-10 SIGTERM drill was
    // operator-pending — D3 surfaced the gap when the test-stack
    // backend kept exiting 60s after boot.)
    shutdown.cancelled().await;

    let cap = std::time::Duration::from_mins(1);
    let outcome = shutdown::drain_with_cap(cap, async {
        let _ = api_handle.await;
    })
    .await;

    // Close DB pools. PgPool::close awaits in-flight queries; Neo4jPool has
    // no explicit close — drop tears it down. `state` was moved into
    // `app.with_state(state)` earlier, so we close on the original `pg`/`db`
    // bindings here.
    pg.close().await;
    drop(db);

    tracing::info!(event = "shutdown_done", outcome = ?outcome);
    Ok(())
}
