//! PostgreSQL adapters for `SagaRepo` and `SagaExecutorRepo`.
//!
//! All SQL is moved verbatim from the original call-sites in:
//!
//! - `akashic-curation::notes::saga` (`find_by_source`, `find_by_name`,
//!   `create_saga`, `get_saga_by_source`, `get_saga_by_id`,
//!   `list_sagas_with_status_filter`, `list_all_sagas`,
//!   `get_saga_timeline_entries`)
//! - `akashic-curation::notes::memory_stack` (`count_active_sagas`,
//!   `count_resolved_sagas`, `get_saga_info`)
//! - `akashic-curation::saga::mod` (`find_idempotent_saga`, `delete_saga`,
//!   `insert_saga`, `insert_saga_step`, `update_saga_completion`,
//!   `update_saga_status`, `update_step_status`, `update_step_status_by_name`,
//!   `cleanup_stale_sagas`, `cleanup_expired_idempotency`)

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::services::{IdempotencyCheck, IdempotencyReservation};
use akashic_domain::ports::{SagaExecutorRepo, SagaRepo};
use akashic_domain::types::{SagaDbRow, SagaIdempotencyRecord, SagaTimelineEntry, SagaWithCount};

// ── PgSagaRepo ────────────────────────────────────────────────────────────────

/// PostgreSQL adapter implementing [`SagaRepo`].
#[derive(Clone)]
pub struct PgSagaRepo {
    pool: PgPool,
}

impl PgSagaRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

// Helper: convert the SagaListRow tuple into SagaWithCount.
type SagaListRow = (
    Uuid,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
    Option<chrono::DateTime<chrono::Utc>>,
    i64,
);

fn row_to_saga_with_count(row: SagaListRow) -> SagaWithCount {
    let (
        id,
        repo_name,
        name,
        source_type,
        source_ref,
        summary,
        status,
        created_at,
        updated_at,
        resolved_at,
        note_count,
    ) = row;
    SagaWithCount {
        saga: SagaDbRow {
            id,
            repo_name,
            name,
            source_type,
            source_ref,
            summary,
            status,
            created_at,
            updated_at,
            resolved_at,
        },
        note_count,
    }
}

#[async_trait]
impl SagaRepo for PgSagaRepo {
    // ── Low-level find helpers ────────────────────────────────────────────────

