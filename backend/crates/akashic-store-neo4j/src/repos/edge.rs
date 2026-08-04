//! Neo4j adapter for `EdgeRepo`.
//!
//! All Cypher is moved verbatim from the original call-sites:
//! - `akashic-retrieval::linking::explains` (`create_deterministic_explains_edges`,
//!   `create_llm_verified_explains_edges`)
//! - `akashic-retrieval::linking::mod.rs` (`link_note_to_chunks`)

use anyhow::{Context, Result};
use async_trait::async_trait;
use neo4rs::query;
use uuid::Uuid;

use akashic_domain::ports::EdgeRepo;

use crate::Neo4jPool;

/// Neo4j adapter implementing [`EdgeRepo`].
#[derive(Clone)]
pub struct Neo4jEdgeRepo {
    graph: Neo4jPool,
}

impl Neo4jEdgeRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl EdgeRepo for Neo4jEdgeRepo {
    async fn merge_explains_to_chunk(
        &self,
        section_pg_id: Uuid,
        chunk_pg_id: Uuid,
        confidence: f64,
        method: &str,
        target_repo: &str,
    ) -> Result<()> {
        // Cypher moved verbatim from
        // akashic-retrieval::linking::explains::create_deterministic_explains_edges
        // and create_llm_verified_explains_edges.
        self.graph
            .execute(
                query(
                    "MATCH (s:Section {pg_id: $section_id}) \
                     MATCH (c:Chunk {pg_id: $chunk_id}) \
                     MERGE (s)-[:EXPLAINS {confidence: $conf, method: $method, target_repo: $target_repo}]->(c)",
                )
                .param("section_id", section_pg_id.to_string().as_str())
                .param("chunk_id", chunk_pg_id.to_string().as_str())
                .param("conf", confidence)
                .param("method", method)
                .param("target_repo", target_repo),
            )
            .await?;
        Ok(())
    }

    async fn merge_explains_to_module(
        &self,
        section_pg_id: Uuid,
        module_pg_id: Uuid,
        confidence: f64,
        method: &str,
        target_repo: &str,
    ) -> Result<()> {
        // Cypher moved verbatim from
        // akashic-retrieval::linking::explains::create_deterministic_explains_edges
        // module-match branch.
        self.graph
            .execute(
                query(
                    "MATCH (s:Section {pg_id: $section_id}) \
                     MATCH (m:Module {pg_id: $module_id}) \
                     MERGE (s)-[:EXPLAINS {confidence: $conf, method: $method, target_repo: $target_repo}]->(m)",
                )
                .param("section_id", section_pg_id.to_string().as_str())
                .param("module_id", module_pg_id.to_string().as_str())
                .param("conf", confidence)
                .param("method", method)
                .param("target_repo", target_repo),
            )
            .await?;
        Ok(())
    }

    async fn merge_attached_to_chunk(&self, note_pg_id: &str, chunk_pg_id: Uuid) -> Result<()> {
        // Cypher moved verbatim from
        // akashic-retrieval::linking::mod.rs::link_note_to_chunks.
        let cypher = "
            MATCH (ctx:Note {pg_id: $ctx_id})
            MATCH (c:Chunk {pg_id: $chunk_id})
            MERGE (ctx)-[:ATTACHED_TO]->(c)
        ";
        self.graph
            .execute(
                query(cypher)
                    .param("ctx_id", note_pg_id)
                    .param("chunk_id", chunk_pg_id.to_string().as_str()),
            )
            .await?;
        Ok(())
    }

    async fn annotate_explains_code_sha(&self, section_pg_id: Uuid, code_sha: &str) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MATCH (s:Section {pg_id: $section_id})-[r:EXPLAINS]->() \
                     SET r.code_sha = $code_sha",
                )
                .param("section_id", section_pg_id.to_string().as_str())
                .param("code_sha", code_sha),
            )
            .await
            .context("Failed to annotate EXPLAINS edges with code_sha")?;
        Ok(())
    }
}
