//! `akashic-context` — shared application state crate (A2a Task 0).
//!
//! `AppState` was originally defined in `akashic-server::lib`. It is moved
//! here so it can be constructed independently of the Axum binary and
//! referenced by any crate in the workspace without creating a dependency on
//! the full server crate (which brings in every handler, tree-sitter grammars,
//! etc.).
//!
//! Holds the seven `Arc<dyn …Service>` application-service ports plus genuine
//! runtime handles (embedder, llm, config, auth_store, ingest_semaphore,
//! event_tx, oauth_health_cache, shutdown, metrics_handle, readiness,
//! raw_embedder).
//!
//! **Spec §4 end-state**: this struct carries NO infrastructure clients —
//! `pg` / `db` pools (A2a) and `http_client` (Phase2-GW) were removed. Handlers
//! go through the service ports; the pools live only at the composition root
//! (where repos/probes/migrate/proxy are built), and all GitLab HTTP goes
//! through `Arc<dyn GitLabGateway>` injected into the services.
//!
//! **No cycle**: this crate depends on domain / identity / service-impl crates
//! but NOT on `akashic-server`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{RwLock, Semaphore};

use akashic_config::Config;
use akashic_domain::ports::corpus::CorpusStore;
use akashic_domain::ports::services::{
    AuthService, CurationService, GraphService, IngestService, NavigationService, RepoService,
    SearchService,
};
use akashic_domain::types::ValidationReport;
use akashic_identity::store::AuthStore;
use akashic_ingestion::ingestion::corpus::CorpusIngestService;

// Re-export the embedding + LLM provider traits via their canonical domain
// paths. `akashic-server` currently imports these as `crate::embedding::EmbeddingProvider`
// (a local shim re-exporting from akashic-embed / akashic-domain).  AppState
// fields hold `Arc<dyn EmbeddingProvider>` — the trait object must be the same
// trait, so we import the one from akashic_domain (both akashic-embed and
// akashic-llm re-export these same domain types).
use akashic_domain::ports::AdminGrantRepo;
use akashic_domain::ports::EmbeddingProvider;
use akashic_domain::ports::LlmProvider;

// The SSE event type used by event_tx lives in akashic-kernel.
use akashic_kernel::AppEvent;

// ── ReadinessState ─────────────────────────────────────────────────────────────
//
// The readiness module lives in akashic-server (it also holds the Probe trait,
// polling logic, and probe implementations that depend on akashic-server types).
// Only the shared *state container* type needs to cross the crate boundary so
// AppState can carry it.  Duplicating the thin type alias here avoids pulling
// all of akashic-server into akashic-context (which would create a cycle).
//
// This must stay bit-for-bit identical to `akashic_server::readiness::ReadinessState`
// so both sides can use the same `Arc<…>` value.

/// The outcome of one readiness probe run.
///
/// Mirrors `akashic_server::readiness::ProbeOutcome` — kept in sync manually.
/// A single `Arc<RwLock<HashMap>>` is shared between AppState and the poller.
#[derive(Clone, Debug)]
pub struct ProbeOutcome {
    pub last_ok_at: Option<std::time::Instant>,
    pub last_check_at: std::time::Instant,
    pub last_error: Option<String>,
}

impl ProbeOutcome {
    pub fn never_checked() -> Self {
        Self {
            last_ok_at: None,
            last_check_at: std::time::Instant::now(),
            last_error: None,
        }
    }
}

/// Shared readiness state: a map from probe name to its latest outcome.
///
/// `Arc<RwLock<…>>` is cheap to clone and thread-safe.
pub type ReadinessState = Arc<RwLock<HashMap<&'static str, ProbeOutcome>>>;

