//! PostgreSQL adapter for `ChunkRepo`.
//!
//! All SQL is moved verbatim from the original call-sites:
//! - `akashic-ingestion::ingestion::store.rs`
//! - `akashic-server::api::routes::modules.rs`
//! - `akashic-retrieval::graphrag::mod.rs`
//! - `akashic-curation::notes::memory_stack.rs`
//! - `akashic-curation::notes::health.rs`

use anyhow::{Context, Result};
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::ChunkRepo;
use akashic_domain::types::{
    ChunkCallResolutionRow, ChunkContentRow, ChunkDefinition, ChunkDetailRow, ChunkFullRow,
    ChunkModuleRow, ChunkRow, ChunkSearchRow, EntryPointChunkRow, GoChunkRow, LanguageCount,
    LargeChunkContentRow, ScoutChunkRow,
};

/// PostgreSQL adapter implementing [`ChunkRepo`].
#[derive(Clone)]
pub struct PgChunkRepo {
    pool: PgPool,
}

impl PgChunkRepo {
    /// Construct a new adapter from a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ChunkRepo for PgChunkRepo {
    // ── Write ──────────────────────────────────────────────────────────────

    async fn store_chunk_row(
        &self,
        repo_name: &str,
        module_path: &str,
        chunk_type: &str,
        name: &str,
        signature: Option<&str>,
        content: &str,
        language: &str,
        embedding: &[f32],
        signature_embedding: Option<&[f32]>,
        git_ref: &str,
        fqn: Option<&str>,
        parent_fqn: Option<&str>,
        start_line: i32,
        end_line: i32,
        visibility: &str,
        is_async: bool,
        is_static: bool,
        is_exported: bool,
        doc: Option<&str>,
    ) -> Result<Uuid> {
        let vec = pgvector::Vector::from(embedding.to_vec());
        let sig_vec = signature_embedding.map(|s| pgvector::Vector::from(s.to_vec()));

        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO chunks \
             (repo_name, module_path, chunk_type, name, signature, content, language, \
              embedding, signature_embedding, git_ref, \
              fqn, parent_fqn, start_line, end_line, visibility, \
              is_async, is_static, is_exported, doc) \
             VALUES \
             ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
              $11, $12, $13, $14, $15, $16, $17, $18, $19) \
             RETURNING id",
        )
        .bind(repo_name)
        .bind(module_path)
        .bind(chunk_type)
        .bind(name)
        .bind(signature)
        .bind(content)
        .bind(language)
        .bind(vec)
        .bind(sig_vec)
        .bind(git_ref)
        .bind(fqn)
        .bind(parent_fqn)
        .bind(start_line)
        .bind(end_line)
        .bind(visibility)
        .bind(is_async)
        .bind(is_static)
        .bind(is_exported)
        .bind(doc)
        .fetch_one(&self.pool)
        .await
        .context("Failed to insert chunk into PG")?;

        Ok(row.0)
    }

    async fn insert_large_chunk(
        &self,
        repo_name: &str,
        module_path: &str,
        chunk_ids: &[Uuid],
        content: &str,
        embedding: &[f32],
        git_ref: &str,
    ) -> Result<Uuid> {
        let vec = pgvector::Vector::from(embedding.to_vec());

        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO large_chunks (repo_name, module_path, chunk_ids, content, embedding, git_ref) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING id",
        )
        .bind(repo_name)
        .bind(module_path)
        .bind(chunk_ids)
        .bind(content)
        .bind(vec)
        .bind(git_ref)
        .fetch_one(&self.pool)
        .await
        .context("Failed to insert large chunk")?;

        Ok(row.0)
    }

    // ── Delete ──────────────────────────────────────────────────────────────

    async fn clean_chunks(&self, repo_name: &str) -> Result<()> {
        sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
            .bind(repo_name)
            .execute(&self.pool)
            .await
            .context("Failed to clean old chunks")?;
        Ok(())
    }

    async fn clean_large_chunks(&self, repo_name: &str) -> Result<()> {
        sqlx::query("DELETE FROM large_chunks WHERE repo_name = $1")
            .bind(repo_name)
            .execute(&self.pool)
            .await
            .context("Failed to clean old large_chunks")?;
        Ok(())
    }

    async fn clean_module_chunks(&self, repo_name: &str, module_path: &str) -> Result<()> {
        sqlx::query("DELETE FROM chunks WHERE repo_name = $1 AND module_path = $2")
            .bind(repo_name)
            .bind(module_path)
            .execute(&self.pool)
            .await
            .context("Failed to clean chunks for module")?;
        Ok(())
    }

    async fn clean_module_large_chunks(&self, repo_name: &str, module_path: &str) -> Result<()> {
        sqlx::query("DELETE FROM large_chunks WHERE repo_name = $1 AND module_path = $2")
            .bind(repo_name)
            .bind(module_path)
            .execute(&self.pool)
            .await
            .context("Failed to clean large_chunks for module")?;
        Ok(())
    }

    // ── Read ──────────────────────────────────────────────────────────────

    async fn get_chunk_detail(
        &self,
        repo_name: &str,
        chunk_id: Uuid,
    ) -> Result<Option<ChunkDetailRow>> {
        let row: Option<(
            Uuid,
            String,
            String,
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, repo_name, module_path, name, chunk_type, signature, \
                    content, language, git_ref, \
                    to_char(ingested_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
             FROM chunks WHERE id = $1 AND repo_name = $2",
        )
        .bind(chunk_id)
        .bind(repo_name)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(
            |(
                id,
                repo_name,
                module_path,
                name,
                chunk_type,
                signature,
                content,
                language,
                git_ref,
                ingested_at,
            )| {
                ChunkDetailRow {
                    id,
                    repo_name,
                    module_path,
                    name,
                    chunk_type,
                    signature,
                    content,
                    language,
                    git_ref,
                    ingested_at,
                }
            },
        ))
    }

    async fn list_chunks_in_module(
        &self,
        repo_name: &str,
        module_path: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ChunkRow>> {
        let rows: Vec<(Uuid, String, String, Option<String>, String, Option<String>)> =
            sqlx::query_as(
                "SELECT id, name, chunk_type, signature, content, language \
                 FROM chunks WHERE repo_name = $1 AND module_path = $2 \
                 ORDER BY chunk_type, name \
                 LIMIT $3 OFFSET $4",
            )
            .bind(repo_name)
            .bind(module_path)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, name, chunk_type, signature, content, language)| ChunkRow {
                    id,
                    name,
                    chunk_type,
                    signature,
                    content,
                    language,
                    // This paginated HTTP listing has no need for fqn/parent_fqn
                    // (only the ingestion-pipeline caller of
                    // `list_chunks_in_module_all`, below, does) — left `None`
                    // rather than widening this SELECT for an unused column.
                    fqn: None,
                    parent_fqn: None,
                },
            )
            .collect())
    }

    async fn search_chunks_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ChunkSearchRow>> {
        let vec = pgvector::Vector::from(query_vec.to_vec());
        let rows: Vec<(Uuid, String, String, f64)> = if let Some(r) = repo {
            sqlx::query_as(
                "SELECT id, name, chunk_type, 1 - (embedding <=> $1::vector) AS score \
                 FROM chunks WHERE repo_name = $2 \
                 ORDER BY embedding <=> $1::vector LIMIT $3",
            )
            .bind(&vec)
            .bind(r)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, name, chunk_type, 1 - (embedding <=> $1::vector) AS score \
                 FROM chunks ORDER BY embedding <=> $1::vector LIMIT $2",
            )
            .bind(&vec)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows
            .into_iter()
            .map(|(id, name, chunk_type, score)| ChunkSearchRow {
                id,
                name,
                chunk_type,
                score: score as f32,
            })
            .collect())
    }

    async fn scout_search_chunks_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ScoutChunkRow>> {
        // SQL moved verbatim from akashic-server::api::routes::search::unified_search
        // (chunk layer): single `$2::text IS NULL OR repo_name = $2` repo filter,
        // no archived/large distinction, score = cosine similarity.
        let vec = pgvector::Vector::from(query_vec.to_vec());
        let rows: Vec<(Uuid, String, String, String, String, f64)> = sqlx::query_as(
            "SELECT id, name, chunk_type, module_path, repo_name, \
                    1 - (embedding <=> $1::vector) AS score \
             FROM chunks \
             WHERE ($2::text IS NULL OR repo_name = $2) \
             ORDER BY embedding <=> $1::vector \
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
                |(id, name, chunk_type, module_path, repo_name, score)| ScoutChunkRow {
                    id,
                    name,
                    chunk_type,
                    module_path,
                    repo_name,
                    score,
                },
            )
            .collect())
    }

    async fn search_chunks_by_bm25(
        &self,
        query: &str,
        repo: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ChunkSearchRow>> {
        let rows: Vec<(Uuid, String, String, f64)> = sqlx::query_as(
            "SELECT id, name, chunk_type, \
                    ts_rank(content_tsv, plainto_tsquery('english', $1))::float8 AS score \
             FROM chunks \
             WHERE content_tsv @@ plainto_tsquery('english', $1) \
               AND ($2::text IS NULL OR repo_name = $2) \
             ORDER BY score DESC LIMIT $3",
        )
        .bind(query)
        .bind(repo)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, name, chunk_type, score)| ChunkSearchRow {
                id,
                name,
                chunk_type,
                score: score as f32,
            })
            .collect())
    }

    async fn search_large_chunks_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ChunkSearchRow>> {
        let vec = pgvector::Vector::from(query_vec.to_vec());
        let rows: Vec<(Uuid, String, f64)> = sqlx::query_as(
            "SELECT id, module_path, 1 - (embedding <=> $1) as score \
             FROM large_chunks WHERE ($3::text IS NULL OR repo_name = $3) \
             ORDER BY embedding <=> $1 LIMIT $2",
        )
        .bind(&vec)
        .bind(limit)
        .bind(repo)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, name, score)| ChunkSearchRow {
                id,
                name,
                chunk_type: "large_chunk".into(),
                score: score as f32,
            })
            .collect())
    }

    async fn search_chunks_by_signature(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> Result<Vec<(Uuid, String, f64)>> {
        let vec = pgvector::Vector::from(query_vec.to_vec());
        let rows: Vec<(Uuid, String, f64)> = sqlx::query_as(
            "SELECT id, name, 1 - (signature_embedding <=> $1) as score \
             FROM chunks WHERE signature_embedding IS NOT NULL \
               AND ($3::text IS NULL OR repo_name = $3) \
             ORDER BY signature_embedding <=> $1 LIMIT $2",
        )
        .bind(&vec)
        .bind(limit)
        .bind(repo)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    async fn count_chunks(&self, repo_name: &str) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks WHERE repo_name = $1")
            .bind(repo_name)
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    async fn get_language_distribution(&self, repo_name: &str) -> Result<Vec<LanguageCount>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT language, COUNT(*) AS cnt \
             FROM chunks WHERE repo_name = $1 AND language IS NOT NULL \
             GROUP BY language ORDER BY cnt DESC LIMIT 3",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(language, count)| LanguageCount { language, count })
            .collect())
    }

    async fn get_max_ingested_at(
        &self,
        repo_name: &str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        let row: Option<(Option<chrono::DateTime<chrono::Utc>>,)> =
            sqlx::query_as("SELECT MAX(ingested_at) FROM chunks WHERE repo_name = $1")
                .bind(repo_name)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|(ts,)| ts))
    }

    async fn check_symbol_in_chunks(&self, repo_name: &str, symbol_name: &str) -> Result<i64> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM chunks WHERE repo_name = $1 AND name = $2")
                .bind(repo_name)
                .bind(symbol_name)
                .fetch_one(&self.pool)
                .await?;
        Ok(count)
    }

    async fn fetch_chunk_definitions(&self, chunk_ids: &[Uuid]) -> Result<Vec<ChunkDefinition>> {
        // The catalog lists this as fetch_chunk_definitions(chunk_ids) -> Vec<(name, signature, fqn)>
        // Source: retrieval/graphrag/symbol_resolution.rs:208 (fqn lookup variant)
        // We also expose the general id-based variant used by detail endpoints.
        let rows: Vec<(String, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT name, signature, fqn FROM chunks WHERE id = ANY($1)")
                .bind(chunk_ids)
                .fetch_all(&self.pool)
                .await?;

        Ok(rows
            .into_iter()
            .map(|(name, signature, fqn)| ChunkDefinition {
                name,
                signature,
                fqn,
            })
            .collect())
    }

    async fn fetch_chunk_content(&self, chunk_id: Uuid) -> Result<Option<ChunkContentRow>> {
        // SQL moved verbatim from akashic-retrieval::graphrag::mod.rs fetch_full_node
        // Code branch (chunk arm).
        let row: Option<(String, String, Option<String>)> =
            sqlx::query_as("SELECT content, module_path, language FROM chunks WHERE id = $1")
                .bind(chunk_id)
                .fetch_optional(&self.pool)
                .await?;

        Ok(row.map(|(content, module_path, language)| ChunkContentRow {
            content,
            module_path,
            language,
        }))
    }

    async fn fetch_large_chunk_content(
        &self,
        chunk_id: Uuid,
    ) -> Result<Option<LargeChunkContentRow>> {
        // SQL moved verbatim from akashic-retrieval::graphrag::mod.rs fetch_full_node
        // Code branch (large_chunk arm).
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT content, module_path FROM large_chunks WHERE id = $1")
                .bind(chunk_id)
                .fetch_optional(&self.pool)
                .await?;

        Ok(row.map(|(content, module_path)| LargeChunkContentRow {
            content,
            module_path,
        }))
    }

    // ── Pipeline query helpers (A1 Task 8) ────────────────────────────────────

    async fn fetch_chunks_for_call_resolution(
        &self,
        repo_name: &str,
    ) -> Result<Vec<ChunkCallResolutionRow>> {
        // SQL moved verbatim from akashic-ingestion::ingestion::pipeline::run_inner Stage 6.
        // D1c-1: `signature` added to the SELECT so ChunkIndex::build_from can
        // parse each method's declared return type into `by_return_type`.
        // D1c-2: `content` added so struct chunks' full field list can be
        // parsed into `by_field_type` (content is NOT NULL in `chunks`).
        let rows: Vec<(
            Uuid,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        )> = sqlx::query_as(
            "SELECT id, name, module_path, chunk_type, fqn, parent_fqn, signature, content \
             FROM chunks WHERE repo_name = $1",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, name, module_path, chunk_type, fqn, parent_fqn, signature, content)| {
                    ChunkCallResolutionRow {
                        id,
                        name,
                        module_path,
                        chunk_type,
                        fqn,
                        parent_fqn,
                        signature,
                        content,
                    }
                },
            )
            .collect())
    }

    async fn fetch_go_chunks(&self, repo_name: &str) -> Result<Vec<GoChunkRow>> {
        // SQL moved verbatim from akashic-ingestion::ingestion::pipeline::run_inner
        // Stage 6 Go structural block.
        let rows: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
            "SELECT id, name, chunk_type, content, module_path \
             FROM chunks \
             WHERE repo_name = $1 AND language = $2 AND chunk_type IN ('type', 'method')",
        )
        .bind(repo_name)
        .bind("go")
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, name, chunk_type, content, module_path)| GoChunkRow {
                id,
                name,
                chunk_type,
                content,
                module_path,
            })
            .collect())
    }

    async fn fetch_chunks_for_entry_points(
        &self,
        repo_name: &str,
    ) -> Result<Vec<EntryPointChunkRow>> {
        // SQL moved verbatim from akashic-ingestion::ingestion::pipeline::run_inner Stage 7.
        let rows: Vec<(Uuid, String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT id, name, module_path, content, language FROM chunks WHERE repo_name = $1",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, name, module_path, content, language)| EntryPointChunkRow {
                    id,
                    name,
                    module_path,
                    content,
                    language,
                },
            )
            .collect())
    }

    async fn count_chunks_per_module(
        &self,
        repo_name: &str,
        module_paths: &[String],
    ) -> Result<Vec<(String, i64)>> {
        if module_paths.is_empty() {
            return Ok(Vec::new());
        }
        // SQL moved verbatim from graph/module.rs get_module_graph.
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT module_path, COUNT(*) FROM chunks \
             WHERE repo_name = $1 AND module_path = ANY($2) \
             GROUP BY module_path",
        )
        .bind(repo_name)
        .bind(module_paths)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn list_chunks_in_module_all(
        &self,
        repo_name: &str,
        module_path: &str,
    ) -> Result<Vec<ChunkRow>> {
        // SQL moved verbatim from graph/module.rs get_module_detail / get_module_call_graph,
        // extended (Roadmap F, Task 7.5) with fqn/parent_fqn: this is the ONLY
        // caller (`stage4_embed_store`'s content-match fetch) that needs them,
        // to build the union `ChunkIndexEntry` Stage 6/7 resolve from.
        let rows: Vec<(
            Uuid,
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, name, chunk_type, signature, content, language, fqn, parent_fqn \
             FROM chunks WHERE repo_name = $1 AND module_path = $2 \
             ORDER BY chunk_type, name",
        )
        .bind(repo_name)
        .bind(module_path)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, name, chunk_type, signature, content, language, fqn, parent_fqn)| ChunkRow {
                    id,
                    name,
                    chunk_type,
                    signature,
                    content,
                    language,
                    fqn,
                    parent_fqn,
                },
            )
            .collect())
    }

    async fn get_chunk_module_batch(&self, chunk_ids: &[Uuid]) -> Result<Vec<ChunkModuleRow>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        // SQL moved verbatim from graph/module.rs get_module_call_graph.
        let rows: Vec<(Uuid, String, String)> =
            sqlx::query_as("SELECT id, chunk_type, module_path FROM chunks WHERE id = ANY($1)")
                .bind(chunk_ids)
                .fetch_all(&self.pool)
                .await?;

        Ok(rows
            .into_iter()
            .map(|(id, chunk_type, module_path)| ChunkModuleRow {
                id,
                chunk_type,
                module_path,
            })
            .collect())
    }

    async fn fetch_chunks_meta_batch(
        &self,
        chunk_ids: &[Uuid],
    ) -> Result<Vec<(Uuid, String, String, String)>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        // SQL moved verbatim from graph/doc.rs get_document_detail / get_cluster_detail.
        let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
            "SELECT id, name, chunk_type, module_path FROM chunks WHERE id = ANY($1)",
        )
        .bind(chunk_ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn fetch_chunks_full_batch(&self, chunk_ids: &[Uuid]) -> Result<Vec<ChunkFullRow>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        // SQL moved verbatim from search.rs details handler.
        let rows: Vec<(
            Uuid,
            String,
            String,
            String,
            Option<String>,
            String,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, name, chunk_type, module_path, signature, content, language \
                 FROM chunks WHERE id = ANY($1)",
        )
        .bind(chunk_ids)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, name, chunk_type, module_path, signature, content, language)| ChunkFullRow {
                    id,
                    name,
                    chunk_type,
                    module_path,
                    signature,
                    content,
                    language,
                },
            )
            .collect())
    }
}
