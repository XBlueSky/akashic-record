//! Port traits for document / section persistence.
//!
//! `DocumentRepo` covers the PostgreSQL side (documents + sections tables).
//! `DocumentGraphRepo` covers the Neo4j side (Document/Section nodes and edges).
//!
//! Both traits are infra-free: they use only domain types from
//! `akashic_domain::types`.  Adapter implementations live in
//! `akashic-store-pg` and `akashic-store-neo4j` respectively.

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::{
    DocumentMetaRow, RelinkSectionRow, ScoutSectionRow, SectionContentRow, SectionDetailRow,
    SectionRow, SectionSearchRow,
};

// ── DocumentRepo ──────────────────────────────────────────────────────────────

/// Repository contract for the `documents` and `sections` PG tables.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn DocumentRepo>`.
#[async_trait]
pub trait DocumentRepo: Send + Sync {
    // ── Write ────────────────────────────────────────────────────────────────

    /// Insert a new document row and return its UUID.
    ///
    /// SQL moved verbatim from `akashic-ingestion::ingestion::doc_store::DocStore::create_document`.
    async fn insert_document(
        &self,
        repo_name: &str,
        title: &str,
        doc_type: &str,
        source_url: Option<&str>,
        version_coordinate: Option<&serde_json::Value>,
    ) -> anyhow::Result<Uuid>;

    /// Insert a new section row and return its UUID.
    ///
    /// The embedding is pre-computed by the caller (DocStore) as `Vec<f32>`.
    /// The adapter converts it to `pgvector::Vector`.
    ///
    /// SQL moved verbatim from `akashic-ingestion::ingestion::doc_store::DocStore::store_sections`.
    #[allow(clippy::too_many_arguments)]
    async fn insert_section(
        &self,
        doc_id: Uuid,
        parent_id: Option<Uuid>,
        heading: &str,
        content: &str,
        depth: i16,
        position: i16,
        tags: &[String],
        embedding: &[f32],
        version_coordinate: Option<&serde_json::Value>,
    ) -> anyhow::Result<Uuid>;

    /// Delete all documents for a repo from PG (sections cascade via FK).
    ///
    /// SQL moved verbatim from `akashic-ingestion::ingestion::doc_store::DocStore::clean_repo_docs`.
    async fn clean_repo_docs(&self, repo_name: &str) -> anyhow::Result<()>;

    /// Delete only `doc_type = 'corpus'` documents for a repo (sections
    /// cascade via FK), leaving any other doc_type (e.g. website-ingested)
    /// documents for the same repo untouched.
    ///
    /// Task 8 (B3) isolation requirement: the derive job rebuilds a repo's
    /// corpus-sourced Doc space from scratch on every run, but must not wipe
    /// documents another ingestion path (website crawl) created for the same
    /// `repo_name` — `clean_repo_docs` is too broad for that.
    async fn clean_repo_corpus_docs(&self, repo_name: &str) -> anyhow::Result<()>;

    /// Vector (cosine) search over section embeddings.
    ///
    /// SQL moved verbatim from `akashic-retrieval::graphrag::mod.rs`
    /// `search_doc_space` (A1 Task 7).
    async fn search_sections_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<SectionSearchRow>>;

    /// Scout vector search returning the display columns (section heading,
    /// document title, per-row repo name) the unified `GET /api/v1/search`
    /// endpoint needs.
    ///
    /// SQL moved verbatim from `akashic-server::api::routes::search::unified_search`
    /// (doc layer). Always joins `documents` (for title + repo name), unlike the
    /// GraphRAG [`search_sections_by_vector`] no-repo branch.
    async fn scout_search_sections_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<ScoutSectionRow>>;

