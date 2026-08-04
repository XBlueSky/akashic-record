//! Port trait for knowledge-graph edge writes (Neo4j side).
//!
//! `EdgeRepo` consolidates the EXPLAINS and ATTACHED_TO edge creation
//! used by the linking pipeline (`linking/explains.rs` and `linking/mod.rs`).
//!
//! Adapter implementations live in `akashic-store-neo4j`.

use async_trait::async_trait;
use uuid::Uuid;

/// Repository contract for knowledge-graph edge writes in Neo4j.
///
/// All methods return `anyhow::Result` — error-type unification deferred.
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn EdgeRepo>`.
#[async_trait]
pub trait EdgeRepo: Send + Sync {
    /// MERGE an EXPLAINS edge from a Section to a Chunk.
    ///
    /// Cypher moved verbatim from
    /// `akashic-retrieval::linking::explains::create_deterministic_explains_edges`
    /// and `create_llm_verified_explains_edges` (A1 Task 7).
    async fn merge_explains_to_chunk(
        &self,
        section_pg_id: Uuid,
        chunk_pg_id: Uuid,
        confidence: f64,
        method: &str,
        target_repo: &str,
    ) -> anyhow::Result<()>;

    /// MERGE an EXPLAINS edge from a Section to a Module.
    ///
    /// Cypher moved verbatim from
    /// `akashic-retrieval::linking::explains::create_deterministic_explains_edges`
    /// module-match branch (A1 Task 7).
    async fn merge_explains_to_module(
        &self,
        section_pg_id: Uuid,
        module_pg_id: Uuid,
        confidence: f64,
        method: &str,
        target_repo: &str,
    ) -> anyhow::Result<()>;

    /// MERGE an ATTACHED_TO edge from a Note to a Chunk.
    ///
    /// Cypher moved verbatim from
    /// `akashic-retrieval::linking::mod.rs::link_note_to_chunks`
    /// (A1 Task 7).
    async fn merge_attached_to_chunk(
        &self,
        note_pg_id: &str,
        chunk_pg_id: Uuid,
    ) -> anyhow::Result<()>;

    /// Best-effort staleness signal (Task 8, B5): stamp `code_sha` onto every
    /// EXPLAINS edge outgoing from `section_pg_id`.
    ///
    /// The docs-corpus derive job anchors EXPLAINS edges against whatever
    /// code snapshot the repo currently has ingested, even when that
    /// snapshot's `git_ref` differs from the corpus version's own `sha` —
    /// there is no requirement that docs and code be derived from the same
    /// commit. Recording the code snapshot's sha directly on the edge turns
    /// a mismatch into a queryable staleness signal instead of silently
    /// asserting the two are in sync.
    async fn annotate_explains_code_sha(
        &self,
        section_pg_id: Uuid,
        code_sha: &str,
    ) -> anyhow::Result<()>;
}
