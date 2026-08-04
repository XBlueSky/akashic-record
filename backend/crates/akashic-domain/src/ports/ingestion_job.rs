//! Port trait for ingestion-job persistence (PostgreSQL side).
//!
//! `IngestionJobRepo` is the infra-free contract for all reads and writes
//! against the `ingestion_jobs` table. Adapter implementations live in
//! `akashic-store-pg`.
//!
//! **No checkpoint methods** (Roadmap F, Task 8): `save_checkpoint` /
//! `load_checkpoint` and the `CheckpointData` type they carried were removed
//! entirely — RAM-first ingest crash-consistency means a failed run has
//! nothing durable to resume FROM, so there is no checkpoint state left to
//! read or write.

use async_trait::async_trait;
use uuid::Uuid;

/// Repository contract for the `ingestion_jobs` PG table.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn IngestionJobRepo>`.
#[async_trait]
pub trait IngestionJobRepo: Send + Sync {
    // ── Write (create) ────────────────────────────────────────────────────────

    /// Insert a fresh ingestion job record with status 'pending'.
    ///
    /// SQL moved verbatim from `akashic-ingestion::ingestion::store::create_job`
    /// (A1 Task 8).
    async fn create_job(&self, repo_name: &str, git_ref: &str) -> anyhow::Result<Uuid>;

    /// Insert a resumed ingestion job referencing the previous job.
    ///
    /// SQL moved verbatim from `akashic-ingestion::ingestion::pipeline::start_resume`
    /// (A1 Task 8).
    async fn create_job_resumed(
        &self,
        repo_name: &str,
        git_ref: &str,
        resumed_from: Uuid,
    ) -> anyhow::Result<Uuid>;

    // ── Write (update) ────────────────────────────────────────────────────────

    /// Update job status, processed_files, total_chunks, and error_message.
    ///
    /// Returns the current `total_files` value so the caller can include it in
    /// SSE events without a separate query.
    ///
    /// SQL moved verbatim from
    /// `akashic-ingestion::ingestion::store::update_job_status` (A1 Task 8).
    async fn update_job_status(
        &self,
        job_id: Uuid,
        status: &str,
        processed_files: Option<i32>,
        total_chunks: Option<i32>,
        error_message: Option<&str>,
    ) -> anyhow::Result<Option<i32>>;

    /// Set `total_files` on a job record.
    ///
    /// SQL moved verbatim from
    /// `akashic-ingestion::ingestion::store::set_total_files` (A1 Task 8).
    async fn set_total_files(&self, job_id: Uuid, total: i32) -> anyhow::Result<()>;

    // ── Read (A2a additions) ──────────────────────────────────────────────────

    /// Return `true` if there is at least one non-terminal ingestion job for
    /// the given repo (status NOT IN ('done', 'completed', 'failed')).
    ///
    /// SQL moved verbatim from the active-job guard in
    /// `akashic-server::api::routes::repos::delete_repo` and
    /// `akashic-server::api::routes::ingestion::trigger_ingest`.
    async fn has_active_job(&self, repo_name: &str) -> anyhow::Result<bool>;

    /// Return the most recent job row for a repo (latest `started_at`).
    ///
    /// Returns `None` if no job exists for the repo.
    ///
    /// Tuple layout: `(id, repo_name, git_ref, status, total_files,
    /// processed_files, total_chunks, error_message, started_at_str,
    /// completed_at_str)` — timestamps formatted as ISO-8601 strings.
    async fn latest_job(
        &self,
        repo_name: &str,
    ) -> anyhow::Result<
        Option<(
            Uuid,
            String,
            Option<String>,
            String,
            Option<i32>,
            Option<i32>,
            Option<i32>,
            Option<String>,
            Option<String>,
            Option<String>,
        )>,
    >;

    /// Return the `git_ref` from the most recent ingestion job for a repo.
    ///
    /// Used by `reingest` to recover the branch when the caller does not
    /// supply an explicit `?branch=` query parameter (finding #39).
    ///
    /// SQL: `SELECT git_ref FROM ingestion_jobs WHERE repo_name = $1
    ///       ORDER BY started_at DESC LIMIT 1`
    async fn get_last_job_git_ref(&self, repo_name: &str) -> anyhow::Result<Option<String>>;

    /// Find the most recent failed job that has a checkpoint (resumable job).
    ///
    /// Returns `(job_id, git_ref, source_type_from_sources)`.
    /// `source_type` is `None` when no row exists in `sources` for the repo.
    ///
    /// SQL moved verbatim from `akashic-server::api::routes::ingestion::resume_ingest`.
    ///
    /// (Roadmap F, Task 8): the underlying SQL's `checkpoint_data IS NOT
    /// NULL` filter is UNCHANGED here — out of this task's stated scope,
    /// which touched `IngestionPipeline::start_resume`'s internals, not this
    /// query. But since nothing writes `checkpoint_data` anymore (the
    /// `save_checkpoint` method that used to set it is deleted), this will
    /// only ever match a job left over from before this change; every job
    /// created from here on will never satisfy it, so `resume_ingest` will
    /// effectively always 404 for a freshly failed job. That is the intended
    /// end state — RAM-first crash-consistency means a failed run leaves
    /// nothing durable to resume FROM — not a bug this task introduces or
    /// needs to patch.
    async fn find_resumable_job(
        &self,
        repo_name: &str,
    ) -> anyhow::Result<Option<(Uuid, String, Option<String>)>>;

    /// Return all repos with an in-progress job (DISTINCT ON, paginated).
    ///
    /// SQL moved verbatim from `akashic-server::api::routes::ingestion::active_jobs`.
    ///
    /// Tuple layout: `(repo_name, status, processed_files, total_files)`.
    async fn list_active_jobs(
        &self,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<(String, String, Option<i32>, Option<i32>)>>;

    /// UPDATE all non-terminal jobs older than 10 minutes to `failed`.
    ///
    /// Returns the number of rows updated (for logging).
    ///
    /// SQL moved verbatim from main.rs startup sweep.
    async fn mark_stale_jobs_failed(&self) -> anyhow::Result<i64>;
}
