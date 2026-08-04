use std::sync::Arc;

use anyhow::Result;
use tracing::info;
use uuid::Uuid;

use akashic_domain::ports::{DocumentGraphRepo, DocumentRepo, EdgeRepo, SymbolRepo};
use akashic_embed::EmbeddingProvider;
use akashic_retrieval::linking::explains;
use akashic_store_neo4j::{Neo4jEdgeRepo, Neo4jPool};
use akashic_store_pg::{PgDocumentRepo, PgSymbolRepo};

use akashic_store_neo4j::Neo4jDocumentGraphRepo;

use akashic_extraction::special::sections::RawSection;

/// Store ingested documents and their hierarchical sections into PostgreSQL + Neo4j.
pub struct DocStore {
    pub doc_repo: Arc<dyn DocumentRepo>,
    pub doc_graph: Arc<dyn DocumentGraphRepo>,
    // Raw pools still needed for the EXPLAINS linking helpers (A2 scope).
    pub pg: sqlx::PgPool,
    pub neo4j: Neo4jPool,
    pub embedder: Arc<dyn EmbeddingProvider>,
    pub llm: Option<Arc<dyn akashic_llm::LlmProvider>>,
}

impl DocStore {
    pub fn new(
        pg: sqlx::PgPool,
        neo4j: Neo4jPool,
        embedder: Arc<dyn EmbeddingProvider>,
        llm: Option<Arc<dyn akashic_llm::LlmProvider>>,
    ) -> Self {
        let doc_repo: Arc<dyn DocumentRepo> = Arc::new(PgDocumentRepo::new(pg.clone()));
        let doc_graph: Arc<dyn DocumentGraphRepo> =
            Arc::new(Neo4jDocumentGraphRepo::new(neo4j.clone()));
        Self {
            doc_repo,
            doc_graph,
            pg,
            neo4j,
            embedder,
            llm,
        }
    }

    /// Create a new document in PG and Neo4j, returning its UUID.
    pub async fn create_document(
        &self,
        repo_name: &str,
        title: &str,
        doc_type: &str,
        source_url: Option<&str>,
        version_coordinate: Option<&serde_json::Value>,
    ) -> Result<Uuid> {
        // Insert into PG via DocumentRepo port
        let doc_id = self
            .doc_repo
            .insert_document(repo_name, title, doc_type, source_url, version_coordinate)
            .await?;

        // Neo4j: MERGE Repository node, then create Document node and edge
        self.doc_graph
            .create_document_node(repo_name, doc_id, title, doc_type)
            .await?;

        info!(repo_name, title, doc_id = %doc_id, "Created document");
        Ok(doc_id)
    }

