//! Port traits for module persistence (PostgreSQL side).
//!
//! `ModuleRepo` is the infra-free contract for the `modules` PG table.
//! Adapter implementations live in `akashic-store-pg`.

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::{ModuleMetaRow, ModuleRow, ModuleSummary};

/// Repository contract for the `modules` PG table.
#[async_trait]
pub trait ModuleRepo: Send + Sync {
    // ── Write (ingestion) ────────────────────────────────────────────────────

    /// Upsert a module row. The embedding vector is pre-computed by the caller.
    #[allow(clippy::too_many_arguments)]
    async fn upsert_module(
        &self,
        repo_name: &str,
        path: &str,
        language: &str,
        summary: &str,
        exports_count: i32,
        file_count: i32,
        is_virtual: bool,
        git_ref: &str,
        embedding: Option<&[f32]>,
    ) -> anyhow::Result<Uuid>;

    /// Delete all modules for a repo (re-ingest cleanup).
    async fn clean_modules(&self, repo_name: &str) -> anyhow::Result<()>;

    /// Delete all modules for a repo (lifecycle delete).
    async fn delete_modules_by_repo(&self, repo_name: &str) -> anyhow::Result<()>;

    // ── Read (interfaces / API) ───────────────────────────────────────────────

    /// Count modules for a repo.
    async fn count_modules(&self, repo_name: &str) -> anyhow::Result<i64>;

    /// List modules for a repo (paginated).
    async fn list_modules(
        &self,
        repo_name: &str,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<ModuleRow>>;

    /// Get a single module by id.
    async fn get_module_detail(&self, module_id: Uuid) -> anyhow::Result<Option<ModuleRow>>;

    /// Fetch (path, summary, language) for a module by id (graphrag context).
    async fn fetch_module_summary(&self, module_id: Uuid) -> anyhow::Result<Option<ModuleSummary>>;

    /// Resolve module paths to ids in a single query (for import-edge wiring).
    async fn resolve_module_paths(
        &self,
        repo_name: &str,
        paths: &[String],
    ) -> anyhow::Result<Vec<(String, Uuid)>>;

    /// Batch-fetch (language, is_virtual) for a set of module ids.
    ///
    /// SQL: `SELECT id, language, is_virtual FROM modules WHERE id = ANY($1)`.
    /// Used by GraphService::get_module_graph to decorate Neo4j module nodes with
    /// PG metadata without an N+1 per-module query.
    async fn get_module_meta_batch(
        &self,
        module_ids: &[Uuid],
    ) -> anyhow::Result<Vec<ModuleMetaRow>>;

    /// Batch-fetch (id, path) for a set of module ids.
    ///
    /// SQL: `SELECT id, path FROM modules WHERE id = ANY($1)`.
    /// Used to resolve module_path from module_id for chunk-count aggregation.
    async fn get_module_paths_batch(
        &self,
        module_ids: &[Uuid],
    ) -> anyhow::Result<Vec<(Uuid, String)>>;

    /// Fetch (path, repo_name) for a module by id.
    ///
    /// SQL: `SELECT id, path, repo_name FROM modules WHERE id = $1`.
    /// Used by get_module_call_graph to resolve path + repo_name.
    async fn get_module_path_and_repo(
        &self,
        module_id: Uuid,
    ) -> anyhow::Result<Option<(Uuid, String, String)>>;
}
