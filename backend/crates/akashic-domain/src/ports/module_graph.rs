//! Port trait for module graph operations (Neo4j side).
//!
//! `ModuleGraphRepo` handles MERGE / DETACH DELETE of `:Module` nodes and their
//! `[:HAS_MODULE]` / `[:HAS_CHUNK]` edges. Adapter implementations live in
//! `akashic-store-neo4j`.

use async_trait::async_trait;
use uuid::Uuid;

/// Repository contract for `:Module` nodes in the Neo4j graph.
#[async_trait]
pub trait ModuleGraphRepo: Send + Sync {
    /// MERGE a `:Module` node, setting its `repo_name` and `path` properties.
    async fn upsert_module_node(
        &self,
        repo_name: &str,
        module_pg_id: Uuid,
        path: &str,
    ) -> anyhow::Result<()>;

    /// MERGE `(:Repository {name})-[:HAS_MODULE]->(:Module {pg_id})`.
    async fn create_has_module_edge(
        &self,
        repo_name: &str,
        module_pg_id: Uuid,
    ) -> anyhow::Result<()>;

    /// MERGE `(:Module {pg_id})-[:HAS_CHUNK]->(:Chunk {pg_id})` for each chunk id.
    async fn create_has_chunk_edges(
        &self,
        module_pg_id: Uuid,
        chunk_pg_ids: &[Uuid],
    ) -> anyhow::Result<()>;

    /// DETACH DELETE all `:Module` nodes for a repo.
    async fn delete_modules_by_repo(&self, repo_name: &str) -> anyhow::Result<()>;

    /// Return `(pg_id, path)` pairs for all Module nodes in a repo.
    async fn get_module_nodes(&self, repo_name: &str) -> anyhow::Result<Vec<(String, String)>>;
}
