//! Custom axum extractors for idempotency and parameter validation.

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use akashic_domain::ports::services::{
    IdempotencyCheck, IdempotencyReservation as DomainReservation,
};

use akashic_context::AppState;

use super::error::AppError;

// ── Parameter Validation ─────────────────────────────────────────────

pub fn validate_repo_name(name: &str) -> Result<(), AppError> {
    if name.is_empty() || name.len() > 128 {
        return Err(AppError::BadRequest(
            "repo_name must be 1-128 characters".into(),
        ));
    }
    if name.contains("..") {
        return Err(AppError::BadRequest(
            "repo_name must not contain '..'".into(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
    {
        return Err(AppError::BadRequest(
            "repo_name contains invalid characters".into(),
        ));
    }
    Ok(())
}

pub fn validate_branch(branch: &str) -> Result<(), AppError> {
    if branch.is_empty() || branch.len() > 256 {
        return Err(AppError::BadRequest(
            "branch must be 1-256 characters".into(),
        ));
    }
    if branch.contains("..") {
        return Err(AppError::BadRequest("branch must not contain '..'".into()));
    }
    if branch
        .chars()
        .any(|c| c.is_control() || matches!(c, '~' | '^' | ':' | '\\'))
    {
        return Err(AppError::BadRequest(
            "branch contains invalid characters".into(),
        ));
    }
    Ok(())
}

pub fn validate_module_path(path: &str) -> Result<(), AppError> {
    if path.is_empty() || path.len() > 512 {
        return Err(AppError::BadRequest(
            "module_path must be 1-512 characters".into(),
        ));
    }
    if path.contains("..") {
        return Err(AppError::BadRequest(
            "module_path must not contain '..'".into(),
        ));
    }
    if path.contains('\0') {
        return Err(AppError::BadRequest(
            "module_path must not contain null bytes".into(),
        ));
    }
    Ok(())
}

pub fn validate_search_query(q: &str) -> Result<(), AppError> {
    if q.len() > 1000 {
        return Err(AppError::BadRequest(
            "search query must be at most 1000 characters".into(),
        ));
    }
    if q.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(AppError::BadRequest(
            "search query contains invalid characters".into(),
        ));
    }
    Ok(())
}

// ── Pagination ───────────────────────────────────────────────────────

/// Paginated response envelope.
#[derive(Serialize)]
pub struct Paginated<T: Serialize> {
    pub data: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
}

impl<T: Serialize> Paginated<T> {
    /// Build from a full result set: take `limit` items and compute cursor.
    pub fn from_offset(data: Vec<T>, offset: i64, limit: i64, total: Option<i64>) -> Self {
        let has_more = data.len() as i64 >= limit;
        let cursor = if has_more {
            let next_offset = offset + limit;
            let encoded = cursor_encode(&format!(r#"{{"offset":{next_offset}}}"#));
            Some(encoded)
        } else {
            None
        };
        Self {
            data,
            cursor,
            total,
        }
    }
}

/// Pagination query params accepted by list endpoints.
#[derive(Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_page_limit")]
    pub limit: i64,
    pub cursor: Option<String>,
}

fn default_page_limit() -> i64 {
    100
}

impl PaginationParams {
    /// Parse the cursor to extract an offset. Returns 0 if no cursor or parse error.
    pub fn offset(&self) -> i64 {
        self.cursor
            .as_ref()
            .and_then(|c| cursor_decode(c))
            .and_then(|json| {
                serde_json::from_str::<serde_json::Value>(&json)
                    .ok()
                    .and_then(|v| v.get("offset")?.as_i64())
            })
            .unwrap_or(0)
    }

    /// Clamp limit to a maximum value.
    pub fn clamped_limit(&self, max: i64) -> i64 {
        self.limit.min(max).max(1)
    }
}

fn cursor_encode(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

fn cursor_decode(s: &str) -> Option<String> {
    urlencoding::decode(s)
        .ok()
        .map(std::borrow::Cow::into_owned)
}

// ── Idempotency helpers ──────────────────────────────────────────────

/// Extract idempotency key from headers. Returns None if header not present.
/// Returns Err if header is present but invalid.
pub fn extract_idempotency_key(headers: &HeaderMap) -> Result<Option<String>, AppError> {
    match headers.get("Idempotency-Key").and_then(|v| v.to_str().ok()) {
        Some(k) if k.is_empty() || k.len() > 64 => Err(AppError::BadRequest(
            "Idempotency-Key must be 1-64 characters".into(),
        )),
        Some(k) => Ok(Some(k.to_string())),
        None => Ok(None),
    }
}

/// Check for an existing saga with this idempotency key.
/// Returns `Some(cached_response)` if the key was already used successfully,
/// or `Err(Conflict)` if running. A stale `failed` row is cleaned up for retry
/// inside the adapter, surfacing here as `Ok(None)`.
///
/// Thinned (A2a): the `sagas`-table SQL now lives in
/// `SagaExecutorRepo::idempotency_check` (reached via `IngestService`); this
/// wrapper only maps the domain outcome onto the handler's `AppError`.
pub async fn check_idempotency(
    state: &AppState,
    key: &str,
) -> Result<Option<serde_json::Value>, AppError> {
    match state
        .ingest_service
        .idempotency_check(key.to_string())
        .await
        .map_err(AppError::from)?
    {
        IdempotencyCheck::Completed(result) => Ok(Some(result)),
        IdempotencyCheck::Running => Err(AppError::Conflict("Request already in progress".into())),
        IdempotencyCheck::Fresh => Ok(None),
    }
}

/// Outcome of [`reserve_idempotency`].
pub enum IdempotencyReservation {
    /// This request won the key and MUST proceed, then call
    /// [`finalize_idempotency`] on success or [`release_idempotency`] on failure.
    Reserved,
    /// A previous request with this key already completed — return its result
    /// instead of doing the work again.
    Cached(serde_json::Value),
}

/// Atomically reserve `key` for this request before doing the (expensive,
/// non-idempotent) work.
///
/// Thinned (A2a): the atomic `INSERT ... ON CONFLICT DO NOTHING` reservation —
/// the fix for the double-ingest TOCTOU — now lives in
/// `SagaExecutorRepo::idempotency_reserve` (reached via `IngestService`). This
/// wrapper maps the domain outcome onto the handler's `AppError`, turning an
/// in-flight reservation into HTTP 409.
pub async fn reserve_idempotency(
    state: &AppState,
    key: &str,
    saga_type: &str,
    repo_name: &str,
) -> Result<IdempotencyReservation, AppError> {
    match state
        .ingest_service
        .idempotency_reserve(
            key.to_string(),
            saga_type.to_string(),
            repo_name.to_string(),
        )
        .await
        .map_err(AppError::from)?
    {
        DomainReservation::Reserved => Ok(IdempotencyReservation::Reserved),
        DomainReservation::Cached(v) => Ok(IdempotencyReservation::Cached(v)),
        DomainReservation::InProgress => {
            Err(AppError::Conflict("Request already in progress".into()))
        }
    }
}

/// Mark a reserved idempotency row `completed` and cache its response, so a
/// later retry of the same key returns the same result without re-doing work.
pub async fn finalize_idempotency(
    state: &AppState,
    key: &str,
    result: &serde_json::Value,
) -> Result<(), AppError> {
    state
        .ingest_service
        .idempotency_finalize(key.to_string(), result.clone())
        .await
        .map_err(AppError::from)
}

/// Best-effort release of a reservation after the work failed to even start,
/// so the key can be retried immediately instead of being stuck `running`.
/// Errors are logged by the adapter, not propagated.
pub async fn release_idempotency(state: &AppState, key: &str) {
    state
        .ingest_service
        .idempotency_release(key.to_string())
        .await;
}

#[cfg(test)]
mod idempotency_tests {
    use super::*;
    use akashic_domain::ports::SagaExecutorRepo;
    use akashic_store_pg::PgSagaExecutorRepo;
    use sqlx::postgres::PgPoolOptions;

    // Integration tests: require the dev PostgreSQL. Set DATABASE_URL to the
    // integration instance (user/password akashic / akashic_secret, db akashic,
    // port 5432). The fallback splits the credentials via format placeholders to
    // match the convention in auth/mod.rs (avoids a literal user:password@ URL).
    async fn test_pool() -> sqlx::PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            format!(
                "postgres://{}:{}@localhost:5432/akashic",
                "akashic", "akashic_secret"
            )
        });
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("DATABASE_URL must point at the integration PostgreSQL");
        // Self-contained: ensure the sagas table (no vector columns), matching
        // db::pg::init_schema, so the test does not depend on a prior boot.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sagas ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 saga_type TEXT NOT NULL, \
                 idempotency_key TEXT UNIQUE, \
                 repo_name TEXT NOT NULL, \
                 status TEXT NOT NULL DEFAULT 'running', \
                 context_json JSONB NOT NULL DEFAULT '{}', \
                 result_json JSONB, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT now() \
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn purge(pg: &sqlx::PgPool, key: &str) {
        sqlx::query("DELETE FROM sagas WHERE idempotency_key = $1")
            .bind(key)
            .execute(pg)
            .await
            .unwrap();
    }

    fn is_reserved(r: &anyhow::Result<DomainReservation>) -> bool {
        matches!(r, Ok(DomainReservation::Reserved))
    }
    fn is_in_progress(r: &anyhow::Result<DomainReservation>) -> bool {
        matches!(r, Ok(DomainReservation::InProgress))
    }

    /// Regression for audit ingestion.rs:231: the atomic reservation (now in
    /// `PgSagaExecutorRepo::idempotency_reserve`) must let exactly ONE of two
    /// concurrent same-key requests proceed; the other is turned away. (The old
    /// check-then-record let both pass and each spawn a full ingest.)
    #[tokio::test]
    async fn concurrent_same_key_reserves_exactly_once() {
        let pg = test_pool().await;
        let repo = PgSagaExecutorRepo::new(pg.clone());
        let key = "test-idem-concurrent-key";
        purge(&pg, key).await;

        let (a, b) = tokio::join!(
            repo.idempotency_reserve(key, "trigger_ingest", "test-repo"),
            repo.idempotency_reserve(key, "trigger_ingest", "test-repo"),
        );
        let reserved = [is_reserved(&a), is_reserved(&b)]
            .into_iter()
            .filter(|x| *x)
            .count();
        let in_progress = [is_in_progress(&a), is_in_progress(&b)]
            .into_iter()
            .filter(|x| *x)
            .count();
        assert_eq!(reserved, 1, "exactly one concurrent request must reserve");
        assert_eq!(
            in_progress, 1,
            "the other must be turned away as InProgress"
        );

        // After finalize, a later request with the same key replays the cached
        // response instead of doing the work again.
        let result = serde_json::json!({ "job_id": "j1", "status": "pending" });
        repo.idempotency_finalize(key, &result)
            .await
            .expect("idempotency_finalize");
        match repo
            .idempotency_reserve(key, "trigger_ingest", "test-repo")
            .await
        {
            Ok(DomainReservation::Cached(v)) => assert_eq!(v, result),
            _ => panic!("expected Cached(result) after finalize"),
        }

        purge(&pg, key).await;
    }

    /// A spawn failure releases the reservation so the key is immediately
    /// retryable, rather than being stuck `running` or cached as a false success.
    #[tokio::test]
    async fn release_frees_key_for_retry() {
        let pg = test_pool().await;
        let repo = PgSagaExecutorRepo::new(pg.clone());
        let key = "test-idem-release-key";
        purge(&pg, key).await;

        assert!(is_reserved(
            &repo.idempotency_reserve(key, "reingest", "r").await
        ));
        // A second reserve while still `running` is rejected.
        assert!(is_in_progress(
            &repo.idempotency_reserve(key, "reingest", "r").await
        ));
        // Releasing (simulating a failed pipeline.start) frees the key.
        repo.idempotency_release(key).await;
        assert!(
            is_reserved(&repo.idempotency_reserve(key, "reingest", "r").await),
            "after release the key must be reservable again"
        );

        purge(&pg, key).await;
    }
}
