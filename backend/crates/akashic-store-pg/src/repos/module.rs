//! PostgreSQL adapter for `ModuleRepo`.
//!
//! SQL moved verbatim from:
//! - `akashic-ingestion::ingestion::store.rs`
//! - `akashic-server::api::routes::modules.rs`
//! - `akashic-retrieval::graphrag::mod.rs`
//! - `akashic-curation::notes::memory_stack.rs`

use anyhow::Result;
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::ModuleRepo;
use akashic_domain::types::{ModuleMetaRow, ModuleRow, ModuleSummary};

/// PostgreSQL adapter implementing [`ModuleRepo`].
#[derive(Clone)]
pub struct PgModuleRepo {
    pool: PgPool,
}

impl PgModuleRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ModuleRepo for PgModuleRepo {
    async fn upsert_module(
        &self,
        repo_name: &str,
        path: &str,
        language: &str,
        summary: &str,
        exports_count: i32,
        file_count: i32,
        is_virtual: bool,
        git_ref: &str,
        embedding: Option<&[f32]>,
    ) -> Result<Uuid> {
        let emb = embedding.map(|e| pgvector::Vector::from(e.to_vec()));

        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO modules (repo_name, path, language, summary, exports_count, file_count, is_virtual, embedding, git_ref) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (repo_name, path) DO UPDATE SET \
               language = EXCLUDED.language, \
               summary = EXCLUDED.summary, \
               exports_count = EXCLUDED.exports_count, \
               file_count = EXCLUDED.file_count, \
               is_virtual = EXCLUDED.is_virtual, \
               embedding = EXCLUDED.embedding, \
               git_ref = EXCLUDED.git_ref, \
               ingested_at = now() \
             RETURNING id",
        )
        .bind(repo_name)
        .bind(path)
        .bind(language)
        .bind(summary)
        .bind(exports_count)
        .bind(file_count)
        .bind(is_virtual)
        .bind(emb)
        .bind(git_ref)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.0)
    }

    async fn clean_modules(&self, repo_name: &str) -> Result<()> {
        sqlx::query("DELETE FROM modules WHERE repo_name = $1")
            .bind(repo_name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete_modules_by_repo(&self, repo_name: &str) -> Result<()> {
        // Aliases clean_modules — same SQL, two port names for clarity
        // (clean = pre-ingest wipe; delete = lifecycle teardown).
        sqlx::query("DELETE FROM modules WHERE repo_name = $1")
            .bind(repo_name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn count_modules(&self, repo_name: &str) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM modules WHERE repo_name = $1")
            .bind(repo_name)
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    async fn list_modules(
        &self,
        repo_name: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ModuleRow>> {
        let rows: Vec<(
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Option<i32>,
            Option<i32>,
        )> = sqlx::query_as(
            "SELECT id, path, language, summary, exports_count, file_count \
                 FROM modules WHERE repo_name = $1 ORDER BY path \
                 LIMIT $2 OFFSET $3",
        )
        .bind(repo_name)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, path, language, summary, exports_count, file_count)| ModuleRow {
                    id,
                    path,
                    language,
                    summary,
                    exports_count,
                    file_count,
                },
            )
            .collect())
    }

    async fn get_module_detail(&self, module_id: Uuid) -> Result<Option<ModuleRow>> {
        let row: Option<(
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Option<i32>,
            Option<i32>,
        )> = sqlx::query_as(
            "SELECT id, path, language, summary, exports_count, file_count \
                 FROM modules WHERE id = $1",
        )
        .bind(module_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(
            |(id, path, language, summary, exports_count, file_count)| ModuleRow {
                id,
                path,
                language,
                summary,
                exports_count,
                file_count,
            },
        ))
    }

    async fn fetch_module_summary(&self, module_id: Uuid) -> Result<Option<ModuleSummary>> {
        let row: Option<(String, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT path, summary, language FROM modules WHERE id = $1")
                .bind(module_id)
                .fetch_optional(&self.pool)
                .await?;

        Ok(row.map(|(path, summary, language)| ModuleSummary {
            path,
            summary,
            language,
        }))
    }

    async fn resolve_module_paths(
        &self,
        repo_name: &str,
        paths: &[String],
    ) -> Result<Vec<(String, Uuid)>> {
        let rows: Vec<(String, Uuid)> =
            sqlx::query_as("SELECT path, id FROM modules WHERE repo_name = $1 AND path = ANY($2)")
                .bind(repo_name)
                .bind(paths)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    async fn get_module_meta_batch(&self, module_ids: &[Uuid]) -> Result<Vec<ModuleMetaRow>> {
        if module_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid, Option<String>, bool)> =
            sqlx::query_as("SELECT id, language, is_virtual FROM modules WHERE id = ANY($1)")
                .bind(module_ids)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(id, language, is_virtual)| ModuleMetaRow {
                id,
                language,
                is_virtual,
            })
            .collect())
    }

    async fn get_module_paths_batch(&self, module_ids: &[Uuid]) -> Result<Vec<(Uuid, String)>> {
        if module_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT id, path FROM modules WHERE id = ANY($1)")
                .bind(module_ids)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    async fn get_module_path_and_repo(
        &self,
        module_id: Uuid,
    ) -> Result<Option<(Uuid, String, String)>> {
        let row: Option<(Uuid, String, String)> =
            sqlx::query_as("SELECT id, path, repo_name FROM modules WHERE id = $1")
                .bind(module_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row)
    }
}
