//! Task 8 (B3, docs-corpus): `POST /api/v1/docs/{repo}/derive/retry` —
//! re-triggers the derive job for a repo's latest published corpus version.
//!
//! Protected: requires at least Developer-level (30) GitLab access to
//! `repo`, mirroring `ingestion::reingest`'s per-repo authorization — like
//! `reingest`, this endpoint discards and rebuilds derived data (the corpus
//! Doc space) from scratch.
//!
//! `akashic-http/src/api/routes/docs_read.rs` (Task 10, the corpus read-side
//! routes) doesn't exist yet — this handler lives in its own small module
//! per the Task 8 brief.

use axum::{
    Json,
    extract::{Path, State},
    response::IntoResponse,
};
use serde::Serialize;
use uuid::Uuid;

use akashic_context::AppState;
use akashic_ingestion::ingestion::corpus::RetryDeriveOutcome;

use super::super::error::AppError;
use super::super::extractors;
use super::require_gitlab_access;

#[derive(Serialize)]
struct RetryDeriveResponse {
    version_id: Uuid,
    derive_job_id: Option<Uuid>,
}

pub(super) async fn retry_derive(
    State(state): State<AppState>,
    Path(repo): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&repo)?;
    require_gitlab_access(&state, &headers, &repo, 30).await?;

    match state
        .corpus_ingest_service
        .retry_derive(&repo)
        .await
        .map_err(AppError::Internal)?
    {
        RetryDeriveOutcome::NotFound => Err(AppError::NotFound),
        RetryDeriveOutcome::Spawned {
            version_id,
            derive_job_id,
        } => Ok(Json(RetryDeriveResponse {
            version_id,
            derive_job_id,
        })),
    }
}
