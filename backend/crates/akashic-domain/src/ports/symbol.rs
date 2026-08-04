//! Port traits for symbol resolution (PostgreSQL side).
//!
//! `SymbolRepo` covers exact-match symbol lookup, reference enrichment, and
//! chunk/module id lookups used by the linking pipeline.
//!
//! Adapter implementations live in `akashic-store-pg`.

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::{ChunkIdRow, ChunkMetaRow, ModuleIdRow, SymbolCandidateRow};

/// Repository contract for symbol lookups against the `chunks` and `modules`
/// PG tables.
///
/// All methods return `anyhow::Result` — error-type unification is deferred to
/// a later slice.  Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn SymbolRepo>`.
#[async_trait]
pub trait SymbolRepo: Send + Sync {
    /// Resolve a symbol name to candidate chunk definitions via exact match.
    ///
    /// Matches `fqn = name`, `fqn LIKE '%.name'`, or `name = name`; optionally
    /// filtered by `repo`, `module_hint` (ILIKE substring), and `kind_hint`
    /// (exact chunk_type match).
    ///
    /// SQL moved verbatim from
    /// `akashic-retrieval::graphrag::symbol_resolution::resolve_symbol`
    /// (A1 Task 7).
    async fn resolve_symbol_candidates(
        &self,
        name: &str,
        escaped_suffix: &str,
        repo: Option<&str>,
        module_hint: Option<&str>,
        kind_hint: Option<&str>,
    ) -> anyhow::Result<Vec<SymbolCandidateRow>>;

    /// Batch-enrich caller rows with (module_path, chunk_type) from the
    /// `chunks` table by fqn — no repo constraint (callers may be cross-repo).
    ///
    /// SQL moved verbatim from
    /// `akashic-retrieval::graphrag::symbol_resolution::find_references`
    /// enrichment block (A1 Task 7).
    async fn enrich_chunk_meta_by_fqns(&self, fqns: &[String])
    -> anyhow::Result<Vec<ChunkMetaRow>>;

    /// Batch-enrich rows with (module_path, chunk_type) from `chunks` by fqn,
    /// optionally constrained to a repo.
    ///
    /// SQL moved verbatim from
    /// `akashic-retrieval::graphrag::symbol_resolution::find_implementations`
    /// enrichment block (A1 Task 7).
    async fn enrich_chunk_meta_by_fqns_and_repo(
        &self,
        fqns: &[String],
        repo: Option<&str>,
    ) -> anyhow::Result<Vec<ChunkMetaRow>>;

    /// Find chunk ids matching an exact symbol name (min length 5) across all
    /// repos, up to `limit`.
    ///
    /// SQL moved verbatim from
    /// `akashic-retrieval::linking::explains::create_deterministic_explains_edges`
    /// chunk-match branch (A1 Task 7).
    async fn find_chunk_ids_by_name(
        &self,
        name: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<ChunkIdRow>>;

    /// Find chunk ids matching an exact symbol name AND a parent module-path
    /// substring (LIKE), in a specific repo, up to `limit`.
    ///
    /// SQL moved verbatim from
    /// `akashic-retrieval::linking::mod.rs::link_note_to_chunks`
    /// parent-qualified branch (A1 Task 7).
    async fn find_chunk_ids_by_name_and_module(
        &self,
        repo_name: &str,
        name: &str,
        parent: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<Uuid>>;

    /// Find chunk ids matching an exact symbol name in a specific repo.
    ///
    /// SQL moved verbatim from
    /// `akashic-retrieval::linking::mod.rs::link_note_to_chunks`
    /// simple-name branch (A1 Task 7).
    async fn find_chunk_ids_by_name_in_repo(
        &self,
        repo_name: &str,
        name: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<Uuid>>;

    /// Find module ids whose path contains the given fragment (LIKE), with min
    /// length 5, across all repos, up to `limit`.
    ///
    /// SQL moved verbatim from
    /// `akashic-retrieval::linking::explains::create_deterministic_explains_edges`
    /// module-match branch (A1 Task 7).
    async fn find_module_ids_by_path_fragment(
        &self,
        fragment: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<ModuleIdRow>>;

    /// Find candidates for embedding-similarity explains linking.
    ///
    /// Returns top `limit` chunks ordered by embedding cosine distance to the
    /// query vector, for all repos.
    ///
    /// SQL moved verbatim from
    /// `akashic-retrieval::linking::explains::create_llm_verified_explains_edges`
    /// candidate-fetch block (A1 Task 7).
    async fn find_chunks_for_explains_linking(
        &self,
        query_vec: &[f32],
        limit: i64,
    ) -> anyhow::Result<Vec<ExplainsLinkCandidate>>;
}

/// A candidate chunk for the `EXPLAINS` embedding-similarity linking.
///
/// Used by `SymbolRepo::find_chunks_for_explains_linking` (A1 Task 7).
#[derive(Debug, Clone)]
pub struct ExplainsLinkCandidate {
    pub id: Uuid,
    pub name: String,
    pub chunk_type: String,
    pub module_path: String,
    pub repo_name: String,
    pub similarity: f64,
}
