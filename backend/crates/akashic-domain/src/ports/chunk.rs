//! Port traits for chunk persistence (PostgreSQL side).
//!
//! `ChunkRepo` is the infra-free contract that all code accessing `chunks` /
//! `large_chunks` tables must go through. Adapter implementations live in
//! `akashic-store-pg`.

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::{
    ChunkCallResolutionRow, ChunkContentRow, ChunkDefinition, ChunkDetailRow, ChunkFullRow,
    ChunkModuleRow, ChunkRow, ChunkSearchRow, EntryPointChunkRow, GoChunkRow, LanguageCount,
    LargeChunkContentRow, ScoutChunkRow,
};

/// Repository contract for the `chunks` and `large_chunks` PG tables.
///
/// All methods take `anyhow::Result` — error-type unification is deferred to a
/// later slice. Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn ChunkRepo>`.
#[async_trait]
pub trait ChunkRepo: Send + Sync {
    // ── Write (ingestion) ────────────────────────────────────────────────────

    /// Insert a single chunk row into PG.  Returns the new UUID.
    ///
    /// NOTE: embedding vectors are computed by the caller (IngestionStore)
    /// before this call; the adapter receives them as `Vec<f32>` slices.
    /// The signature embedding is optional (only chunks with a signature
    /// produce one).
    #[allow(clippy::too_many_arguments)]
    async fn store_chunk_row(
        &self,
        repo_name: &str,
        module_path: &str,
        chunk_type: &str,
        name: &str,
        signature: Option<&str>,
        content: &str,
        language: &str,
        embedding: &[f32],
        signature_embedding: Option<&[f32]>,
        git_ref: &str,
        fqn: Option<&str>,
        parent_fqn: Option<&str>,
        start_line: i32,
        end_line: i32,
        visibility: &str,
        is_async: bool,
        is_static: bool,
        is_exported: bool,
        doc: Option<&str>,
    ) -> anyhow::Result<Uuid>;

    /// Insert a large (multi-chunk window) record with its embedding.
    async fn insert_large_chunk(
        &self,
        repo_name: &str,
        module_path: &str,
        chunk_ids: &[Uuid],
        content: &str,
        embedding: &[f32],
        git_ref: &str,
    ) -> anyhow::Result<Uuid>;

    // ── Delete (cleanup) ─────────────────────────────────────────────────────

    /// Delete all chunks for a repo (full re-ingest cleanup).
    async fn clean_chunks(&self, repo_name: &str) -> anyhow::Result<()>;

    /// Delete all large_chunks for a repo.
    async fn clean_large_chunks(&self, repo_name: &str) -> anyhow::Result<()>;

    /// Delete chunks belonging to a specific module path.
    async fn clean_module_chunks(&self, repo_name: &str, module_path: &str) -> anyhow::Result<()>;

    /// Delete large_chunks belonging to a specific module path.
    async fn clean_module_large_chunks(
        &self,
        repo_name: &str,
        module_path: &str,
    ) -> anyhow::Result<()>;

    // ── Read (interfaces / API) ───────────────────────────────────────────────

    /// Get single chunk detail by id.
    async fn get_chunk_detail(
        &self,
        repo_name: &str,
        chunk_id: Uuid,
    ) -> anyhow::Result<Option<ChunkDetailRow>>;

