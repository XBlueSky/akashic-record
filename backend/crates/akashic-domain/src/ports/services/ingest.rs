use super::*;

// ── IngestService (8 methods) ──────────────────────────────────────────────────

/// Use-case port for ingestion-pipeline operations.
///
/// Covers: triggering, re-ingesting, resuming, and monitoring ingestion jobs,
/// adding new sources, fetching a sources overview dashboard, and running
/// community detection as an isolated stage.
///
/// **A1 note:** methods whose orchestration still resides in `akashic-server`
/// handlers (idempotency reservation, raw SQL against `ingestion_jobs` /
/// `sources`) are stubbed with `anyhow::bail!` and labeled `(A2)`.
/// The trait is `#[allow(dead_code)]` at the impl level until A2 wires it.
#[async_trait]
pub trait IngestService: Send + Sync {
    /// Start a fresh ingestion job for a repo from source.
    ///
    /// Creates a job record and spawns the 9-stage pipeline in the background.
    /// Returns the new job UUID immediately.
    ///
    /// HTTP: `POST /api/v1/repos/:name/ingest`
    async fn trigger_ingest(
        &self,
        repo: String,
        git_ref: String,
        source: String,
        path: Option<String>,
        user_token: Option<String>,
    ) -> crate::DomainResult<uuid::Uuid>;

    /// Re-ingest a repo using its stored source configuration.
    ///
    /// Looks up `sources` table for stored source type / crawl config and
    /// the most recent job's `git_ref`, then calls `trigger_ingest`.
    ///
    /// HTTP: `POST /api/v1/repos/:name/reingest`
    async fn reingest(
        &self,
        repo: String,
        branch: Option<String>,
        user_token: Option<String>,
    ) -> crate::DomainResult<uuid::Uuid>;

    /// Resume the most recent failed job from its checkpoint.
    ///
    /// Finds the failed job with `checkpoint_data IS NOT NULL` and delegates
    /// to `IngestionPipeline::start_resume`.
    ///
    /// HTTP: `POST /api/v1/repos/:name/resume`
    async fn resume_ingest(
        &self,
        repo: String,
        user_token: Option<String>,
    ) -> crate::DomainResult<uuid::Uuid>;

    /// Fetch the latest job status for a repo.
    ///
    /// HTTP: `GET /api/v1/repos/:name/ingest/status`
    async fn ingest_status(&self, repo: String) -> crate::DomainResult<IngestStatus>;

    /// List all repos whose ingestion job is currently in-progress.
    ///
    /// HTTP: `GET /api/v1/jobs/active`
    async fn active_jobs(&self, limit: i64, offset: i64)
    -> crate::DomainResult<Vec<ActiveJobItem>>;

    /// Register a new website source for admin review (gated onboarding).
    ///
    /// Derives `repo_name` from the URL (host + first path segment), persists
    /// a `pending_review` record to `sources`, and does NOT start the crawl.
    /// A separate `approve_source` call is required to start ingestion.
    ///
    /// HTTP: `POST /api/v1/sources/add`
    async fn add_source(
        &self,
        source_type: String,
        url: String,
        crawl_depth: Option<u8>,
        url_pattern: Option<String>,
        submitter: String,
    ) -> crate::DomainResult<AddSourceResult>;

    /// Admin approves a pending source: mark approved, start the crawl.
    async fn approve_source(
        &self,
        id: uuid::Uuid,
        reviewer: String,
    ) -> crate::DomainResult<AddSourceResult>;

    /// Admin rejects a pending source: mark rejected, no crawl.
    async fn reject_source(&self, id: uuid::Uuid, reviewer: String) -> crate::DomainResult<()>;

    /// Sources awaiting review.
    async fn list_pending_sources(&self) -> crate::DomainResult<Vec<PendingSourceItem>>;

    /// Aggregated demand backlog: `unsupported` submissions grouped by host.
    async fn list_demand(&self) -> crate::DomainResult<Vec<DemandItem>>;

    /// Dashboard overview of all registered sources with counts and job status.
    ///
    /// HTTP: `GET /api/v1/sources/overview`
    async fn sources_overview(&self) -> crate::DomainResult<SourcesOverview>;

    /// Run community detection (Leiden / union-find + Neo4j storage) for a repo.
    ///
    /// Normally Stage 9 of the pipeline; also callable as a standalone stage
    /// for incremental re-clustering without re-ingesting the codebase.
    async fn detect_communities(&self, repo_name: String) -> crate::DomainResult<()>;

