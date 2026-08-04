//! Neo4j adapter for `ModuleGraphRepo`.
//!
//! Cypher moved verbatim from `akashic-ingestion::ingestion::store.rs`.

use anyhow::Result;
use async_trait::async_trait;
use neo4rs::query;
use uuid::Uuid;

use akashic_domain::ports::ModuleGraphRepo;

use crate::Neo4jPool;

/// Neo4j adapter implementing [`ModuleGraphRepo`].
#[derive(Clone)]
pub struct Neo4jModuleGraphRepo {
    graph: Neo4jPool,
}

impl Neo4jModuleGraphRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl ModuleGraphRepo for Neo4jModuleGraphRepo {
    async fn upsert_module_node(
        &self,
        repo_name: &str,
        module_pg_id: Uuid,
        path: &str,
    ) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MERGE (m:Module {pg_id: $pg_id}) \
                     SET m.repo_name = $repo_name, m.path = $path",
                )
                .param("pg_id", module_pg_id.to_string().as_str())
                .param("repo_name", repo_name)
                .param("path", path),
            )
            .await?;
        Ok(())
    }

    async fn create_has_module_edge(&self, repo_name: &str, module_pg_id: Uuid) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MATCH (r:Repository {name: $repo_name}) \
                     MATCH (m:Module {pg_id: $pg_id}) \
                     MERGE (r)-[:HAS_MODULE]->(m)",
                )
                .param("repo_name", repo_name)
                .param("pg_id", module_pg_id.to_string().as_str()),
            )
            .await?;
        Ok(())
    }

    async fn create_has_chunk_edges(
        &self,
        module_pg_id: Uuid,
        chunk_pg_ids: &[Uuid],
    ) -> Result<()> {
        for chunk_id in chunk_pg_ids {
            self.graph
                .execute(
                    query(
                        "MATCH (m:Module {pg_id: $module_id}) \
                         MATCH (c:Chunk {pg_id: $chunk_id}) \
                         MERGE (m)-[:HAS_CHUNK]->(c)",
                    )
                    .param("module_id", module_pg_id.to_string().as_str())
                    .param("chunk_id", chunk_id.to_string().as_str()),
                )
                .await?;
        }
        Ok(())
    }

    async fn delete_modules_by_repo(&self, repo_name: &str) -> Result<()> {
        self.graph
            .execute(
                query("MATCH (m:Module {repo_name: $repo_name}) DETACH DELETE m")
                    .param("repo_name", repo_name),
            )
            .await?;
        Ok(())
    }

    async fn get_module_nodes(&self, repo_name: &str) -> Result<Vec<(String, String)>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (m:Module {repo_name: $repo_name}) \
                     RETURN m.pg_id AS pg_id, m.path AS path",
                )
                .param("repo_name", repo_name),
            )
            .await?;

        let mut result = Vec::new();
        for row in rows {
            let pg_id: String = row.get("pg_id").unwrap_or_default();
            let path: String = row.get("path").unwrap_or_default();
            result.push((pg_id, path));
        }
        Ok(result)
    }
}
