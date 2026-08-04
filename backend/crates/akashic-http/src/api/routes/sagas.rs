use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use serde::Deserialize;

use akashic_context::AppState;

use super::super::error::AppError;
use super::super::extractors;

// ── Request types ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct SagaListQuery {
    status: Option<String>,
}

// ── Handlers ────────────────────────────────────────────────────────────────

pub(super) async fn list_sagas_handler(
    Path(name): Path<String>,
    Query(params): Query<SagaListQuery>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&name)?;
    let sagas = state
        .curation_service
        .list_sagas(name, params.status)
        .await?;
    Ok(Json(sagas))
}

pub(super) async fn get_saga_handler(
    Path((name, saga_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&name)?;
    let uuid = uuid::Uuid::parse_str(&saga_id)
        .map_err(|_| AppError::BadRequest("invalid saga UUID".into()))?;
    let (saga, timeline) = state
        .curation_service
        .get_saga_timeline(uuid)
        .await
        .map_err(|_| AppError::NotFound)?;
    Ok(Json(serde_json::json!({
        "saga": saga,
        "timeline": timeline,
    })))
}
