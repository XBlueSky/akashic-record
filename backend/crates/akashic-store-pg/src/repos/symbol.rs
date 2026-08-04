//! PostgreSQL adapter for `SymbolRepo`.
//!
//! All SQL is moved verbatim from the original call-sites:
//! - `akashic-retrieval::graphrag::symbol_resolution` (`resolve_symbol`,
//!   `find_references` enrich block, `find_implementations` enrich block)
//! - `akashic-retrieval::linking::explains` (chunk / module id lookups,
//!   candidate fetch)
//! - `akashic-retrieval::linking::mod.rs` (chunk id lookups for ATTACHED_TO)

use anyhow::Result;
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::{ExplainsLinkCandidate, SymbolRepo};
use akashic_domain::types::{ChunkIdRow, ChunkMetaRow, ModuleIdRow, SymbolCandidateRow};

/// PostgreSQL adapter implementing [`SymbolRepo`].
#[derive(Clone)]
pub struct PgSymbolRepo {
    pool: PgPool,
}

impl PgSymbolRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SymbolRepo for PgSymbolRepo {
    async fn resolve_symbol_candidates(
        &self,
        name: &str,
        escaped_suffix: &str,
        repo: Option<&str>,
        module_hint: Option<&str>,
        kind_hint: Option<&str>,
    ) -> Result<Vec<SymbolCandidateRow>> {
        // SQL moved verbatim from
        // akashic-retrieval::graphrag::symbol_resolution::resolve_symbol.
        // The LIKE suffix ($2) is pre-computed by the caller (e.g. "%.name")
        // to avoid re-formatting on every call.
        let rows: Vec<(
            Option<String>,
            String,
            String,
            String,
            Option<String>,
            Option<i32>,
            Option<i32>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT fqn, name, chunk_type, module_path, visibility, \
                    start_line, end_line, signature \
             FROM chunks \
             WHERE (fqn = $1 OR fqn LIKE $2 OR name = $1) \
               AND ($3::text IS NULL OR repo_name = $3) \
               AND ($4::text IS NULL OR module_path ILIKE '%' || $4 || '%') \
               AND ($5::text IS NULL OR chunk_type = $5) \
             LIMIT 50",
        )
        .bind(name)
        .bind(escaped_suffix)
        .bind(repo)
        .bind(module_hint)
        .bind(kind_hint)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(
                    fqn,
                    name,
                    chunk_type,
                    module_path,
                    visibility,
                    start_line,
                    end_line,
                    signature,
                )| {
                    SymbolCandidateRow {
                        fqn,
                        name,
                        chunk_type,
                        module_path,
                        visibility,
                        start_line,
                        end_line,
                        signature,
                    }
                },
            )
            .collect())
    }

    async fn enrich_chunk_meta_by_fqns(&self, fqns: &[String]) -> Result<Vec<ChunkMetaRow>> {
        // SQL moved verbatim from
        // akashic-retrieval::graphrag::symbol_resolution::find_references
        // enrichment block (no repo constraint — callers may be cross-repo).
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT fqn, module_path, chunk_type FROM chunks \
             WHERE fqn = ANY($1)",
        )
        .bind(fqns)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(fqn, module_path, chunk_type)| ChunkMetaRow {
                fqn,
                module_path,
                chunk_type,
            })
            .collect())
    }

    async fn enrich_chunk_meta_by_fqns_and_repo(
        &self,
        fqns: &[String],
        repo: Option<&str>,
    ) -> Result<Vec<ChunkMetaRow>> {
        // SQL moved verbatim from
        // akashic-retrieval::graphrag::symbol_resolution::find_implementations
        // enrichment block.
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT fqn, module_path, chunk_type FROM chunks \
             WHERE fqn = ANY($1) AND ($2::text IS NULL OR repo_name = $2)",
        )
        .bind(fqns)
        .bind(repo)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(fqn, module_path, chunk_type)| ChunkMetaRow {
                fqn,
                module_path,
                chunk_type,
            })
            .collect())
    }

    async fn find_chunk_ids_by_name(&self, name: &str, limit: i64) -> Result<Vec<ChunkIdRow>> {
        // SQL moved verbatim from
        // akashic-retrieval::linking::explains::create_deterministic_explains_edges
        // chunk-match branch.
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, repo_name FROM chunks WHERE name = $1 AND length($1) >= 5 LIMIT $2",
        )
        .bind(name)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, repo_name)| ChunkIdRow { id, repo_name })
            .collect())
    }

    async fn find_chunk_ids_by_name_and_module(
        &self,
        repo_name: &str,
        name: &str,
        parent: &str,
        limit: i64,
    ) -> Result<Vec<Uuid>> {
        // SQL moved verbatim from
        // akashic-retrieval::linking::mod.rs::link_note_to_chunks
        // parent-qualified branch.
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM chunks WHERE repo_name = $1 AND name = $2 \
             AND module_path LIKE '%' || $3 || '%' LIMIT $4",
        )
        .bind(repo_name)
        .bind(name)
        .bind(parent)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn find_chunk_ids_by_name_in_repo(
        &self,
        repo_name: &str,
        name: &str,
        limit: i64,
    ) -> Result<Vec<Uuid>> {
        // SQL moved verbatim from
        // akashic-retrieval::linking::mod.rs::link_note_to_chunks
        // simple-name branch.
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("SELECT id FROM chunks WHERE repo_name = $1 AND name = $2 LIMIT $3")
                .bind(repo_name)
                .bind(name)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;

        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn find_module_ids_by_path_fragment(
        &self,
        fragment: &str,
        limit: i64,
    ) -> Result<Vec<ModuleIdRow>> {
        // SQL moved verbatim from
        // akashic-retrieval::linking::explains::create_deterministic_explains_edges
        // module-match branch.
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, repo_name FROM modules \
             WHERE path LIKE '%' || $1 || '%' AND length($1) >= 5 LIMIT $2",
        )
        .bind(fragment)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, repo_name)| ModuleIdRow { id, repo_name })
            .collect())
    }

    async fn find_chunks_for_explains_linking(
        &self,
        query_vec: &[f32],
        limit: i64,
    ) -> Result<Vec<ExplainsLinkCandidate>> {
        // SQL moved verbatim from
        // akashic-retrieval::linking::explains::create_llm_verified_explains_edges
        // candidate-fetch block.
        let vec = pgvector::Vector::from(query_vec.to_vec());
        let rows: Vec<(Uuid, String, String, String, String, f64)> = sqlx::query_as(
            "SELECT id, name, chunk_type, module_path, repo_name, \
                    1 - (embedding <=> $1) AS similarity \
             FROM chunks \
             ORDER BY embedding <=> $1 \
             LIMIT $2",
        )
        .bind(&vec)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, name, chunk_type, module_path, repo_name, similarity)| {
                    ExplainsLinkCandidate {
                        id,
                        name,
                        chunk_type,
                        module_path,
                        repo_name,
                        similarity,
                    }
                },
            )
            .collect())
    }
}
