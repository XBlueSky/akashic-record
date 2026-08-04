use axum::{
    Json,
    extract::{Path, State},
    response::IntoResponse,
};
use serde::Serialize;

use akashic_context::AppState;

use super::super::error::AppError;
use super::super::extractors;
use super::{extract_user_token, resolve_gitlab_access};

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct PermissionsResponse {
    access_level: i32,
    can_edit: bool,
    can_delete: bool,
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/repos
pub(super) async fn list_repos(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let repos = state.repo_service.list_repos().await?;
    Ok(Json(repos))
}

/// GET /api/v1/repos/:name/branches
pub(super) async fn list_branches(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let branches = state.repo_service.list_branches(name).await?;
    Ok(Json(branches))
}

/// GET /api/v1/repos/:name/permissions — returns current user's GitLab access level.
pub(super) async fn get_permissions(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(repo_name): Path<String>,
) -> Result<Json<PermissionsResponse>, AppError> {
    let gitlab_token = match extract_user_token(&state, &headers).await {
        Some(t) => t,
        None => {
            return Ok(Json(PermissionsResponse {
                access_level: 0,
                can_edit: false,
                can_delete: false,
            }));
        }
    };

    let perms = state
        .repo_service
        .get_repo_permissions(repo_name, Some(gitlab_token))
        .await?;

    Ok(Json(PermissionsResponse {
        access_level: perms.access_level,
        can_edit: perms.can_edit,
        can_delete: perms.can_delete,
    }))
}

/// GET /api/v1/repos/{name}/detail — repo metadata + source config + latest job
pub(super) async fn repo_detail(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let detail = state.repo_service.get_repo_detail(name).await?;
    Ok(Json(detail))
}

/// DELETE /api/v1/repos/{name} — permanently delete a repo and all its data
pub(super) async fn delete_repo(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&name)?;

    // Authorization: deleting a repo permanently destroys ALL of its data, so
    // require the same Maintainer+ GitLab access as deleting a note. Without
    // this, any authenticated user could delete any repo — the frontend
    // `canDelete` gate is advisory only and trivially bypassed with a direct
    // request.
    let gitlab_token = extract_user_token(&state, &headers)
        .await
        .ok_or(AppError::Unauthorized)?;
    let access_level = resolve_gitlab_access(&state, &gitlab_token, &name).await?;
    if access_level < 40 {
        return Err(AppError::Forbidden(
            "Maintainer+ access required to delete a repository".into(),
        ));
    }

    state.repo_service.delete_repo(name.clone()).await?;

    tracing::info!(%name, "Repository deleted");

    let _ = state
        .event_tx
        .send(super::super::events::AppEvent::ReposChanged);

    Ok(Json(serde_json::json!({
        "deleted": true,
        "repo_name": name
    })))
}

/// Permanently delete all of a repo's data from PostgreSQL (in a single
/// transaction) and Neo4j. This function is kept for integration tests that
/// exercise the deletion order directly.
///
/// Handlers should use `state.repo_service.delete_repo(name)` instead.
pub async fn delete_repo_data(state: &AppState, name: &str) -> Result<(), AppError> {
    state
        .repo_service
        .delete_repo(name.to_string())
        .await
        .map_err(AppError::from)
}