    /// Store hierarchical sections for a document, returning a flat list of (uuid, heading).
    pub fn store_sections<'a>(
        &'a self,
        doc_id: Uuid,
        repo_name: &'a str,
        doc_title: &'a str,
        sections: &'a [RawSection],
        parent_id: Option<Uuid>,
        version_coordinate: Option<&'a serde_json::Value>,
    ) -> futures::future::BoxFuture<'a, Result<Vec<(Uuid, String)>>> {
        Box::pin(async move {
            let mut result = Vec::new();

            for section in sections {
                // Build contextual embedding text
                let content_truncated: String = section.content.chars().take(512).collect();
                let embed_text = format!(
                    "In documentation '{}', section '{}': {}",
                    doc_title, section.heading, content_truncated
                );

                // Generate embedding
                let emb_resp = self.embedder.embed(&embed_text).await?;
                let embedding_raw = emb_resp.vector.clone(); // save for EXPLAINS

                // Insert into PG via DocumentRepo port
                let section_id = self
                    .doc_repo
                    .insert_section(
                        doc_id,
                        parent_id,
                        &section.heading,
                        &section.content,
                        section.depth as i16,
                        section.position as i16,
                        &section
                            .tags
                            .iter()
                            .map(|t| t.to_lowercase())
                            .collect::<Vec<_>>(),
                        &embedding_raw,
                        version_coordinate,
                    )
                    .await?;

                result.push((section_id, section.heading.clone()));

                // Neo4j: create Section node
                self.doc_graph
                    .create_section_node(section_id, doc_id, &section.heading, section.depth as i64)
                    .await?;

                // Neo4j: create parent relationship
                match parent_id {
                    Some(pid) => {
                        // Parent section -> child section
                        self.doc_graph
                            .create_has_subsection_edge(pid, section_id)
                            .await?;
                    }
                    None => {
                        // Document -> top-level section
                        self.doc_graph
                            .create_has_section_edge(doc_id, section_id)
                            .await?;
                    }
                }

                // Neo4j: create TAGGED_WITH edges for each tag
                for tag in &section.tags {
                    let tag_lower = tag.to_lowercase();
                    self.doc_graph
                        .create_tagged_with_edge(section_id, &tag_lower)
                        .await?;
                }

                // ── EXPLAINS edges ──────────────────────────────────────
                // Tier 1: deterministic (exact code-reference matches)
                let symbol_repo: Arc<dyn SymbolRepo> = Arc::new(PgSymbolRepo::new(self.pg.clone()));
                let edge_repo: Arc<dyn EdgeRepo> = Arc::new(Neo4jEdgeRepo::new(self.neo4j.clone()));
                let exact_count = explains::create_deterministic_explains_edges(
                    &symbol_repo,
                    &edge_repo,
                    section_id,
                    &section.content,
                    repo_name,
                )
                .await?;

                // Tier 2: LLM-verified (vector similarity + LLM confirmation)
                if exact_count == 0 && !section.content.is_empty() {
                    if let Some(ref llm) = self.llm {
                        let llm_count = explains::create_llm_verified_explains_edges(
                            &symbol_repo,
                            &edge_repo,
                            section_id,
                            &section.content,
                            &section.heading,
                            &embedding_raw,
                            repo_name,
                            llm.as_ref(),
                        )
                        .await?;
                        if llm_count > 0 {
                            tracing::info!(
                                section_id = %section_id,
                                edges = llm_count,
                                "Created LLM-verified EXPLAINS edges"
                            );
                        }
                    } else {
                        tracing::debug!(
                            section_id = %section_id,
                            heading = %section.heading,
                            "No deterministic EXPLAINS edges found, LLM not configured"
                        );
                    }
                }

                // Recurse into children
                if !section.children.is_empty() {
                    let child_results = self
                        .store_sections(
                            doc_id,
                            repo_name,
                            doc_title,
                            &section.children,
                            Some(section_id),
                            version_coordinate,
                        )
                        .await?;
                    result.extend(child_results);
                }
            }

            Ok(result)
        })
    }

    /// Delete all documents for a repo from PG (cascades to sections) and Neo4j.
    pub async fn clean_repo_docs(&self, repo_name: &str) -> Result<()> {
        // PG: delete documents (sections cascade via FK) via DocumentRepo port
        self.doc_repo.clean_repo_docs(repo_name).await?;

        // Neo4j: detach delete document nodes, their sections, and subsections
        self.doc_graph.clean_repo_docs(repo_name).await?;

        info!(repo_name, "Cleaned repo document data");
        Ok(())
    }

    /// Delete only `doc_type = 'corpus'` documents for a repo from PG
    /// (cascades to sections) and Neo4j, leaving any other doc_type (e.g.
    /// website-ingested) documents for the same repo untouched.
    ///
    /// Task 8 (B3): the derive job calls this before rebuilding a repo's
    /// corpus Doc space, so re-derivation is idempotent (no stale sections
    /// from a previous run) without touching documents another ingestion
    /// path created for the same `repo_name`.
    pub async fn clean_repo_corpus_docs(&self, repo_name: &str) -> Result<()> {
        self.doc_repo.clean_repo_corpus_docs(repo_name).await?;
        self.doc_graph.clean_repo_corpus_docs(repo_name).await?;

        info!(repo_name, "Cleaned repo corpus document data");
        Ok(())
    }
}
