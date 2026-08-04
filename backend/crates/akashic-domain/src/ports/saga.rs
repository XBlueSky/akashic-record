//! Port traits for saga persistence (PostgreSQL side).
//!
//! `SagaRepo` covers the narrative-saga DAL used by the MCP tools, REST API,
//! and memory-stack building.  `SagaExecutorRepo` covers the low-level
//! saga-pattern orchestration table (`sagas`/`saga_steps`) used by
//! `SagaExecutor`.
//!
//! Both traits are infra-free: they use only domain types from
//! `akashic_domain::types`.  Adapter implementations live in
//! `akashic-store-pg`.
//!
//! (Roadmap F, Task 8): ingestion-job checkpoint methods (`save_checkpoint`/
//! `load_checkpoint`) that used to live on `IngestionJobRepo` are gone
//! entirely — RAM-first crash-consistency retired checkpoint/resume, so
//! there is no checkpoint state for any port to carry.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::ports::services::{IdempotencyCheck, IdempotencyReservation};
use crate::types::{SagaDbRow, SagaIdempotencyRecord, SagaTimelineEntry, SagaWithCount};

// ── SagaRepo ──────────────────────────────────────────────────────────────────

/// Repository contract for the narrative-saga DAL.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn SagaRepo>`.
#[async_trait]
pub trait SagaRepo: Send + Sync {
    // ── Low-level find helpers ────────────────────────────────────────────────

    /// Find a saga by (repo, source_type, source_ref).
    ///
    /// Returns `Some(id)` if found, `None` if not.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::saga::find_or_create_saga`
    /// (the `source_ref IS NOT NULL` branch).
    async fn find_by_source(
        &self,
        repo_name: &str,
        source_type: &str,
        source_ref: &str,
    ) -> anyhow::Result<Option<Uuid>>;

    /// Find a saga by (repo, source_type, name) where source_ref IS NULL.
    ///
    /// Used for manual-topic sagas that are keyed by name, not source_ref.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::saga::find_or_create_saga`
    /// (the `source_ref IS NULL` branch).
    async fn find_by_name(
        &self,
        repo_name: &str,
        source_type: &str,
        name: &str,
    ) -> anyhow::Result<Option<Uuid>>;

    /// Insert a new knowledge saga using ON CONFLICT DO NOTHING.
    ///
    /// Returns `Some(id)` when the row was inserted, `None` when another
    /// concurrent insert won the race (ON CONFLICT DO NOTHING → 0 rows).
    ///
    /// SQL moved verbatim from `akashic-curation::notes::saga::find_or_create_saga`.
    #[allow(clippy::too_many_arguments)] // verbatim DAL signature (saga + source identity)
    async fn create_saga(
        &self,
        saga_id: Uuid,
        saga_type: &str,
        repo_name: &str,
        status: &str,
        name: Option<&str>,
        source_type: &str,
        source_ref: Option<&str>,
    ) -> anyhow::Result<Option<Uuid>>;

    /// Fetch the winning saga id after an ON CONFLICT race on (repo, source_type, source_ref).
    ///
    /// SQL moved verbatim from `akashic-curation::notes::saga::find_or_create_saga`
    /// (the post-race fetch).
    async fn get_saga_by_source(
        &self,
        repo_name: &str,
        source_type: &str,
        source_ref: &str,
    ) -> anyhow::Result<Uuid>;

    // ── Structured reads ──────────────────────────────────────────────────────

    /// Fetch a knowledge saga by id.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::saga::get_saga_timeline`.
    async fn get_saga_by_id(&self, saga_id: Uuid) -> anyhow::Result<SagaDbRow>;

    /// List knowledge sagas for a repo with a status filter, including note counts.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::saga::list_sagas`
    /// (the status-filtered branch).
    async fn list_sagas_with_status_filter(
        &self,
        repo_name: &str,
        status: &str,
    ) -> anyhow::Result<Vec<SagaWithCount>>;

    /// List all knowledge sagas for a repo, including note counts.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::saga::list_sagas`
    /// (the unfiltered branch).
    async fn list_all_sagas(&self, repo_name: &str) -> anyhow::Result<Vec<SagaWithCount>>;

    /// Get the timeline entries (notes) for a saga ordered by COALESCE(valid_at, created_at) ASC.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::saga::get_saga_timeline`.
    async fn get_saga_timeline_entries(
        &self,
        saga_id: Uuid,
    ) -> anyhow::Result<Vec<SagaTimelineEntry>>;

    // ── Count reads ───────────────────────────────────────────────────────────

    /// Count sagas with status 'active' or 'open' in a repo.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::memory_stack::build_l0`.
    async fn count_active_sagas(&self, repo_name: &str) -> anyhow::Result<i64>;

    /// Count sagas with status 'resolved' in a repo.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::memory_stack::build_l0`.
    async fn count_resolved_sagas(&self, repo_name: &str) -> anyhow::Result<i64>;

    /// Fetch name and status for a single saga by id.
    ///
    /// SQL moved verbatim from `akashic-curation::notes::memory_stack::build_l2`.
    async fn get_saga_info(
        &self,
        saga_id: Uuid,
    ) -> anyhow::Result<Option<(Option<String>, String)>>;

