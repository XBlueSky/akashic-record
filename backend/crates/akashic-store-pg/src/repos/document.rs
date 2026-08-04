//! PostgreSQL adapter for `DocumentRepo`.
//!
//! All SQL is moved verbatim from the original call-sites:
//! - `akashic-ingestion::ingestion::doc_store` (`create_document`, `store_sections`,
//!   `clean_repo_docs`)

use anyhow::{Context, Result};
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::DocumentRepo;
use akashic_domain::types::{
    DocumentMetaRow, RelinkSectionRow, ScoutSectionRow, SectionContentRow, SectionDetailRow,
    SectionRow, SectionSearchRow,
};

/// PostgreSQL adapter implementing [`DocumentRepo`].
#[derive(Clone)]
pub struct PgDocumentRepo {
    pool: PgPool,
}

impl PgDocumentRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DocumentRepo for PgDocumentRepo {
    async fn insert_document(
        &self,
        repo_name: &str,
        title: &str,
        doc_type: &str,
        source_url: Option<&str>,
        version_coordinate: Option<&serde_json::Value>,
    ) -> Result<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO documents (repo_name, title, doc_type, source_url, version_coordinate) \
             VALUES ($1, $2, $3, $4, $5) \
             RETURNING id",
        )
        .bind(repo_name)
        .bind(title)
        .bind(doc_type)
        .bind(source_url)
        .bind(version_coordinate.map(sqlx::types::Json))
        .fetch_one(&self.pool)
        .await
        .context("Failed to insert document into PG")?;
        Ok(row.0)
    }

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
    ) -> Result<Uuid> {
        let vec = pgvector::Vector::from(embedding.to_vec());
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO sections (doc_id, parent_id, heading, content, depth, position, tags, embedding, version_coordinate) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             RETURNING id",
        )
        .bind(doc_id)
        .bind(parent_id)
        .bind(heading)
        .bind(content)
        .bind(depth)
        .bind(position)
        .bind(tags)
        .bind(vec)
        .bind(version_coordinate.map(sqlx::types::Json))
        .fetch_one(&self.pool)
        .await
        .context("Failed to insert section into PG")?;
        Ok(row.0)
    }

    async fn clean_repo_docs(&self, repo_name: &str) -> Result<()> {
        sqlx::query("DELETE FROM documents WHERE repo_name = $1")
            .bind(repo_name)
            .execute(&self.pool)
            .await
            .context("Failed to clean repo documents from PG")?;
        Ok(())
    }

    async fn clean_repo_corpus_docs(&self, repo_name: &str) -> Result<()> {
        sqlx::query("DELETE FROM documents WHERE repo_name = $1 AND doc_type = 'corpus'")
            .bind(repo_name)
            .execute(&self.pool)
            .await
            .context("Failed to clean repo corpus documents from PG")?;
        Ok(())
    }

    async fn search_sections_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SectionSearchRow>> {
        // SQL moved verbatim from akashic-retrieval::graphrag::mod.rs search_doc_space.
        let vec = pgvector::Vector::from(query_vec.to_vec());
        let rows: Vec<(Uuid, String, f64)> = if let Some(r) = repo {
            sqlx::query_as(
                "SELECT s.id, s.heading, 1 - (s.embedding <=> $1::vector) AS score \
                 FROM sections s JOIN documents d ON s.doc_id = d.id \
                 WHERE d.repo_name = $2 \
                 ORDER BY s.embedding <=> $1::vector LIMIT $3",
            )
            .bind(&vec)
            .bind(r)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, heading, 1 - (embedding <=> $1::vector) AS score \
                 FROM sections ORDER BY embedding <=> $1::vector LIMIT $2",
            )
            .bind(&vec)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows
            .into_iter()
            .map(|(id, heading, score)| SectionSearchRow {
                id,
                heading,
                score: score as f32,
            })
            .collect())
    }

    async fn scout_search_sections_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ScoutSectionRow>> {
        // SQL moved verbatim from akashic-server::api::routes::search::unified_search
        // (doc layer): always joins `documents` for title + repo name, single
        // `$2::text IS NULL OR d.repo_name = $2` repo filter. `d.doc_type` and
        // `d.source_url` (D5/C3 supplements) let the search service decide
        // whether this hit backs a docs-corpus deep link.
        let vec = pgvector::Vector::from(query_vec.to_vec());
        let rows: Vec<(Uuid, String, String, String, String, Option<String>, f64)> =
            sqlx::query_as(
                "SELECT s.id, s.heading, d.title, d.repo_name, d.doc_type, d.source_url, \
                        1 - (s.embedding <=> $1::vector) AS score \
                 FROM sections s JOIN documents d ON s.doc_id = d.id \
                 WHERE ($2::text IS NULL OR d.repo_name = $2) \
                 ORDER BY s.embedding <=> $1::vector \
                 LIMIT $3",
            )
            .bind(&vec)
            .bind(repo)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, heading, doc_title, repo_name, doc_type, source_url, score)| {
                    ScoutSectionRow {
                        id,
                        heading,
                        doc_title,
                        repo_name,
                        score,
                        doc_type,
                        source_url,
                    }
                },
            )
            .collect())
    }

    async fn search_sections_by_bm25(
        &self,
        query: &str,
        repo: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SectionSearchRow>> {
        // SQL moved verbatim from akashic-retrieval::graphrag::mod.rs search_doc_bm25.
        let rows: Vec<(Uuid, String, f64)> = sqlx::query_as(
            "SELECT s.id, s.heading, \
                    ts_rank(s.content_tsv, plainto_tsquery('english', $1))::float8 AS score \
             FROM sections s JOIN documents d ON s.doc_id = d.id \
             WHERE s.content_tsv @@ plainto_tsquery('english', $1) \
               AND ($2::text IS NULL OR d.repo_name = $2) \
             ORDER BY score DESC LIMIT $3",
        )
        .bind(query)
        .bind(repo)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, heading, score)| SectionSearchRow {
                id,
                heading,
                score: score as f32,
            })
            .collect())
    }

    async fn fetch_section_content(&self, section_id: Uuid) -> Result<Option<SectionContentRow>> {
        // SQL moved verbatim from akashic-retrieval::graphrag::mod.rs fetch_full_node
        // Doc branch.
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT s.content, d.title \
             FROM sections s JOIN documents d ON s.doc_id = d.id \
             WHERE s.id = $1",
        )
        .bind(section_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(content, doc_title)| SectionContentRow { content, doc_title }))
    }

    async fn get_document_by_id(&self, doc_id: Uuid) -> Result<Option<DocumentMetaRow>> {
        // SQL moved verbatim from graph/doc.rs get_document_detail.
        let row: Option<(Uuid, String, String, Option<String>)> =
            sqlx::query_as("SELECT id, title, doc_type, source_url FROM documents WHERE id = $1")
                .bind(doc_id)
                .fetch_optional(&self.pool)
                .await?;

        Ok(
            row.map(|(id, title, doc_type, source_url)| DocumentMetaRow {
                id,
                title,
                doc_type,
                source_url,
            }),
        )
    }

    async fn get_document_id_by_source(
        &self,
        repo_name: &str,
        doc_type: &str,
        source_url: &str,
    ) -> Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM documents WHERE repo_name = $1 AND doc_type = $2 AND source_url = $3",
        )
        .bind(repo_name)
        .bind(doc_type)
        .bind(source_url)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(id,)| id))
    }

    async fn get_sections_by_doc(&self, doc_id: Uuid) -> Result<Vec<SectionRow>> {
        // SQL moved verbatim from graph/doc.rs get_document_detail.
        let rows: Vec<(Uuid, Option<Uuid>, String, String, i16, Option<Vec<String>>)> =
            sqlx::query_as(
                "SELECT id, parent_id, heading, content, depth, tags \
                 FROM sections WHERE doc_id = $1 ORDER BY depth, position",
            )
            .bind(doc_id)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, parent_id, heading, content, depth, tags)| SectionRow {
                    id,
                    parent_id,
                    heading,
                    content,
                    depth,
                    tags,
                },
            )
            .collect())
    }

    async fn get_sections_by_cluster(&self, cluster_id: Uuid) -> Result<Vec<SectionRow>> {
        // SQL moved verbatim from graph/doc.rs get_cluster_detail.
        let rows: Vec<(Uuid, Option<Uuid>, String, String, i16, Option<Vec<String>>)> =
            sqlx::query_as(
                "SELECT id, parent_id, heading, content, depth, tags \
                 FROM sections WHERE cluster_id = $1 ORDER BY depth, position",
            )
            .bind(cluster_id)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, parent_id, heading, content, depth, tags)| SectionRow {
                    id,
                    parent_id,
                    heading,
                    content,
                    depth,
                    tags,
                },
            )
            .collect())
    }

    async fn count_sections_per_doc(&self, doc_ids: &[Uuid]) -> Result<Vec<(Uuid, i64)>> {
        if doc_ids.is_empty() {
            return Ok(Vec::new());
        }
        // SQL moved verbatim from graph/doc.rs get_doc_graph_documents.
        let rows: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT doc_id, COUNT(*) FROM sections WHERE doc_id = ANY($1) GROUP BY doc_id",
        )
        .bind(doc_ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn get_source_urls_by_repo(
        &self,
        repo_name: &str,
    ) -> Result<Vec<(Uuid, Option<String>)>> {
        // SQL moved verbatim from graph/doc.rs get_doc_graph_documents.
        let rows: Vec<(Uuid, Option<String>)> =
            sqlx::query_as("SELECT id, source_url FROM documents WHERE repo_name = $1")
                .bind(repo_name)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    async fn fetch_sections_detail_batch(
        &self,
        section_ids: &[Uuid],
    ) -> Result<Vec<SectionDetailRow>> {
        if section_ids.is_empty() {
            return Ok(Vec::new());
        }
        // SQL moved verbatim from search.rs details handler.
        let rows: Vec<(Uuid, String, String, String, i32, Option<Vec<String>>)> = sqlx::query_as(
            "SELECT s.id, s.heading, s.content, d.title, s.depth, s.tags \
             FROM sections s JOIN documents d ON s.doc_id = d.id \
             WHERE s.id = ANY($1)",
        )
        .bind(section_ids)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, heading, content, document_title, depth, tags)| SectionDetailRow {
                    id,
                    heading,
                    content,
                    document_title,
                    depth,
                    tags,
                },
            )
            .collect())
    }

    async fn fetch_sections_for_relink(&self, repo_name: &str) -> Result<Vec<RelinkSectionRow>> {
        // SQL moved verbatim from `POST /api/v1/repos/:name/relink-explains` handler.
        let rows: Vec<(Uuid, String, String, Option<pgvector::Vector>)> = sqlx::query_as(
            "SELECT s.id, s.heading, s.content, s.embedding \
             FROM sections s JOIN documents d ON s.doc_id = d.id \
             WHERE d.repo_name = $1",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, heading, content, embedding)| RelinkSectionRow {
                id,
                heading,
                content,
                embedding: embedding.map(|v| v.to_vec()),
            })
            .collect())
    }
}