/// Build a fresh [`ReadinessState`] pre-populated with `never_checked` entries
/// for each probe name.  Called once at startup.
pub fn new_readiness_state(probe_names: &[&'static str]) -> ReadinessState {
    let mut map = HashMap::new();
    for name in probe_names {
        map.insert(*name, ProbeOutcome::never_checked());
    }
    Arc::new(RwLock::new(map))
}

// ── AppState ───────────────────────────────────────────────────────────────────

/// Shared application state threaded through every Axum handler and MCP tool
/// via the `State` extractor.
///
/// Clone is cheap: all heavy resources are wrapped in `Arc`.
///
/// **Field layout:**
/// - Fields 1–14: original infrastructure handles (unchanged from A1).
/// - Fields 15–21: new `Arc<dyn …Service>` application-service ports (A2a
///   Task 0). Handlers do NOT call these yet; they are constructed in main.rs
///   and held here so later tasks can thin handlers one-by-one without
///   requiring another structural change.
#[derive(Clone)]
pub struct AppState {
    // ── infrastructure handles ────────────────────────────────────────────
    //
    // Spec §4 end-state: AppState carries only `Arc<dyn Service>` + genuine
    // runtime handles — NO `PgPool`, `Neo4jPool`, or `reqwest::Client`. The
    // `pg`/`db` pools (A2a) and `http_client` (Phase2-GW) were removed: every
    // repo adapter and the GitLab gateway are pre-built at the composition root
    // and injected into the services below, so no handler can reach the DB or
    // GitLab HTTP directly. The embed/llm providers own their own clients.
    // migrate / readiness / the MCP proxy take pools passed directly at startup.
    /// Quota-wrapped embedding provider (billed calls go through this).
    pub embedder: Arc<dyn EmbeddingProvider>,

    /// Quota-wrapped LLM provider.
    pub llm: Arc<dyn LlmProvider>,

    /// Typed application configuration.
    pub config: Config,

    /// Authentication store (sessions, MCP tokens, passthrough cache).
    pub auth_store: Arc<AuthStore>,

    /// Limits concurrent ingestion pipeline runs.
    pub ingest_semaphore: Arc<Semaphore>,

    /// Broadcast channel for real-time SSE events pushed to the frontend.
    pub event_tx: tokio::sync::broadcast::Sender<AppEvent>,

    /// B5: 60-second LRU cache for `/api/v1/health/oauth` responses.
    /// Tuple: `(cached_at, ValidationReport)`.
    pub oauth_health_cache: Arc<RwLock<Option<(std::time::Instant, ValidationReport)>>>,

    /// C2: top-level shutdown cancellation token. Handlers and background tasks
    /// call `.cancelled().await` to cooperate with SIGTERM-triggered drain.
    pub shutdown: tokio_util::sync::CancellationToken,

    /// C5: Prometheus rendering handle (cheap to clone — Arc inside).
    pub metrics_handle: metrics_exporter_prometheus::PrometheusHandle,

    /// C6: shared readiness state (probe outcomes keyed by probe name).
    pub readiness: ReadinessState,

    /// C6: raw (non-quota-wrapped) embedding provider for the readiness probe
    /// so health checks do not burn per-actor quota.
    pub raw_embedder: Arc<dyn EmbeddingProvider>,

    // ── A2a: application-service ports ────────────────────────────────────
    //
    // All seven fields are held but NOT yet called by any handler.  Later A2a
    // tasks will thin handlers one domain at a time.  The fields are `pub` on
    // a `pub` struct, so the compiler does not flag them as dead code.
    /// Unified search, GraphRAG, and EXPLAINS re-linking.
    pub search_service: Arc<dyn SearchService>,

    /// Call-graph traversal, goto-definition, find-refs, impact analysis.
    pub navigation_service: Arc<dyn NavigationService>,

    /// Graph topology reads (nodes/edges for D3, god-nodes, module graph, …).
    pub graph_service: Arc<dyn GraphService>,

    /// Ingestion pipeline orchestration (trigger, reingest, resume, status, …).
    pub ingest_service: Arc<dyn IngestService>,

    /// Repository metadata and lifecycle (list, delete, branches, …).
    pub repo_service: Arc<dyn RepoService>,

    /// Note CRUD, health analysis, saga lifecycle, saga-pattern orchestration.
    pub curation_service: Arc<dyn CurationService>,

    /// Authentication and identity operations (sessions, tokens, OAuth flows).
    pub auth_service: Arc<dyn AuthService>,

    /// Admin trust-chain: grant/revoke/reachability for delegated admins.
    pub admin_grant_repo: Arc<dyn AdminGrantRepo>,

    /// Task 5/7 (B2, docs-corpus): validates + persists an uploaded
    /// `corpus.tar.gz` (or an already-resolved file tree, pull path). Not a
    /// `dyn` port — `CorpusIngestService` is itself the seam (its store and
    /// derive-spawner are injected), so AppState just holds the concrete
    /// type directly, mirroring how `embedder`/`llm` hold trait objects but
    /// this one composition doesn't need a second layer of indirection.
    pub corpus_ingest_service: Arc<CorpusIngestService>,

    /// Task 10 (C3, docs-corpus): direct read access to the raw corpus
    /// store for the public `/api/v1/docs/*` read endpoints.
    /// `corpus_ingest_service` holds its own `Arc<dyn CorpusStore>`
    /// internally but doesn't expose it (its `store` field is private) — this
    /// is a second `Arc` clone of the SAME underlying store, wired at the
    /// composition root, so the read-only docs handlers don't need any of
    /// `CorpusIngestService`'s write-path (publish/derive) methods.
    pub corpus_store: Arc<dyn CorpusStore>,
}
