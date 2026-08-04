//! Port trait for the `sources` table (PostgreSQL).
//!
//! `SourceRepo` is the infra-free domain contract for all reads and writes
//! against the `sources` table. Adapter implementations live in
//! `akashic-store-pg`.
//!
//! **Domain boundary rules:**
//! - No `sqlx`, no infra types.
//! - `anyhow::Result` throughout.
//! - All traits are `#[async_trait]`, `dyn`-safe, and `Send + Sync`.

use async_trait::async_trait;

// ── Row types (domain-safe) ───────────────────────────────────────────────────

/// Source configuration row returned by `lookup_source_config`.
#[derive(Debug, Clone)]
pub struct SourceConfig {
    pub source_type: String,
    pub seed_url: Option<String>,
    pub crawl_depth: Option<i16>,
    pub url_pattern: Option<String>,
    pub git_url: Option<String>,
}

/// Source type row returned by `list_source_types`.
#[derive(Debug, Clone)]
pub struct SourceTypeRow {
    pub repo_name: String,
    pub source_type: String,
}

/// Full source row for the onboarding status machine (A2d-1 + A2d-2 probe fields).
#[derive(Debug, Clone)]
pub struct SourceRow {
    pub id: uuid::Uuid,
    pub repo_name: String,
    pub source_type: String,
    pub seed_url: Option<String>,
    pub crawl_depth: Option<i16>,
    pub url_pattern: Option<String>,
    pub status: String,
    pub submitter: Option<String>,
    pub adapter_id: Option<String>,
    pub classification: Option<String>,
    pub probe_note: Option<String>,
}

/// Inputs for an idempotent website-source upsert (A2d-2).
#[derive(Debug)]
pub struct WebsiteSourceUpsert<'a> {
    pub repo: &'a str,
    pub seed_url: &'a str,
    pub crawl_depth: Option<i16>,
    pub url_pattern: Option<String>,
    pub submitter: &'a str,
    pub status: &'a str,
    pub adapter_id: Option<&'a str>,
    pub classification: &'a str,
    pub probe_note: &'a str,
}

/// One aggregated demand-backlog entry (`status='unsupported'` grouped by host).
#[derive(Debug, Clone)]
pub struct DemandRow {
    pub host: String,
    pub count: i64,
    pub sample_note: Option<String>,
}

/// Per-source row returned by `sources_overview`.
///
/// Includes the latest-job status columns from a LATERAL join, plus per-repo
/// chunk/module/note counts as correlated subqueries.
#[derive(Debug, Clone)]
pub struct SourceOverviewRow {
    pub repo_name: String,
    pub source_type: String,
    /// The gated-onboarding lifecycle status from `sources.status`
    /// ('approved' | 'pending_review' | 'unsupported' | 'rejected'), distinct
    /// from `job_status` (the latest ingestion job's state).
    pub source_status: String,
    pub job_status: Option<String>,
    pub git_ref: Option<String>,
    pub processed_files: Option<i32>,
    pub total_files: Option<i32>,
    pub error_message: Option<String>,
    pub completed_at: Option<String>,
    pub can_resume: bool,
    pub chunk_count: i64,
    pub module_count: i64,
    pub note_count: i64,
}

// ── Port trait ────────────────────────────────────────────────────────────────

/// Repository contract for the `sources` PostgreSQL table.
///
/// Methods map 1-to-1 with the SQL patterns currently inline in
/// `akashic-server::api::routes::repos` and
/// `akashic-server::api::routes::ingestion`.
#[async_trait]
pub trait SourceRepo: Send + Sync {
    /// Return all `(repo_name, source_type)` pairs from the sources table.
    ///
    /// SQL: `SELECT repo_name, source_type FROM sources`
    async fn list_source_types(&self) -> anyhow::Result<Vec<SourceTypeRow>>;

    /// Look up the full source configuration for a single repo.
    ///
    /// SQL: `SELECT source_type, seed_url, crawl_depth, url_pattern, git_url
    ///       FROM sources WHERE repo_name = $1`
    async fn lookup_source_config(&self, repo: &str) -> anyhow::Result<Option<SourceConfig>>;

    /// Upsert a GitLab source record for a repo.
    ///
    /// SQL: `INSERT INTO sources (repo_name, source_type, git_url) VALUES ...
    ///       ON CONFLICT (repo_name) DO UPDATE SET source_type='gitlab', git_url=...`
    async fn upsert_source_gitlab(&self, repo: &str, git_url: &str) -> anyhow::Result<()>;

    /// Delete the source record for a repo.
    ///
    /// SQL: `DELETE FROM sources WHERE repo_name = $1`
    async fn delete_source(&self, repo: &str) -> anyhow::Result<()>;

    /// Return all sources with their latest job and per-repo counts.
    ///
    /// SQL moved verbatim from
    /// `akashic-server::api::routes::ingestion::sources_overview` (LATERAL join
    /// + correlated subquery counts for chunks/modules/notes).
    async fn sources_overview(&self) -> anyhow::Result<Vec<SourceOverviewRow>>;

    /// Upsert a website source at the given status with probe metadata. Returns
    /// the row id. ON CONFLICT (repo_name) re-queues (resets reviewer fields).
    async fn upsert_website_source(&self, u: WebsiteSourceUpsert<'_>)
    -> anyhow::Result<uuid::Uuid>;

    /// Unsupported submissions aggregated by host (the adapter-demand backlog).
    async fn list_demand(&self) -> anyhow::Result<Vec<DemandRow>>;

    /// Fetch a single source by id.
    async fn get_source(&self, id: uuid::Uuid) -> anyhow::Result<Option<SourceRow>>;

    /// Set status + reviewer (reviewed_at = now()). Returns false if no such row.
    async fn set_source_status(
        &self,
        id: uuid::Uuid,
        status: &str,
        reviewed_by: &str,
    ) -> anyhow::Result<bool>;

    /// All sources in `pending_review`, newest first.
    async fn list_pending_sources(&self) -> anyhow::Result<Vec<SourceRow>>;
}
