//! Neo4j adapter for `DocumentGraphRepo`.
//!
//! Cypher moved verbatim from `akashic-ingestion::ingestion::doc_store` (`create_document`,
//! `store_sections`, `clean_repo_docs`).

use anyhow::{Context, Result};
use async_trait::async_trait;
use neo4rs::query;
use uuid::Uuid;

use akashic_domain::ports::DocumentGraphRepo;

use crate::Neo4jPool;

/// Neo4j adapter implementing [`DocumentGraphRepo`].
#[derive(Clone)]
pub struct Neo4jDocumentGraphRepo {
    graph: Neo4jPool,
}

impl Neo4jDocumentGraphRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl DocumentGraphRepo for Neo4jDocumentGraphRepo {
    async fn create_document_node(
        &self,
        repo_name: &str,
        doc_pg_id: Uuid,
        title: &str,
        doc_type: &str,
    ) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MERGE (r:Repository {name: $repo_name}) \
                     MERGE (d:Document {pg_id: $pg_id}) \
                     SET d.repo_name = $repo_name, d.title = $title, d.doc_type = $doc_type \
                     MERGE (d)-[:BELONGS_TO]->(r)",
                )
                .param("repo_name", repo_name)
                .param("pg_id", doc_pg_id.to_string().as_str())
                .param("title", title)
                .param("doc_type", doc_type),
            )
            .await
            .context("Failed to create Document node in Neo4j")?;
        Ok(())
    }

    async fn create_section_node(
        &self,
        section_pg_id: Uuid,
        doc_pg_id: Uuid,
        heading: &str,
        depth: i64,
    ) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MERGE (s:Section {pg_id: $pg_id}) \
                     SET s.doc_id = $doc_id, s.heading = $heading, s.depth = $depth",
                )
                .param("pg_id", section_pg_id.to_string().as_str())
                .param("doc_id", doc_pg_id.to_string().as_str())
                .param("heading", heading)
                .param("depth", depth),
            )
            .await
            .context("Failed to create Section node in Neo4j")?;
        Ok(())
    }

    async fn create_has_subsection_edge(
        &self,
        parent_pg_id: Uuid,
        child_pg_id: Uuid,
    ) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MATCH (p:Section {pg_id: $parent_id}) \
                     MATCH (c:Section {pg_id: $child_id}) \
                     MERGE (p)-[:HAS_SUBSECTION]->(c)",
                )
                .param("parent_id", parent_pg_id.to_string().as_str())
                .param("child_id", child_pg_id.to_string().as_str()),
            )
            .await
            .context("Failed to create HAS_SUBSECTION edge in Neo4j")?;
        Ok(())
    }

    async fn create_has_section_edge(&self, doc_pg_id: Uuid, section_pg_id: Uuid) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MATCH (d:Document {pg_id: $doc_id}) \
                     MATCH (s:Section {pg_id: $section_id}) \
                     MERGE (d)-[:HAS_SECTION]->(s)",
                )
                .param("doc_id", doc_pg_id.to_string().as_str())
                .param("section_id", section_pg_id.to_string().as_str()),
            )
            .await
            .context("Failed to create HAS_SECTION edge in Neo4j")?;
        Ok(())
    }

    async fn create_tagged_with_edge(&self, section_pg_id: Uuid, tag: &str) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MERGE (t:Tag {name: $tag}) \
                     WITH t \
                     MATCH (s:Section {pg_id: $section_id}) \
                     MERGE (s)-[:TAGGED_WITH]->(t)",
                )
                .param("tag", tag)
                .param("section_id", section_pg_id.to_string().as_str()),
            )
            .await
            .context("Failed to create TAGGED_WITH edge in Neo4j")?;
        Ok(())
    }

    async fn clean_repo_docs(&self, repo_name: &str) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MATCH (d:Document {repo_name: $repo}) \
                     OPTIONAL MATCH (d)-[:HAS_SECTION|HAS_SUBSECTION*]->(s:Section) \
                     DETACH DELETE s, d",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to clean repo documents from Neo4j")?;
        Ok(())
    }

    async fn clean_repo_corpus_docs(&self, repo_name: &str) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MATCH (d:Document {repo_name: $repo, doc_type: 'corpus'}) \
                     OPTIONAL MATCH (d)-[:HAS_SECTION|HAS_SUBSECTION*]->(s:Section) \
                     DETACH DELETE s, d",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to clean repo corpus documents from Neo4j")?;
        Ok(())
    }
}