    async fn find_by_source(
        &self,
        repo_name: &str,
        source_type: &str,
        source_ref: &str,
    ) -> Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM sagas \
             WHERE repo_name = $1 AND source_type = $2 AND source_ref = $3 \
             LIMIT 1",
        )
        .bind(repo_name)
        .bind(source_type)
        .bind(source_ref)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id,)| id))
    }

    async fn find_by_name(
        &self,
        repo_name: &str,
        source_type: &str,
        name: &str,
    ) -> Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM sagas \
             WHERE repo_name = $1 AND source_type = $2 AND source_ref IS NULL AND name = $3 \
             LIMIT 1",
        )
        .bind(repo_name)
        .bind(source_type)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id,)| id))
    }

    async fn create_saga(
        &self,
        saga_id: Uuid,
        saga_type: &str,
        repo_name: &str,
        status: &str,
        name: Option<&str>,
        source_type: &str,
        source_ref: Option<&str>,
    ) -> Result<Option<Uuid>> {
        // Finding (saga.rs:113): the partial unique index
        // `idx_sagas_unique_source_ref` only covers rows with
        // `source_ref IS NOT NULL AND saga_type = 'knowledge'`, so this ON CONFLICT
        // arm only provides race protection for source-backed sagas (issue/branch).
        // Manual-topic sagas (source_ref IS NULL) are NOT covered by any unique
        // index, so concurrent creates of the same (repo, topic) can still produce
        // duplicate rows here — the find-then-insert above is best-effort for those.
        let row: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO sagas (id, saga_type, repo_name, status, name, source_type, source_ref) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (repo_name, source_type, source_ref) \
             WHERE source_ref IS NOT NULL AND saga_type = 'knowledge' \
             DO NOTHING \
             RETURNING id",
        )
        .bind(saga_id)
        .bind(saga_type)
        .bind(repo_name)
        .bind(status)
        .bind(name)
        .bind(source_type)
        .bind(source_ref)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id,)| id))
    }

    async fn get_saga_by_source(
        &self,
        repo_name: &str,
        source_type: &str,
        source_ref: &str,
    ) -> Result<Uuid> {
        let (id,): (Uuid,) = sqlx::query_as(
            "SELECT id FROM sagas WHERE repo_name = $1 AND source_type = $2 AND source_ref = $3 \
             AND saga_type = 'knowledge' LIMIT 1",
        )
        .bind(repo_name)
        .bind(source_type)
        .bind(source_ref)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    // ── Structured reads ──────────────────────────────────────────────────────

    async fn get_saga_by_id(&self, saga_id: Uuid) -> Result<SagaDbRow> {
        let row: (
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            chrono::DateTime<chrono::Utc>,
            chrono::DateTime<chrono::Utc>,
            Option<chrono::DateTime<chrono::Utc>>,
        ) = sqlx::query_as(
            "SELECT id, repo_name, name, source_type, source_ref, summary, \
                    status, created_at, updated_at, resolved_at \
             FROM sagas WHERE id = $1 AND saga_type = 'knowledge'",
        )
        .bind(saga_id)
        .fetch_one(&self.pool)
        .await?;

        Ok(SagaDbRow {
            id: row.0,
            repo_name: row.1,
            name: row.2,
            source_type: row.3,
            source_ref: row.4,
            summary: row.5,
            status: row.6,
            created_at: row.7,
            updated_at: row.8,
            resolved_at: row.9,
        })
    }

    async fn list_sagas_with_status_filter(
        &self,
        repo_name: &str,
        status: &str,
    ) -> Result<Vec<SagaWithCount>> {
        // Finding (saga.rs:211): only knowledge sagas are listed. Ingestion-control
        // rows (saga_type in trigger_ingest/reingest/resume_ingest/add_source, with
        // name = NULL) share the `sagas` table but must not leak into the knowledge-
        // saga UI, so both query arms filter on `saga_type = 'knowledge'`.
        let rows: Vec<SagaListRow> = sqlx::query_as(
            "SELECT s.id, s.repo_name, s.name, s.source_type, s.source_ref, s.summary, \
                        s.status, s.created_at, s.updated_at, s.resolved_at, \
                        COUNT(n.id) AS note_count \
                 FROM sagas s \
                 LEFT JOIN notes n ON n.saga_id = s.id \
                 WHERE s.repo_name = $1 AND s.status = $2 \
                   AND s.saga_type = 'knowledge' \
                 GROUP BY s.id \
                 ORDER BY s.updated_at DESC",
        )
        .bind(repo_name)
        .bind(status)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(row_to_saga_with_count).collect())
    }

    async fn list_all_sagas(&self, repo_name: &str) -> Result<Vec<SagaWithCount>> {
        // Finding (saga.rs:211): filter on saga_type = 'knowledge' to exclude
        // ingestion-control rows.
        let rows: Vec<SagaListRow> = sqlx::query_as(
            "SELECT s.id, s.repo_name, s.name, s.source_type, s.source_ref, s.summary, \
                        s.status, s.created_at, s.updated_at, s.resolved_at, \
                        COUNT(n.id) AS note_count \
                 FROM sagas s \
                 LEFT JOIN notes n ON n.saga_id = s.id \
                 WHERE s.repo_name = $1 \
                   AND s.saga_type = 'knowledge' \
                 GROUP BY s.id \
                 ORDER BY s.updated_at DESC",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(row_to_saga_with_count).collect())
    }

    async fn get_saga_timeline_entries(&self, saga_id: Uuid) -> Result<Vec<SagaTimelineEntry>> {
        let rows: Vec<(
            String,
            Option<String>,
            String,
            Option<String>,
            Option<chrono::DateTime<chrono::Utc>>,
            Option<chrono::DateTime<chrono::Utc>>,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
        )> = sqlx::query_as(
            "SELECT id::text AS uuid, title, category, summary, \
                    valid_at, invalid_at, superseded_by::text, created_at \
             FROM notes \
             WHERE saga_id = $1 \
             ORDER BY COALESCE(valid_at, created_at) ASC",
        )
        .bind(saga_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(
                    uuid,
                    title,
                    category,
                    summary,
                    valid_at,
                    invalid_at,
                    superseded_by,
                    created_at,
                )| {
                    SagaTimelineEntry {
                        uuid,
                        title,
                        category,
                        summary,
                        valid_at,
                        invalid_at,
                        superseded_by,
                        created_at,
                    }
                },
            )
            .collect())
    }

    // ── Count reads ───────────────────────────────────────────────────────────

    async fn count_active_sagas(&self, repo_name: &str) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM sagas WHERE repo_name = $1 AND status IN ('active', 'open')",
        )
        .bind(repo_name)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    async fn count_resolved_sagas(&self, repo_name: &str) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM sagas WHERE repo_name = $1 AND status = 'resolved'",
        )
        .bind(repo_name)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    async fn get_saga_info(&self, saga_id: Uuid) -> Result<Option<(Option<String>, String)>> {
        let row: Option<(Option<String>, String)> =
            sqlx::query_as("SELECT name, status FROM sagas WHERE id = $1")
                .bind(saga_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row)
    }

    async fn get_saga_statuses_batch(
        &self,
        saga_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, String>> {
        if saga_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        // SQL moved verbatim from graph/module.rs get_module_graph (N+1 fix).
        let rows: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT id, status FROM sagas WHERE id = ANY($1)")
                .bind(saga_ids)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().collect())
    }
}

