//! Port trait for chunk graph operations (Neo4j side).
//!
//! `ChunkGraphRepo` handles MERGE / DETACH DELETE of `:Chunk` nodes and their
//! `:TAGGED_WITH` edges. Adapter implementations live in `akashic-store-neo4j`.

use async_trait::async_trait;

/// A single chunk's graph properties, used for batched node creation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChunkGraphNode {
    pub pg_id: String,
    pub name: String,
    pub chunk_type: String,
    pub module_path: String,
    pub fqn: Option<String>,
    pub parent_fqn: Option<String>,
    pub start_line: i64,
    pub end_line: i64,
    pub visibility: String,
    pub is_async: bool,
    pub is_static: bool,
    pub is_exported: bool,
    /// HTTP verb for `route` / `http_call` chunks (C1). `None` for all others →
    /// the Neo4j `SET` writes null, i.e. no property is created.
    pub http_method: Option<String>,
    /// HTTP path for `route` / `http_call` chunks (C1). `None` for all others.
    pub http_path: Option<String>,
}

/// Repository contract for `:Chunk` nodes in the Neo4j graph.
#[async_trait]
pub trait ChunkGraphRepo: Send + Sync {
    /// Batch MERGE of `:Chunk` nodes for all chunks in one module.
    async fn create_chunk_nodes_batch(
        &self,
        repo_name: &str,
        chunks: &[ChunkGraphNode],
    ) -> anyhow::Result<()>;

    /// Batch MERGE of `:Tag` nodes + `[:TAGGED_WITH]` edges.
    async fn create_tagged_with_edges(
        &self,
        tag_pg_ids: Vec<String>,
        tag_names: Vec<String>,
    ) -> anyhow::Result<()>;

    /// DETACH DELETE all `:Chunk` nodes for a repo.
    async fn delete_chunks_by_repo(&self, repo_name: &str) -> anyhow::Result<()>;

    /// DETACH DELETE `:Chunk` nodes under a specific module path.
    async fn delete_chunks_by_module(
        &self,
        repo_name: &str,
        module_path: &str,
    ) -> anyhow::Result<()>;
}

/// A single tag pair for `[:TAGGED_WITH]` edge creation.
#[derive(Debug, Clone)]
pub struct TagEdge {
    pub chunk_pg_id: String,
    pub tag_name: String,
}