    /// List chunks within a module (paginated).
    async fn list_chunks_in_module(
        &self,
        repo_name: &str,
        module_path: &str,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<ChunkRow>>;

    /// Vector (cosine) search over chunk embeddings.
    async fn search_chunks_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<ChunkSearchRow>>;

    /// Scout vector search returning the display columns (module path + per-row
    /// repo name) the unified `GET /api/v1/search` endpoint needs.
    ///
    /// SQL moved verbatim from `akashic-server::api::routes::search::unified_search`
    /// (chunk layer). Distinct from [`search_chunks_by_vector`] so the GraphRAG
    /// RRF path stays unchanged.
    async fn scout_search_chunks_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<ScoutChunkRow>>;

    /// BM25 full-text search over chunks.
    async fn search_chunks_by_bm25(
        &self,
        query: &str,
        repo: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<ChunkSearchRow>>;

    /// Vector search over large-chunk embeddings.
    async fn search_large_chunks_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<ChunkSearchRow>>;

    /// Signature-embedding vector search (for goto-definition quality).
    async fn search_chunks_by_signature(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<(Uuid, String, f64)>>;

    /// Count chunks for a repo (used by repo-detail and memory-stack L0).
    async fn count_chunks(&self, repo_name: &str) -> anyhow::Result<i64>;

    /// Language distribution (top 3) for a repo.
    async fn get_language_distribution(
        &self,
        repo_name: &str,
    ) -> anyhow::Result<Vec<LanguageCount>>;

    /// Most-recent `ingested_at` timestamp for a repo (memory-stack L0).
    async fn get_max_ingested_at(
        &self,
        repo_name: &str,
    ) -> anyhow::Result<Option<chrono::DateTime<chrono::Utc>>>;

    /// Count chunks matching a symbol name (health check).
    async fn check_symbol_in_chunks(
        &self,
        repo_name: &str,
        symbol_name: &str,
    ) -> anyhow::Result<i64>;

    /// Fetch (name, signature, fqn) tuples for a set of chunk ids.
    async fn fetch_chunk_definitions(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<ChunkDefinition>>;

    /// Fetch (content, module_path, language) for a single chunk by id.
    ///
    /// SQL moved verbatim from `akashic-retrieval::graphrag::mod.rs`
    /// `fetch_full_node` Code branch (A1 Task 7).
    async fn fetch_chunk_content(&self, chunk_id: Uuid) -> anyhow::Result<Option<ChunkContentRow>>;

    /// Fetch (content, module_path) for a single large_chunk by id.
    ///
    /// SQL moved verbatim from `akashic-retrieval::graphrag::mod.rs`
    /// `fetch_full_node` large_chunk branch (A1 Task 7).
    async fn fetch_large_chunk_content(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Option<LargeChunkContentRow>>;

    // ── Pipeline query helpers (A1 Task 8) ────────────────────────────────────

    /// Fetch all chunks needed for Stage 6 CALLS-edge resolution.
    ///
    /// Returns `(id, name, module_path, chunk_type, fqn, parent_fqn,
    /// signature, content)` for every chunk belonging to `repo_name`.
    /// `signature` (D1c-1) feeds `ChunkIndex::by_return_type` via
    /// `parse_return_type`. `content` (D1c-2) feeds `ChunkIndex::by_field_type`
    /// via `parse_struct_fields` for STRUCT chunks.
    ///
    /// SQL moved verbatim from
    /// `akashic-ingestion::ingestion::pipeline::run_inner` Stage 6 (A1 Task 8).
    async fn fetch_chunks_for_call_resolution(
        &self,
        repo_name: &str,
    ) -> anyhow::Result<Vec<ChunkCallResolutionRow>>;

    /// Fetch Go chunks needed for structural interface-satisfaction detection.
    ///
    /// Returns `(id, name, chunk_type, content, module_path)` for every chunk
    /// in `repo_name` where `language = 'go'` and `chunk_type IN ('type', 'method')`.
    ///
    /// SQL moved verbatim from
    /// `akashic-ingestion::ingestion::pipeline::run_inner` Stage 6 Go block
    /// (A1 Task 8).
    async fn fetch_go_chunks(&self, repo_name: &str) -> anyhow::Result<Vec<GoChunkRow>>;

    /// Fetch all chunks for Stage 7 entry-point detection.
    ///
    /// Returns `(id, name, module_path, content, language)` for every chunk
    /// belonging to `repo_name`.
    ///
    /// SQL moved verbatim from
    /// `akashic-ingestion::ingestion::pipeline::run_inner` Stage 7 (A1 Task 8).
    async fn fetch_chunks_for_entry_points(
        &self,
        repo_name: &str,
    ) -> anyhow::Result<Vec<EntryPointChunkRow>>;

    /// Count chunks per module path for a set of paths in a repo.
    ///
    /// SQL: `SELECT module_path, COUNT(*) FROM chunks
    ///       WHERE repo_name = $1 AND module_path = ANY($2) GROUP BY module_path`.
    /// Used by GraphService::get_module_graph to batch-fetch chunk counts for all
    /// modules without N+1 per-module queries.
    async fn count_chunks_per_module(
        &self,
        repo_name: &str,
        module_paths: &[String],
    ) -> anyhow::Result<Vec<(String, i64)>>;

    /// Fetch (id, name, chunk_type, signature, content, language) for all chunks
    /// in a module, ordered by chunk_type, name.
    ///
    /// SQL: `SELECT id, name, chunk_type, signature, content, language
    ///       FROM chunks WHERE repo_name = $1 AND module_path = $2
    ///       ORDER BY chunk_type, name`.
    /// Used by GraphService::get_module_detail / get_module_call_graph.
    async fn list_chunks_in_module_all(
        &self,
        repo_name: &str,
        module_path: &str,
    ) -> anyhow::Result<Vec<ChunkRow>>;

    /// Batch-fetch (chunk_type, module_path) for a set of chunk ids.
    ///
    /// SQL: `SELECT id, chunk_type, module_path FROM chunks WHERE id = ANY($1)`.
    /// Used by GraphService::get_module_call_graph to resolve ghost-node metadata.
    async fn get_chunk_module_batch(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<ChunkModuleRow>>;

    /// Batch-fetch (id, name, chunk_type, module_path) for EXPLAINS target chunks.
    ///
    /// SQL: `SELECT id, name, chunk_type, module_path FROM chunks WHERE id = ANY($1)`.
    /// Used by GraphService::get_document_detail / get_cluster_detail.
    async fn fetch_chunks_meta_batch(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<(Uuid, String, String, String)>>;

    /// Batch-fetch full chunk detail rows for the `get_details` endpoint.
    ///
    /// SQL (verbatim from `GET /api/v1/details` handler):
    /// `SELECT id, name, chunk_type, module_path, signature, content, language
    ///  FROM chunks WHERE id = ANY($1)`.
    async fn fetch_chunks_full_batch(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<ChunkFullRow>>;
}