    /// BM25 full-text search over sections.
    ///
    /// SQL moved verbatim from `akashic-retrieval::graphrag::mod.rs`
    /// `search_doc_bm25` (A1 Task 7).
    async fn search_sections_by_bm25(
        &self,
        query: &str,
        repo: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<SectionSearchRow>>;

    /// Fetch (content, doc_title) for a single section by id.
    ///
    /// SQL moved verbatim from `akashic-retrieval::graphrag::mod.rs`
    /// `fetch_full_node` Doc branch (A1 Task 7).
    async fn fetch_section_content(
        &self,
        section_id: Uuid,
    ) -> anyhow::Result<Option<SectionContentRow>>;

    /// Fetch document metadata (title, doc_type, source_url) by id.
    ///
    /// SQL: `SELECT id, title, doc_type, source_url FROM documents WHERE id = $1`.
    async fn get_document_by_id(&self, doc_id: Uuid) -> anyhow::Result<Option<DocumentMetaRow>>;

    /// Fetch a document's id by its exact `(repo_name, doc_type, source_url)`
    /// key.
    ///
    /// This is the key corpus-derive's `create_document` writes for a corpus
    /// page (`doc_type = "corpus"`, `source_url = <corpus file path>`; see
    /// `ingestion::corpus_derive::run_derive_inner`). Used by the docs-read
    /// `page` endpoint (Task 10) to attach `document_id` once a corpus
    /// version has been derived.
    ///
    /// SQL: `SELECT id FROM documents WHERE repo_name = $1 AND doc_type = $2
    /// AND source_url = $3`.
    async fn get_document_id_by_source(
        &self,
        repo_name: &str,
        doc_type: &str,
        source_url: &str,
    ) -> anyhow::Result<Option<Uuid>>;

    /// Fetch all sections for a document, ordered by depth, position.
    ///
    /// SQL: `SELECT id, parent_id, heading, content, depth, tags
    ///       FROM sections WHERE doc_id = $1 ORDER BY depth, position`.
    async fn get_sections_by_doc(&self, doc_id: Uuid) -> anyhow::Result<Vec<SectionRow>>;

    /// Fetch all sections for a cluster, ordered by depth, position.
    ///
    /// SQL: `SELECT id, parent_id, heading, content, depth, tags
    ///       FROM sections WHERE cluster_id = $1 ORDER BY depth, position`.
    async fn get_sections_by_cluster(&self, cluster_id: Uuid) -> anyhow::Result<Vec<SectionRow>>;

    /// Count sections per doc_id for a set of doc ids.
    ///
    /// SQL: `SELECT doc_id, COUNT(*) FROM sections WHERE doc_id = ANY($1) GROUP BY doc_id`.
    /// Used by GraphService::get_doc_graph (document fallback).
    async fn count_sections_per_doc(&self, doc_ids: &[Uuid]) -> anyhow::Result<Vec<(Uuid, i64)>>;

    /// Fetch (id, source_url) for all documents in a repo.
    ///
    /// SQL: `SELECT id, source_url FROM documents WHERE repo_name = $1`.
    async fn get_source_urls_by_repo(
        &self,
        repo_name: &str,
    ) -> anyhow::Result<Vec<(Uuid, Option<String>)>>;

    /// Batch-fetch section rows with document title for the `get_details` endpoint.
    ///
    /// SQL (verbatim from `GET /api/v1/details` handler):
    /// `SELECT s.id, s.heading, s.content, d.title, s.depth, s.tags
    ///  FROM sections s JOIN documents d ON s.doc_id = d.id
    ///  WHERE s.id = ANY($1)`.
    async fn fetch_sections_detail_batch(
        &self,
        section_ids: &[Uuid],
    ) -> anyhow::Result<Vec<SectionDetailRow>>;

    /// Fetch all sections for a repo with their content and embeddings.
    ///
    /// SQL (verbatim from `POST /api/v1/repos/:name/relink-explains` handler):
    /// `SELECT s.id, s.heading, s.content, s.embedding`
    /// `FROM sections s JOIN documents d ON s.doc_id = d.id`
    /// `WHERE d.repo_name = $1`
    ///
    /// Used by `SearchService::relink_explains`.
    async fn fetch_sections_for_relink(
        &self,
        repo_name: &str,
    ) -> anyhow::Result<Vec<RelinkSectionRow>>;
}

// ── DocumentGraphRepo ─────────────────────────────────────────────────────────

/// Repository contract for Document and Section nodes/edges in Neo4j.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn DocumentGraphRepo>`.
#[async_trait]
pub trait DocumentGraphRepo: Send + Sync {
    // ── Document node ────────────────────────────────────────────────────────

    /// MERGE a Repository node and a Document node; create BELONGS_TO edge.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::ingestion::doc_store::DocStore::create_document`.
    async fn create_document_node(
        &self,
        repo_name: &str,
        doc_pg_id: Uuid,
        title: &str,
        doc_type: &str,
    ) -> anyhow::Result<()>;

    // ── Section nodes ─────────────────────────────────────────────────────────

    /// MERGE a Section node, setting doc_id, heading, and depth.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::ingestion::doc_store::DocStore::store_sections`.
    async fn create_section_node(
        &self,
        section_pg_id: Uuid,
        doc_pg_id: Uuid,
        heading: &str,
        depth: i64,
    ) -> anyhow::Result<()>;

    /// MATCH parent Section and child Section; MERGE HAS_SUBSECTION edge.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::ingestion::doc_store::DocStore::store_sections`.
    async fn create_has_subsection_edge(
        &self,
        parent_pg_id: Uuid,
        child_pg_id: Uuid,
    ) -> anyhow::Result<()>;

    /// MATCH Document and top-level Section; MERGE HAS_SECTION edge.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::ingestion::doc_store::DocStore::store_sections`.
    async fn create_has_section_edge(
        &self,
        doc_pg_id: Uuid,
        section_pg_id: Uuid,
    ) -> anyhow::Result<()>;

    /// MERGE Tag node and MERGE TAGGED_WITH edge from Section.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::ingestion::doc_store::DocStore::store_sections`.
    async fn create_tagged_with_edge(&self, section_pg_id: Uuid, tag: &str) -> anyhow::Result<()>;

    // ── Cleanup ───────────────────────────────────────────────────────────────

    /// DETACH DELETE all Document nodes (and their sections) for a repo.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::ingestion::doc_store::DocStore::clean_repo_docs`.
    async fn clean_repo_docs(&self, repo_name: &str) -> anyhow::Result<()>;

    /// DETACH DELETE only `doc_type = 'corpus'` Document nodes (and their
    /// sections) for a repo. See [`DocumentRepo::clean_repo_corpus_docs`]
    /// for the isolation rationale (Task 8, B3).
    async fn clean_repo_corpus_docs(&self, repo_name: &str) -> anyhow::Result<()>;
}
