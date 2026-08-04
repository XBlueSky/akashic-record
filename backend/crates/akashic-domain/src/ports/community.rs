//! Port traits for community persistence.
//!
//! `CommunityRepo` covers the PostgreSQL side (communities + community_members
//! tables).  `CommunityGraphRepo` covers the Neo4j side (Community nodes and
//! HAS_MEMBER edges).
//!
//! Both traits are infra-free: they use only domain types from
//! `akashic_domain::types`.  Adapter implementations live in
//! `akashic-store-pg` and `akashic-store-neo4j` respectively.

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::CommunitySummaryRow;

// ── CommunityRepo ─────────────────────────────────────────────────────────────

/// Repository contract for the `communities` and `community_members` PG tables.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn CommunityRepo>`.
#[async_trait]
pub trait CommunityRepo: Send + Sync {
    // ── Write ────────────────────────────────────────────────────────────────

    /// Delete all community_members for a repo (by join through communities),
    /// then delete the communities themselves.
    ///
    /// SQL moved verbatim from `akashic-ingestion::community::mod::store_communities`
    /// (the clean-old-data prologue).
    async fn clean_communities(&self, repo_name: &str) -> anyhow::Result<()>;

    /// Insert a new community row and return its UUID.
    ///
    /// SQL moved verbatim from `akashic-ingestion::community::mod::store_communities`.
    async fn insert_community(
        &self,
        repo_name: &str,
        level: i16,
        member_count: i32,
    ) -> anyhow::Result<Uuid>;

    /// Insert a chunk-member link into community_members.
    ///
    /// SQL moved verbatim from `akashic-ingestion::community::mod::store_communities`.
    async fn insert_community_chunk_member(
        &self,
        community_id: Uuid,
        chunk_id: Uuid,
    ) -> anyhow::Result<()>;

    /// Insert a module-member link into community_members.
    ///
    /// SQL moved verbatim from `akashic-ingestion::community::mod::store_communities`.
    async fn insert_community_module_member(
        &self,
        community_id: Uuid,
        module_id: Uuid,
    ) -> anyhow::Result<()>;

    /// Update a community row with LLM-generated name, summary, and embedding.
    ///
    /// SQL moved verbatim from `akashic-ingestion::community::summarize::summarize_communities`.
    async fn update_community_summary(
        &self,
        community_id: Uuid,
        name: &str,
        summary: &str,
        embedding: Option<&[f32]>,
    ) -> anyhow::Result<()>;

    /// Resolve the PostgreSQL community id for an in-memory community by matching
    /// on a representative member (chunk or module), not by member_count.
    ///
    /// Returns `None` when no community row is found for the member at that level.
    ///
    /// SQL moved verbatim from `akashic-ingestion::community::summarize::resolve_community_id`.
    async fn resolve_community_id_by_chunk_member(
        &self,
        repo_name: &str,
        level: i16,
        member_pg_id: Uuid,
    ) -> anyhow::Result<Option<Uuid>>;

    /// Resolve the PostgreSQL community id for an in-memory community by matching
    /// on a representative module member.
    ///
    /// SQL moved verbatim from `akashic-ingestion::community::summarize::resolve_community_id`.
    async fn resolve_community_id_by_module_member(
        &self,
        repo_name: &str,
        level: i16,
        member_pg_id: Uuid,
    ) -> anyhow::Result<Option<Uuid>>;

    // ── Read (global_query) ──────────────────────────────────────────────────

    /// Return the highest level at which any community in `repo_name` has a
    /// non-NULL summary.
    ///
    /// SQL: `SELECT MAX(level) FROM communities
    ///       WHERE repo_name = $1 AND summary IS NOT NULL`.
    async fn max_summarized_level(&self, repo_name: &str) -> anyhow::Result<Option<i16>>;

    /// Vector search over community summaries at a given level.
    ///
    /// SQL (verbatim from MCP global_query):
    /// `SELECT id, coalesce(name,'Unnamed'), coalesce(summary,''), level, member_count,
    ///         (1.0 - (embedding <=> $1::vector))::float8 AS score
    ///  FROM communities
    ///  WHERE repo_name = $2 AND level = $3 AND embedding IS NOT NULL
    ///  ORDER BY embedding <=> $1::vector LIMIT $4`.
    async fn select_communities_by_vector(
        &self,
        repo_name: &str,
        level: i16,
        embedding: &[f32],
        limit: i64,
    ) -> anyhow::Result<Vec<CommunitySummaryRow>>;

    /// Fetch example chunk triples for a community (top N by community membership).
    ///
    /// SQL (verbatim from MCP global_query):
    /// `SELECT c.name, c.chunk_type, c.module_path
    ///  FROM community_members cm JOIN chunks c ON cm.chunk_id = c.id
    ///  WHERE cm.community_id = $1 LIMIT $2`.
    async fn fetch_community_examples(
        &self,
        community_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<(String, String, String)>>;
}

// ── CommunityGraphRepo ────────────────────────────────────────────────────────

/// Repository contract for community nodes and edges in Neo4j.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn CommunityGraphRepo>`.
#[async_trait]
pub trait CommunityGraphRepo: Send + Sync {
    /// DETACH DELETE all Community nodes for a repo.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::community::mod::store_communities`
    /// (the clean-old-data prologue).
    async fn delete_communities_by_repo(&self, repo_name: &str) -> anyhow::Result<()>;

    /// CREATE a single Community node.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::community::mod::store_communities`.
    async fn create_community_node(
        &self,
        community_id: Uuid,
        level: u8,
        repo_name: &str,
        member_count: usize,
    ) -> anyhow::Result<()>;

    /// CREATE a HAS_MEMBER edge from a Community node to a Chunk or Module node.
    ///
    /// `member_label` is either `"Chunk"` or `"Module"`.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::community::mod::store_communities`.
    async fn create_has_member_edge(
        &self,
        community_id: Uuid,
        member_pg_id: Uuid,
        member_label: &str,
    ) -> anyhow::Result<()>;

    /// Load chunk/module nodes and weighted edges from Neo4j for Leiden community detection.
    ///
    /// Returns `(node_ids, node_meta[(id → (name, type))], edges[(src, tgt, weight)])`.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::community::mod::load_graph`.
    async fn load_graph_for_detection(
        &self,
        repo_name: &str,
    ) -> anyhow::Result<(
        Vec<Uuid>,
        std::collections::HashMap<Uuid, (String, String)>,
        Vec<(Uuid, Uuid, f64)>,
    )>;
}
