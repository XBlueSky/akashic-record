//! Neo4j adapter for `CommunityGraphRepo`.
//!
//! Cypher moved verbatim from `akashic-ingestion::community::mod` (`store_communities`,
//! `load_graph`).

use std::collections::HashMap;

use anyhow::{Context, Result};
use async_trait::async_trait;
use neo4rs::query;
use uuid::Uuid;

use akashic_domain::ports::CommunityGraphRepo;

use crate::Neo4jPool;

/// Validate a community member's node label before it is interpolated into
/// Cypher. Cypher cannot parameterize a label, so `create_has_member_edge` must
/// build the label into the query string — and the label reaches this crate
/// straight from a deserialized graph snapshot on the CLI import path. Restrict
/// it to the only two labels a community member can carry so a crafted snapshot
/// (`"Chunk) DETACH DELETE (x) //"`) can't inject arbitrary Cypher.
fn validate_member_label(label: &str) -> Result<&str> {
    match label {
        "Chunk" | "Module" => Ok(label),
        other => anyhow::bail!("Invalid community member label: {other:?}"),
    }
}

/// Neo4j adapter implementing [`CommunityGraphRepo`].
#[derive(Clone)]
pub struct Neo4jCommunityGraphRepo {
    graph: Neo4jPool,
}

impl Neo4jCommunityGraphRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl CommunityGraphRepo for Neo4jCommunityGraphRepo {
    async fn delete_communities_by_repo(&self, repo_name: &str) -> Result<()> {
        self.graph
            .execute(
                query("MATCH (c:Community {repo_name: $repo}) DETACH DELETE c")
                    .param("repo", repo_name),
            )
            .await
            .context("Failed to delete old communities")?;
        Ok(())
    }

    async fn create_community_node(
        &self,
        community_id: Uuid,
        level: u8,
        repo_name: &str,
        member_count: usize,
    ) -> Result<()> {
        self.graph
            .execute(
                query(
                    "CREATE (c:Community { \
                       pg_id: $id, level: $level, repo_name: $repo, \
                       member_count: $count \
                     })",
                )
                .param("id", community_id.to_string().as_str())
                .param("level", level as i64)
                .param("repo", repo_name)
                .param("count", member_count as i64),
            )
            .await?;
        Ok(())
    }

    async fn create_has_member_edge(
        &self,
        community_id: Uuid,
        member_pg_id: Uuid,
        member_label: &str,
    ) -> Result<()> {
        let label = validate_member_label(member_label)?;
        let cypher = format!(
            "MATCH (c:Community {{pg_id: $cid}}) \
             MATCH (m:{label} {{pg_id: $mid}}) \
             CREATE (c)-[:HAS_MEMBER]->(m)"
        );
        self.graph
            .execute(
                query(&cypher)
                    .param("cid", community_id.to_string().as_str())
                    .param("mid", member_pg_id.to_string().as_str()),
            )
            .await?;
        Ok(())
    }

    async fn load_graph_for_detection(
        &self,
        repo_name: &str,
    ) -> Result<(
        Vec<Uuid>,
        HashMap<Uuid, (String, String)>,
        Vec<(Uuid, Uuid, f64)>,
    )> {
        let mut node_ids = Vec::new();
        let mut node_meta: HashMap<Uuid, (String, String)> = HashMap::new();

        // Load chunks
        let chunk_rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo})-[:HAS_MODULE]->(:Module)-[:HAS_CHUNK]->(c:Chunk) \
                     RETURN c.pg_id AS id, c.name AS name",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to load chunks")?;

        for row in &chunk_rows {
            let id_str: String = row.get("id").unwrap_or_default();
            let name: String = row.get("name").unwrap_or_default();
            if let Ok(id) = Uuid::parse_str(&id_str) {
                node_ids.push(id);
                node_meta.insert(id, (name, "chunk".into()));
            }
        }

        // Load modules
        let module_rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo})-[:HAS_MODULE]->(m:Module) \
                     RETURN m.pg_id AS id, m.path AS name",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to load modules")?;

        for row in &module_rows {
            let id_str: String = row.get("id").unwrap_or_default();
            let name: String = row.get("name").unwrap_or_default();
            if let Ok(id) = Uuid::parse_str(&id_str) {
                node_ids.push(id);
                node_meta.insert(id, (name, "module".into()));
            }
        }

        // Load edges: CALLS (weight=confidence), IMPORTS_FROM (0.5), HAS_CHUNK (0.3)
        let mut edges = Vec::new();

        let call_rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo})-[:HAS_MODULE]->(:Module)-[:HAS_CHUNK]->(src:Chunk) \
                     MATCH (src)-[c:CALLS]->(tgt:Chunk) \
                     RETURN src.pg_id AS src, tgt.pg_id AS tgt, c.confidence AS w",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to load CALLS edges")?;

        for row in &call_rows {
            let src: String = row.get("src").unwrap_or_default();
            let tgt: String = row.get("tgt").unwrap_or_default();
            let w: f64 = row.get("w").unwrap_or(1.0);
            if let (Ok(s), Ok(t)) = (Uuid::parse_str(&src), Uuid::parse_str(&tgt)) {
                edges.push((s, t, w));
            }
        }

        let import_rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo})-[:HAS_MODULE]->(src:Module) \
                     MATCH (src)-[:IMPORTS_FROM]->(tgt:Module) \
                     RETURN src.pg_id AS src, tgt.pg_id AS tgt",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to load IMPORTS_FROM edges")?;

        for row in &import_rows {
            let src: String = row.get("src").unwrap_or_default();
            let tgt: String = row.get("tgt").unwrap_or_default();
            if let (Ok(s), Ok(t)) = (Uuid::parse_str(&src), Uuid::parse_str(&tgt)) {
                edges.push((s, t, 0.5));
            }
        }

        let has_chunk_rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo})-[:HAS_MODULE]->(m:Module)-[:HAS_CHUNK]->(c:Chunk) \
                     RETURN m.pg_id AS src, c.pg_id AS tgt",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to load HAS_CHUNK edges")?;

        for row in &has_chunk_rows {
            let src: String = row.get("src").unwrap_or_default();
            let tgt: String = row.get("tgt").unwrap_or_default();
            if let (Ok(s), Ok(t)) = (Uuid::parse_str(&src), Uuid::parse_str(&tgt)) {
                edges.push((s, t, 0.3));
            }
        }

        Ok((node_ids, node_meta, edges))
    }
}

#[cfg(test)]
mod tests {
    use super::validate_member_label;

    #[test]
    fn member_label_allows_only_chunk_and_module() {
        assert_eq!(validate_member_label("Chunk").unwrap(), "Chunk");
        assert_eq!(validate_member_label("Module").unwrap(), "Module");
    }

    #[test]
    fn member_label_rejects_cypher_injection() {
        // The exact shape a crafted snapshot would use to break out of the label
        // position and run arbitrary Cypher.
        assert!(validate_member_label("Chunk) DETACH DELETE (x) //").is_err());
    }

    #[test]
    fn member_label_rejects_unknown_labels() {
        for bad in ["chunk", "Repository", "Note", "", "Chunk "] {
            assert!(
                validate_member_label(bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }
}
