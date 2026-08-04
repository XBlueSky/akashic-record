//! PostgreSQL adapter for `IngestionJobRepo`.
//!
//! All SQL is moved verbatim from the original call-sites:
//! - `akashic-ingestion::ingestion::store` (`create_job`, `update_job_status`,
//!   `set_total_files`)
//! - `akashic-ingestion::ingestion::pipeline` (`create_job_resumed`)
//!
//! (Roadmap F, Task 8): `save_checkpoint`/`load_checkpoint` are gone —
//! checkpoint/resume machinery is retired entirely.

use anyhow::{Context, Result};
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::IngestionJobRepo;

/// PostgreSQL adapter implementing [`IngestionJobRepo`].
#[derive(Clone)]
pub struct PgIngestionJobRepo {
    pool: PgPool,
}

impl PgIngestionJobRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl IngestionJobRepo for PgIngestionJobRepo {
    async fn create_job(&self, repo_name: &str, git_ref: &str) -> Result<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO ingestion_jobs (repo_name, git_ref, status) VALUES ($1, $2, 'pending') RETURNING id",
        )
        .bind(repo_name)
        .bind(git_ref)
        .fetch_one(&self.pool)
        .await
        .context("Failed to create ingestion job")?;
        Ok(row.0)
    }

    async fn create_job_resumed(
        &self,
        repo_name: &str,
        git_ref: &str,
        resumed_from: Uuid,
    ) -> Result<Uuid> {
        let job_id: Uuid = sqlx::query_scalar(
            "INSERT INTO ingestion_jobs (repo_name, git_ref, status, resumed_from) \
             VALUES ($1, $2, 'pending', $3) RETURNING id",
        )
        .bind(repo_name)
        .bind(git_ref)
        .bind(resumed_from)
        .fetch_one(&self.pool)
        .await
        .context("Failed to create resumed ingestion job")?;
        Ok(job_id)
    }

    async fn update_job_status(
        &self,
        job_id: Uuid,
        status: &str,
        processed_files: Option<i32>,
        total_chunks: Option<i32>,
        error_message: Option<&str>,
    ) -> Result<Option<i32>> {
        let completed_at = if status == "done" || status == "failed" {
            Some(chrono::Utc::now())
        } else {
            None
        };

        // Fetch total_files so the caller can include it in the SSE event
        let total_files: Option<i32> =
            sqlx::query_scalar("SELECT total_files FROM ingestion_jobs WHERE id = $1")
                .bind(job_id)
                .fetch_optional(&self.pool)
                .await?
                .flatten();

        sqlx::query(
            "UPDATE ingestion_jobs SET status = $1, \
             processed_files = COALESCE($2, processed_files), \
             total_chunks = COALESCE($3, total_chunks), \
             error_message = COALESCE($4, error_message), \
             completed_at = COALESCE($5, completed_at) \
             WHERE id = $6",
        )
        .bind(status)
        .bind(processed_files)
        .bind(total_chunks)
        .bind(error_message)
        .bind(completed_at)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .context("Failed to update job status")?;

        Ok(total_files)
    }

    async fn set_total_files(&self, job_id: Uuid, total: i32) -> Result<()> {
        sqlx::query("UPDATE ingestion_jobs SET total_files = $1 WHERE id = $2")
            .bind(total)
            .bind(job_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn has_active_job(&self, repo_name: &str) -> Result<bool> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT status FROM ingestion_jobs \
             WHERE repo_name = $1 AND status NOT IN ('done', 'completed', 'failed') \
             LIMIT 1",
        )
        .bind(repo_name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    async fn latest_job(
        &self,
        repo_name: &str,
    ) -> Result<
        Option<(
            Uuid,
            String,
            Option<String>,
            String,
            Option<i32>,
            Option<i32>,
            Option<i32>,
            Option<String>,
            Option<String>,
            Option<String>,
        )>,
    > {
        let row = sqlx::query_as(
            "SELECT id, repo_name, git_ref, status, total_files, processed_files, \
                    total_chunks, error_message, \
                    to_char(started_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
                    to_char(completed_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
             FROM ingestion_jobs WHERE repo_name = $1 \
             ORDER BY started_at DESC LIMIT 1",
        )
        .bind(repo_name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn get_last_job_git_ref(&self, repo_name: &str) -> Result<Option<String>> {
        // SQL moved verbatim from akashic-server::api::routes::ingestion::reingest
        // (the last_job query that recovers the git_ref for reingest).
        let row: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT git_ref FROM ingestion_jobs \
             WHERE repo_name = $1 \
             ORDER BY started_at DESC LIMIT 1",
        )
        .bind(repo_name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|(r,)| r))
    }

    async fn find_resumable_job(
        &self,
        repo_name: &str,
    ) -> Result<Option<(Uuid, String, Option<String>)>> {
        // SQL moved verbatim from akashic-server::api::routes::ingestion::resume_ingest.
        // (Roadmap F, Task 8): `checkpoint_data IS NOT NULL` is intentionally
        // UNCHANGED — see the trait doc comment on `find_resumable_job` for
        // why this now only ever matches pre-existing (pre-Task-8) rows.
        let row: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
            "SELECT j.id, COALESCE(j.git_ref, ''), \
                    (SELECT source_type FROM sources WHERE repo_name = $1) \
             FROM ingestion_jobs j \
             WHERE j.repo_name = $1 AND j.status = 'failed' AND j.checkpoint_data IS NOT NULL \
             ORDER BY j.started_at DESC LIMIT 1",
        )
        .bind(repo_name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn list_active_jobs(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<(String, String, Option<i32>, Option<i32>)>> {
        // SQL moved verbatim from akashic-server::api::routes::ingestion::active_jobs
        // (DISTINCT ON paginated query, finding ingestion.rs:600).
        let rows: Vec<(String, String, Option<i32>, Option<i32>)> = sqlx::query_as(
            "SELECT DISTINCT ON (repo_name) repo_name, status, processed_files, total_files \
             FROM ingestion_jobs \
             WHERE status NOT IN ('done', 'completed', 'failed') \
             ORDER BY repo_name, started_at DESC \
             LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn mark_stale_jobs_failed(&self) -> Result<i64> {
        // SQL moved verbatim from main.rs startup sweep (A2a Task 9).
        let stale = sqlx::query_scalar::<_, i32>(
            "UPDATE ingestion_jobs \
             SET status = 'failed', error_message = 'Backend restarted during ingestion', completed_at = now() \
             WHERE status NOT IN ('done', 'completed', 'failed') \
               AND started_at < now() - interval '10 minutes' \
             RETURNING 1",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(stale.len() as i64)
    }
}