    /// Batch-fetch status for a set of saga ids in a single query.
    ///
    /// Returns a map from saga_id → status. Used by GraphService::get_module_graph
    /// to batch-fetch all saga statuses after collecting the saga group rows,
    /// preserving the N+1 fix (one `WHERE id = ANY($1)` instead of per-group queries).
    async fn get_saga_statuses_batch(
        &self,
        saga_ids: &[Uuid],
    ) -> anyhow::Result<std::collections::HashMap<Uuid, String>>;
}

// ── SagaExecutorRepo ──────────────────────────────────────────────────────────

/// Repository contract for the low-level saga-pattern orchestration tables
/// (`sagas` / `saga_steps`).
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn SagaExecutorRepo>`.
#[async_trait]
pub trait SagaExecutorRepo: Send + Sync {
    /// Look up an existing saga by idempotency key.
    ///
    /// SQL moved verbatim from `akashic-curation::saga::mod::SagaExecutor::run`.
    async fn find_idempotent_saga(
        &self,
        idempotency_key: &str,
    ) -> anyhow::Result<Option<SagaIdempotencyRecord>>;

    /// Delete a saga row by id (used to clear a failed saga before retry).
    ///
    /// SQL moved verbatim from `akashic-curation::saga::mod::SagaExecutor::run`.
    async fn delete_saga(&self, saga_id: Uuid) -> anyhow::Result<()>;

    /// Insert a new saga row with status 'running'.
    ///
    /// SQL moved verbatim from `akashic-curation::saga::mod::SagaExecutor::run`.
    async fn insert_saga(
        &self,
        saga_id: Uuid,
        saga_type: &str,
        idempotency_key: Option<&str>,
        repo_name: &str,
        context_json: &JsonValue,
    ) -> anyhow::Result<()>;

    /// Insert a saga step row with status 'pending'.
    ///
    /// SQL moved verbatim from `akashic-curation::saga::mod::SagaExecutor::run`.
    async fn insert_saga_step(
        &self,
        step_id: Uuid,
        saga_id: Uuid,
        step_index: i16,
        step_name: &str,
    ) -> anyhow::Result<()>;

    /// Mark a saga as 'completed' and store the result JSON.
    ///
    /// SQL moved verbatim from `akashic-curation::saga::mod::SagaExecutor::run`.
    async fn update_saga_completion(
        &self,
        saga_id: Uuid,
        result_json: &JsonValue,
    ) -> anyhow::Result<()>;

    /// Update the saga status (e.g. 'compensating', 'failed').
    ///
    /// SQL moved verbatim from `akashic-curation::saga::mod::update_saga_status`.
    async fn update_saga_status(&self, saga_id: Uuid, status: &str) -> anyhow::Result<()>;

    /// Update a saga step status by step_index.
    ///
    /// SQL moved verbatim from `akashic-curation::saga::mod::update_step_status`.
    async fn update_step_status(
        &self,
        saga_id: Uuid,
        step_index: i16,
        status: &str,
    ) -> anyhow::Result<()>;

    /// Update a saga step status by step_name.
    ///
    /// SQL moved verbatim from `akashic-curation::saga::mod::update_step_status_by_name`.
    async fn update_step_status_by_name(
        &self,
        saga_id: Uuid,
        step_name: &str,
        status: &str,
    ) -> anyhow::Result<()>;

    /// Mark sagas stuck in 'running' for longer than `stale_minutes` as 'failed'.
    ///
    /// Returns the count of rows updated.
    ///
    /// SQL moved verbatim from `akashic-curation::saga::mod::cleanup_stale_sagas`.
    async fn cleanup_stale_sagas(&self, stale_minutes: i64) -> anyhow::Result<i64>;

    /// Delete completed sagas with idempotency keys older than `ttl_hours`.
    ///
    /// Returns the count of rows deleted.
    ///
    /// SQL moved verbatim from `akashic-curation::saga::mod::cleanup_expired_idempotency`.
    async fn cleanup_expired_idempotency(&self, ttl_hours: i64) -> anyhow::Result<i64>;

    // ── Idempotency-key guard (sagas table keyed by idempotency_key) ─────────
    //
    // SQL moved verbatim from `akashic-server::api::extractors`. The state
    // machine (completed/running/failed) + the atomic INSERT-on-conflict
    // reservation + the bounded stale-`failed` retry loop all live in the
    // adapter; callers map the returned outcome to their transport error.

    /// Fast-path check: `completed` → cached result, `running` → in-flight,
    /// `failed` → cleaned up and reported as fresh.
    async fn idempotency_check(&self, key: &str) -> anyhow::Result<IdempotencyCheck>;

    /// Atomic `INSERT ... ON CONFLICT DO NOTHING` reservation; on conflict the
    /// existing row's status decides Reserved / Cached / InProgress.
    async fn idempotency_reserve(
        &self,
        key: &str,
        saga_type: &str,
        repo_name: &str,
    ) -> anyhow::Result<IdempotencyReservation>;

    /// Mark the reserved row `completed` and store its result JSON.
    async fn idempotency_finalize(&self, key: &str, result: &JsonValue) -> anyhow::Result<()>;

    /// Delete a still-`running` reservation so the key can be retried.
    /// Best-effort: errors are logged by the adapter, not returned.
    async fn idempotency_release(&self, key: &str);
}
