//! Port trait for docs-corpus version/file persistence (docs-kit ingestion,
//! C2).
//!
//! `CorpusStore` covers the PostgreSQL side (`corpus_versions` +
//! `corpus_files` tables, C1). It is infra-free: it uses only domain types
//! from `akashic_domain::types`. The adapter implementation
//! (`PgCorpusStore`) lives in `akashic-store-pg`.

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::{
    CorpusFileRow, CorpusManifest, CorpusRepoSummary, CorpusSource, CorpusVersionMeta,
    DeriveStatus, NavTree,
};

// ── NewCorpusVersion / NewCorpusFile — input bundle for insert_version ───────

/// Input bundle for [`CorpusStore::insert_version`].
///
/// `page_count`, `asset_count`, and `total_bytes` are NOT carried here — the
/// adapter derives them from `files` (count of markdown vs. non-markdown
/// entries, and total byte length) so callers can't pass values that
/// disagree with the actual file set.
#[derive(Debug, Clone)]
pub struct NewCorpusVersion {
    pub manifest: CorpusManifest,
    pub nav: NavTree,
    pub source: CorpusSource,
    pub is_tagged: bool,
    pub files: Vec<NewCorpusFile>,
}

/// A single file to persist as part of [`NewCorpusVersion`].
///
/// `content_hash` and `size_bytes` are NOT carried here — the adapter
/// computes both from `content` (SHA-256 hex digest, byte length) so they
/// can never drift from the actual bytes stored.
#[derive(Debug, Clone)]
pub struct NewCorpusFile {
    pub path: String,
    pub content: Vec<u8>,
    pub is_markdown: bool,
}

/// Outcome of [`CorpusStore::insert_version`].
///
/// `Existing` is the idempotent-replay path: re-submitting a corpus artifact
/// with the same `(repo_name, sha)` is not an error, it just returns the
/// already-persisted version.
#[derive(Debug, Clone)]
pub enum InsertOutcome {
    Inserted(CorpusVersionMeta),
    Existing(CorpusVersionMeta),
}

// ── CorpusStore ───────────────────────────────────────────────────────────────

/// Repository contract for the `corpus_versions` and `corpus_files` PG
/// tables.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn CorpusStore>`.
#[async_trait]
pub trait CorpusStore: Send + Sync {
    /// Insert a corpus version and its files in a single transaction.
    ///
    /// The previous `is_latest` row for the repo (if any) is flipped to
    /// `false` BEFORE the new row is inserted with `is_latest = true`, both
    /// inside the same transaction, so the partial unique index
    /// `corpus_latest_one` never observes two `true` rows for one repo.
    ///
    /// If `(repo_name, sha)` already exists (a `UNIQUE` violation on the
    /// insert), the transaction is rolled back and the existing row is
    /// fetched and returned as `InsertOutcome::Existing` — this is the
    /// idempotent-replay path, not an error.
    async fn insert_version(&self, v: NewCorpusVersion) -> anyhow::Result<InsertOutcome>;

    /// Resolve a version selector to its metadata.
    ///
    /// Resolution order: literal `"latest"` → exact `version` match → `sha`
    /// prefix match (selector must be at least 7 chars to be tried as a sha
    /// prefix). Returns `Ok(None)` if nothing matches at any stage.
    ///
    /// An exact `version` selector matching multiple rows (the same version
    /// string re-published at a different sha) deterministically resolves to
    /// the newest-ingested matching row (`ORDER BY ingested_at DESC LIMIT
    /// 1`), consistent with a resolved version being treated as
    /// immutable-cacheable downstream.
    ///
    /// A sha-prefix selector that matches multiple rows is not an error at
    /// this layer: it deterministically resolves to the earliest-ingested
    /// matching row (`ORDER BY ingested_at ASC LIMIT 1`). Surfacing the
    /// ambiguity itself (e.g. an HTTP 300) is left to the HTTP layer
    /// (Task 10), which can re-query `list_versions` and filter by prefix
    /// if it wants to detect and report the multi-match case.
    async fn resolve_version(
        &self,
        repo: &str,
        selector: &str,
    ) -> anyhow::Result<Option<CorpusVersionMeta>>;

    /// List all repos that have at least one ingested corpus version, each
    /// with its latest version and total version count.
    async fn list_repos(&self) -> anyhow::Result<Vec<CorpusRepoSummary>>;

    /// List all versions for a repo, newest-ingested first.
    async fn list_versions(&self, repo: &str) -> anyhow::Result<Vec<CorpusVersionMeta>>;

    /// Fetch a single version's metadata by id.
    ///
    /// Task 8's derive job (and its spawner) only ever receive a bare
    /// `version_id` — this is how they recover `repo_name` (needed for the
    /// corpus-scoped doc clean and for looking up the repo's code snapshot)
    /// and the corpus's own `sha` (for the B5 staleness note) without a
    /// second `(repo, selector)` round-trip.
    async fn get_version_by_id(
        &self,
        version_id: Uuid,
    ) -> anyhow::Result<Option<CorpusVersionMeta>>;

    /// Fetch a version's persisted nav tree (the `corpus_versions.nav`
    /// JSONB column — sidebar structure plus the index page's lede
    /// `description`).
    ///
    /// `CorpusVersionMeta` deliberately does not carry `nav` (it would bloat
    /// every list/resolve row with a JSON blob nobody but the nav endpoint
    /// needs) — this is the dedicated single-row read for it. `Ok(None)`
    /// when `version_id` doesn't exist.
    async fn get_nav(&self, version_id: Uuid) -> anyhow::Result<Option<NavTree>>;

    /// Fetch a single file by version + path.
    async fn get_file(&self, version_id: Uuid, path: &str)
    -> anyhow::Result<Option<CorpusFileRow>>;

    /// List the paths of all markdown files for a version.
    async fn list_md_paths(&self, version_id: Uuid) -> anyhow::Result<Vec<String>>;

    /// Update the derive lifecycle status (and optional error / job id) for
    /// a version.
    async fn set_derive_status(
        &self,
        version_id: Uuid,
        status: DeriveStatus,
        error: Option<&str>,
        job_id: Option<Uuid>,
    ) -> anyhow::Result<()>;

    /// Atomically claim the `running` derive status for a version — a CAS
    /// guard against two `run_derive` invocations for the SAME `version_id`
    /// executing concurrently (Task 8 review fix). Implementations MUST
    /// perform this as a single conditional `UPDATE ... WHERE derive_status
    /// IS DISTINCT FROM 'running'` (or equivalent), not a separate read then
    /// write, so two concurrent callers can't both observe "not running" and
    /// both proceed. Also clears any stale `derive_error` from a previous
    /// failed run.
    ///
    /// Returns `true` if THIS call won the claim (the version wasn't already
    /// `running`) — the caller owns the rebuild and must eventually
    /// transition to `Complete`/`Failed`. Returns `false` if another run is
    /// already in progress — the caller must bail out without touching the
    /// Doc space (retry-vs-retry, or retry-vs-a-fresh-publish's auto-derive,
    /// both target the same `version_id`'s "latest" version, so a
    /// per-version claim is sufficient to serialize them).
    async fn claim_derive_running(
        &self,
        version_id: Uuid,
        job_id: Option<Uuid>,
    ) -> anyhow::Result<bool>;
}
