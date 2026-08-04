//! Port traits for doc-cluster persistence.
//!
//! `DocClusterRepo` covers the PostgreSQL side (doc_clusters table + section
//! cluster_id updates).  `DocClusterGraphRepo` covers the Neo4j side
//! (TopicCluster nodes and IN_CLUSTER edges).
//!
//! Both traits are infra-free: they use only domain types from
//! `akashic_domain::types`.  Adapter implementations live in
//! `akashic-store-pg` and `akashic-store-neo4j` respectively.

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::{DocClusterRow, SectionEmbeddingRow};

// ── DocClusterRepo ────────────────────────────────────────────────────────────

/// Repository contract for the `doc_clusters` PG table and related section updates.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn DocClusterRepo>`.
#[async_trait]
pub trait DocClusterRepo: Send + Sync {
    // ── Read ─────────────────────────────────────────────────────────────────

    /// List all doc_cluster rows for a repo.
    ///
    /// SQL moved verbatim from `akashic-server::api::routes::graph::doc::get_doc_graph`:
    /// `SELECT id, name, section_count FROM doc_clusters WHERE repo_name = $1`.
    async fn list_clusters_by_repo(&self, repo_name: &str) -> anyhow::Result<Vec<DocClusterRow>>;

    /// Fetch cluster metadata by id (for get_cluster_detail).
    ///
    /// SQL: `SELECT id, name, section_count, repo_name FROM doc_clusters WHERE id = $1`.
    async fn get_cluster_by_id(&self, cluster_id: Uuid) -> anyhow::Result<Option<DocClusterRow>>;

    /// Fetch all section ids, headings, and embeddings for a repo.
    ///
    /// SQL moved verbatim from `akashic-ingestion::ingestion::doc_clustering::DocClustering::fetch_section_embeddings`.
    async fn fetch_section_embeddings(
        &self,
        repo_name: &str,
    ) -> anyhow::Result<Vec<SectionEmbeddingRow>>;

    // ── Write ────────────────────────────────────────────────────────────────

    /// Null out cluster_id on all sections belonging to clusters for this repo,
    /// then delete the cluster rows.
    ///
    /// SQL moved verbatim from `akashic-ingestion::ingestion::doc_clustering::DocClustering::clean_clusters`.
    async fn clean_clusters(&self, repo_name: &str) -> anyhow::Result<()>;

    /// Upsert a doc_cluster row (ON CONFLICT repo_name, name) and return its UUID.
    ///
    /// The centroid embedding is pre-computed by the caller as `Vec<f32>`.
    /// The adapter converts it to `pgvector::Vector`.
    ///
    /// SQL moved verbatim from `akashic-ingestion::ingestion::doc_clustering::DocClustering::store_cluster`.
    async fn store_cluster(
        &self,
        repo_name: &str,
        name: &str,
        section_count: i32,
        centroid: &[f32],
    ) -> anyhow::Result<Uuid>;

    /// Set cluster_id = $1 on all sections whose id is in section_ids.
    ///
    /// SQL moved verbatim from `akashic-ingestion::ingestion::doc_clustering::DocClustering::assign_sections_to_cluster`.
    async fn assign_sections_to_cluster(
        &self,
        cluster_id: Uuid,
        section_ids: &[Uuid],
    ) -> anyhow::Result<()>;
}

// ── DocClusterGraphRepo ───────────────────────────────────────────────────────

/// Repository contract for TopicCluster nodes and IN_CLUSTER edges in Neo4j.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn DocClusterGraphRepo>`.
#[async_trait]
pub trait DocClusterGraphRepo: Send + Sync {
    // ── Write ────────────────────────────────────────────────────────────────

    /// DETACH DELETE all TopicCluster nodes for a repo.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::ingestion::doc_clustering::DocClustering::clean_clusters`.
    async fn delete_clusters_by_repo(&self, repo_name: &str) -> anyhow::Result<()>;

    /// MERGE TopicCluster node and BELONGS_TO edge to Repository.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::ingestion::doc_clustering::DocClustering::create_neo4j_cluster`.
    async fn create_cluster_node(
        &self,
        cluster_pg_id: Uuid,
        name: &str,
        repo_name: &str,
    ) -> anyhow::Result<()>;

    /// MERGE IN_CLUSTER edge from Section to TopicCluster.
    ///
    /// Cypher moved verbatim from `akashic-ingestion::ingestion::doc_clustering::DocClustering::create_neo4j_cluster`.
    async fn create_in_cluster_edge(
        &self,
        section_pg_id: Uuid,
        cluster_pg_id: Uuid,
    ) -> anyhow::Result<()>;
}