    /// Resolve the effective `(source_type, from_db)` for a repo: look up the
    /// `sources` table, else infer from the repo name (`'/'` → gitlab, else
    /// website). `from_db = false` marks the inferred fallback so callers that
    /// require a stored source (resume) can reject it.
    ///
    /// Used handler-side so the per-repo GitLab authz check can run BEFORE the
    /// idempotency reserve.
    async fn resolve_source_type(&self, repo: String) -> crate::DomainResult<(String, bool)>;

    /// Re-cluster a repo's document sections (Leiden over EXPLAINS-linked
    /// sections + LLM-named topics). Returns the cluster count. Runs after
    /// `SearchService::relink_explains` re-links the cross-repo edges.
    ///
    /// HTTP: `POST /api/v1/repos/:name/relink-explains` (clustering half)
    async fn recluster_doc_sections(&self, repo: String) -> crate::DomainResult<usize>;

    // ── Idempotency guard (handler-orchestrated; SQL in the adapter) ──────────
    //
    // The reserve → active-job-check → spawn → finalize/release ordering is
    // owned by the handler (Ground Rule 3); these methods only move the
    // `sagas`-table SQL behind the port. The atomic INSERT-on-conflict that
    // closes the double-ingest TOCTOU lives in the adapter.

    /// Fast-path idempotency lookup by key (no reservation).
    async fn idempotency_check(&self, key: String) -> crate::DomainResult<IdempotencyCheck>;

    /// Atomically reserve `key` (INSERT-on-conflict) before non-idempotent work.
    async fn idempotency_reserve(
        &self,
        key: String,
        saga_type: String,
        repo: String,
    ) -> crate::DomainResult<IdempotencyReservation>;

    /// Mark a reserved key `completed` and cache its response.
    async fn idempotency_finalize(
        &self,
        key: String,
        result: serde_json::Value,
    ) -> crate::DomainResult<()>;

    /// Best-effort release of a `running` reservation after a failed start.
    async fn idempotency_release(&self, key: String);
}

/// Outcome of [`IngestService::idempotency_check`].
#[derive(Debug, Clone)]
pub enum IdempotencyCheck {
    /// A prior request with this key completed — return its cached response.
    Completed(serde_json::Value),
    /// A request with this key is in flight (caller maps to HTTP 409).
    Running,
    /// No usable row (none, or a stale `failed` row that was cleaned up).
    Fresh,
}

/// Outcome of [`IngestService::idempotency_reserve`].
#[derive(Debug, Clone)]
pub enum IdempotencyReservation {
    /// This request won the key and MUST proceed, then finalize/release.
    Reserved,
    /// A prior request with this key already completed — return its response.
    Cached(serde_json::Value),
    /// Another request owns the key right now (caller maps to HTTP 409).
    InProgress,
}

// ── IngestService domain response types ───────────────────────────────────────

/// Latest job status for a repo (returned by `IngestService::ingest_status`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct IngestStatus {
    pub job_id: String,
    pub repo_name: String,
    pub git_ref: Option<String>,
    pub status: String,
    pub total_files: Option<i32>,
    pub processed_files: Option<i32>,
    pub total_chunks: Option<i32>,
    pub error_message: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// A single in-progress job (returned by `IngestService::active_jobs`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActiveJobItem {
    pub repo_name: String,
    pub status: String,
    pub processed_files: Option<i32>,
    pub total_files: Option<i32>,
}

/// Result of `IngestService::add_source`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AddSourceResult {
    pub job_id: String,
    pub repo_name: String,
    pub status: String,
}

/// A source awaiting admin review (returned by `IngestService::list_pending_sources`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingSourceItem {
    pub id: String,
    pub repo_name: String,
    pub seed_url: Option<String>,
    pub submitter: Option<String>,
    pub adapter_id: Option<String>,
    pub classification: Option<String>,
    pub probe_note: Option<String>,
}

/// One aggregated demand-backlog entry (returned by `IngestService::list_demand`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DemandItem {
    pub host: String,
    pub count: i64,
    pub sample_note: Option<String>,
}

/// A single source entry in the dashboard overview.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SourceOverviewItem {
    pub name: String,
    pub source_type: String,
    pub status: String,
    pub branch: Option<String>,
    pub chunk_count: i64,
    pub module_count: i64,
    pub note_count: i64,
    pub section_count: i64,
    pub last_synced_at: Option<String>,
    pub active_job: Option<ActiveJobItem>,
    pub last_error: Option<String>,
    pub can_resume: bool,
}

/// Dashboard overview response (returned by `IngestService::sources_overview`).
#[derive(Debug, serde::Serialize)]
pub struct SourcesOverview {
    pub sources: Vec<SourceOverviewItem>,
    pub total: usize,
    pub healthy: usize,
    pub stale: usize,
    pub failed: usize,
    pub ingesting: usize,
}
