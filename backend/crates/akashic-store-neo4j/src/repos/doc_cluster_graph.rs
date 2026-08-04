//! Neo4j adapter for `DocClusterGraphRepo`.
//!
//! Cypher moved verbatim from `akashic-ingestion::ingestion::doc_clustering`
//! (`clean_clusters`, `create_neo4j_cluster`).

use anyhow::{Context, Result};
use async_trait::async_trait;
use neo4rs::query;
use uuid::Uuid;

use akashic_domain::ports::DocClusterGraphRepo;

use crate::Neo4jPool;

/// Neo4j adapter implementing [`DocClusterGraphRepo`].
#[derive(Clone)]
pub struct Neo4jDocClusterGraphRepo {
    graph: Neo4jPool,
}

impl Neo4jDocClusterGraphRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl DocClusterGraphRepo for Neo4jDocClusterGraphRepo {
    async fn delete_clusters_by_repo(&self, repo_name: &str) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MATCH (tc:TopicCluster {repo_name: $repo}) \
                     DETACH DELETE tc",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to clean Neo4j topic clusters")?;
        Ok(())
    }

    async fn create_cluster_node(
        &self,
        cluster_pg_id: Uuid,
        name: &str,
        repo_name: &str,
    ) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MERGE (tc:TopicCluster {pg_id: $cluster_id}) \
                     SET tc.name = $name, tc.repo_name = $repo_name \
                     WITH tc \
                     MERGE (r:Repository {name: $repo_name}) \
                     MERGE (tc)-[:BELONGS_TO]->(r)",
                )
                .param("cluster_id", cluster_pg_id.to_string().as_str())
                .param("name", name)
                .param("repo_name", repo_name),
            )
            .await?;
        Ok(())
    }

    async fn create_in_cluster_edge(&self, section_pg_id: Uuid, cluster_pg_id: Uuid) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MATCH (s:Section {pg_id: $section_id}) \
                     MATCH (tc:TopicCluster {pg_id: $cluster_id}) \
                     MERGE (s)-[:IN_CLUSTER]->(tc)",
                )
                .param("section_id", section_pg_id.to_string().as_str())
                .param("cluster_id", cluster_pg_id.to_string().as_str()),
            )
            .await?;
        Ok(())
    }
}