// ── PgSagaExecutorRepo ────────────────────────────────────────────────────────

/// PostgreSQL adapter implementing [`SagaExecutorRepo`].
#[derive(Clone)]
pub struct PgSagaExecutorRepo {
    pool: PgPool,
}

impl PgSagaExecutorRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SagaExecutorRepo for PgSagaExecutorRepo {
    async fn find_idempotent_saga(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<SagaIdempotencyRecord>> {
        let row: Option<(Uuid, String, Option<JsonValue>)> =
            sqlx::query_as("SELECT id, status, result_json FROM sagas WHERE idempotency_key = $1")
                .bind(idempotency_key)
                .fetch_optional(&self.pool)
                .await?;

        Ok(row.map(|(id, status, result_json)| SagaIdempotencyRecord {
            id,
            status,
            result_json,
        }))
    }

    async fn delete_saga(&self, saga_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM sagas WHERE id = $1")
            .bind(saga_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn insert_saga(
        &self,
        saga_id: Uuid,
        saga_type: &str,
        idempotency_key: Option<&str>,
        repo_name: &str,
        context_json: &JsonValue,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sagas (id, saga_type, idempotency_key, repo_name, status, context_json) \
             VALUES ($1, $2, $3, $4, 'running', $5)",
        )
        .bind(saga_id)
        .bind(saga_type)
        .bind(idempotency_key)
        .bind(repo_name)
        .bind(context_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn insert_saga_step(
        &self,
        step_id: Uuid,
        saga_id: Uuid,
        step_index: i16,
        step_name: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO saga_steps (id, saga_id, step_index, step_name, status) \
             VALUES ($1, $2, $3, $4, 'pending')",
        )
        .bind(step_id)
        .bind(saga_id)
        .bind(step_index)
        .bind(step_name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_saga_completion(&self, saga_id: Uuid, result_json: &JsonValue) -> Result<()> {
        sqlx::query(
            "UPDATE sagas SET status = 'completed', result_json = $1, updated_at = now() \
             WHERE id = $2",
        )
        .bind(result_json)
        .bind(saga_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_saga_status(&self, saga_id: Uuid, status: &str) -> Result<()> {
        sqlx::query("UPDATE sagas SET status = $1, updated_at = now() WHERE id = $2")
            .bind(status)
            .bind(saga_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn update_step_status(&self, saga_id: Uuid, step_index: i16, status: &str) -> Result<()> {
        sqlx::query(
            "UPDATE saga_steps SET status = $1, updated_at = now() \
             WHERE saga_id = $2 AND step_index = $3",
        )
        .bind(status)
        .bind(saga_id)
        .bind(step_index)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_step_status_by_name(
        &self,
        saga_id: Uuid,
        step_name: &str,
        status: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE saga_steps SET status = $1, updated_at = now() \
             WHERE saga_id = $2 AND step_name = $3",
        )
        .bind(status)
        .bind(saga_id)
        .bind(step_name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn cleanup_stale_sagas(&self, stale_minutes: i64) -> Result<i64> {
        let result = sqlx::query_scalar::<_, i64>(
            "WITH stale AS ( \
                 UPDATE sagas SET status = 'failed', updated_at = now() \
                 WHERE status = 'running' \
                   AND updated_at < now() - make_interval(mins => $1::int) \
                 RETURNING id \
             ) SELECT count(*) FROM stale",
        )
        .bind(stale_minutes)
        .fetch_one(&self.pool)
        .await?;

        if result > 0 {
            tracing::warn!(count = result, "Cleaned up stale sagas");
        }
        Ok(result)
    }

    async fn cleanup_expired_idempotency(&self, ttl_hours: i64) -> Result<i64> {
        let result = sqlx::query_scalar::<_, i64>(
            "WITH expired AS ( \
                 DELETE FROM sagas \
                 WHERE status = 'completed' \
                   AND idempotency_key IS NOT NULL \
                   AND updated_at < now() - make_interval(hours => $1::int) \
                 RETURNING id \
             ) SELECT count(*) FROM expired",
        )
        .bind(ttl_hours)
        .fetch_one(&self.pool)
        .await?;

        if result > 0 {
            tracing::info!(count = result, "Cleaned up expired idempotency keys");
        }
        Ok(result)
    }

    async fn idempotency_check(&self, key: &str) -> Result<IdempotencyCheck> {
        // SQL moved verbatim from akashic-server::api::extractors::check_idempotency.
        let existing: Option<(String, Option<JsonValue>)> =
            sqlx::query_as("SELECT status, result_json FROM sagas WHERE idempotency_key = $1")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;

        if let Some((status, result)) = existing {
            match status.as_str() {
                // A completed row with a NULL body is treated as a cache miss
                // (Fresh), matching the legacy `Ok(result)` (= Ok(None)) handler.
                "completed" => match result {
                    Some(v) => return Ok(IdempotencyCheck::Completed(v)),
                    None => return Ok(IdempotencyCheck::Fresh),
                },
                "running" => return Ok(IdempotencyCheck::Running),
                "failed" => {
                    sqlx::query("DELETE FROM sagas WHERE idempotency_key = $1")
                        .bind(key)
                        .execute(&self.pool)
                        .await?;
                }
                _ => {}
            }
        }
        Ok(IdempotencyCheck::Fresh)
    }

    async fn idempotency_reserve(
        &self,
        key: &str,
        saga_type: &str,
        repo_name: &str,
    ) -> Result<IdempotencyReservation> {
        // SQL + state machine moved verbatim from
        // akashic-server::api::extractors::reserve_idempotency (the atomic
        // INSERT-on-conflict that closes the double-ingest TOCTOU).
        for _ in 0..3 {
            let inserted = sqlx::query(
                "INSERT INTO sagas (id, saga_type, idempotency_key, repo_name, status) \
                 VALUES (gen_random_uuid(), $1, $2, $3, 'running') \
                 ON CONFLICT (idempotency_key) DO NOTHING",
            )
            .bind(saga_type)
            .bind(key)
            .bind(repo_name)
            .execute(&self.pool)
            .await?
            .rows_affected();

            if inserted == 1 {
                return Ok(IdempotencyReservation::Reserved);
            }

            let existing: Option<(String, Option<JsonValue>)> =
                sqlx::query_as("SELECT status, result_json FROM sagas WHERE idempotency_key = $1")
                    .bind(key)
                    .fetch_optional(&self.pool)
                    .await?;

            match existing {
                Some((status, result)) => match status.as_str() {
                    "completed" => {
                        return Ok(IdempotencyReservation::Cached(
                            result.unwrap_or(JsonValue::Null),
                        ));
                    }
                    "failed" => {
                        sqlx::query(
                            "DELETE FROM sagas WHERE idempotency_key = $1 AND status = 'failed'",
                        )
                        .bind(key)
                        .execute(&self.pool)
                        .await?;
                        continue;
                    }
                    // 'running' or any in-flight state: another request owns it.
                    _ => return Ok(IdempotencyReservation::InProgress),
                },
                // Row vanished between the INSERT conflict and this SELECT — retry.
                None => continue,
            }
        }
        Ok(IdempotencyReservation::InProgress)
    }

    async fn idempotency_finalize(&self, key: &str, result: &JsonValue) -> Result<()> {
        sqlx::query(
            "UPDATE sagas SET status = 'completed', result_json = $2, updated_at = now() \
             WHERE idempotency_key = $1",
        )
        .bind(key)
        .bind(result)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn idempotency_release(&self, key: &str) {
        if let Err(e) =
            sqlx::query("DELETE FROM sagas WHERE idempotency_key = $1 AND status = 'running'")
                .bind(key)
                .execute(&self.pool)
                .await
        {
            tracing::warn!(error = %e, "failed to release idempotency reservation");
        }
    }
}
