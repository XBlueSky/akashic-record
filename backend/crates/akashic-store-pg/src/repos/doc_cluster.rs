//! PostgreSQL adapter for `DocClusterRepo`.
//!
//! All SQL is moved verbatim from the original call-sites:
//! - `akashic-ingestion::ingestion::doc_clustering` (`fetch_section_embeddings`,
//!   `clean_clusters`, `store_cluster`, `assign_sections_to_cluster`)

use anyhow::Result;
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::DocClusterRepo;
use akashic_domain::types::{DocClusterRow, SectionEmbeddingRow};

/// PostgreSQL adapter implementing [`DocClusterRepo`].
#[derive(Clone)]
pub struct PgDocClusterRepo {
    pool: PgPool,
}

impl PgDocClusterRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DocClusterRepo for PgDocClusterRepo {
    async fn fetch_section_embeddings(&self, repo_name: &str) -> Result<Vec<SectionEmbeddingRow>> {
        let rows: Vec<(Uuid, String, pgvector::Vector)> = sqlx::query_as(
            "SELECT s.id, s.heading, s.embedding \
             FROM sections s JOIN documents d ON s.doc_id = d.id \
             WHERE d.repo_name = $1 AND s.embedding IS NOT NULL",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, heading, emb)| SectionEmbeddingRow {
                id,
                heading,
                embedding: emb.to_vec(),
            })
            .collect())
    }

    async fn clean_clusters(&self, repo_name: &str) -> Result<()> {
        // Remove cluster assignments from sections
        sqlx::query(
            "UPDATE sections SET cluster_id = NULL \
             WHERE cluster_id IN (SELECT id FROM doc_clusters WHERE repo_name = $1)",
        )
        .bind(repo_name)
        .execute(&self.pool)
        .await?;

        // Delete clusters from PG
        sqlx::query("DELETE FROM doc_clusters WHERE repo_name = $1")
            .bind(repo_name)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    async fn store_cluster(
        &self,
        repo_name: &str,
        name: &str,
        section_count: i32,
        centroid: &[f32],
    ) -> Result<Uuid> {
        let vec = pgvector::Vector::from(centroid.to_vec());
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO doc_clusters (repo_name, name, section_count, embedding) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (repo_name, name) DO UPDATE SET \
               section_count = EXCLUDED.section_count, \
               embedding = EXCLUDED.embedding \
             RETURNING id",
        )
        .bind(repo_name)
        .bind(name)
        .bind(section_count)
        .bind(vec)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    async fn assign_sections_to_cluster(
        &self,
        cluster_id: Uuid,
        section_ids: &[Uuid],
    ) -> Result<()> {
        sqlx::query("UPDATE sections SET cluster_id = $1 WHERE id = ANY($2)")
            .bind(cluster_id)
            .bind(section_ids)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn list_clusters_by_repo(&self, repo_name: &str) -> Result<Vec<DocClusterRow>> {
        // SQL moved verbatim from graph/doc.rs get_doc_graph.
        let rows: Vec<(Uuid, String, i32)> =
            sqlx::query_as("SELECT id, name, section_count FROM doc_clusters WHERE repo_name = $1")
                .bind(repo_name)
                .fetch_all(&self.pool)
                .await?;

        Ok(rows
            .into_iter()
            .map(|(id, name, section_count)| DocClusterRow {
                id,
                name,
                section_count,
                repo_name: Some(repo_name.to_string()),
            })
            .collect())
    }

    async fn get_cluster_by_id(&self, cluster_id: Uuid) -> Result<Option<DocClusterRow>> {
        // SQL moved verbatim from graph/doc.rs get_cluster_detail.
        let row: Option<(Uuid, String, i32, String)> = sqlx::query_as(
            "SELECT id, name, section_count, repo_name FROM doc_clusters WHERE id = $1",
        )
        .bind(cluster_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(id, name, section_count, repo)| DocClusterRow {
            id,
            name,
            section_count,
            repo_name: Some(repo),
        }))
    }
}
