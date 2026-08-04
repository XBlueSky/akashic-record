//! Task 6 (B1, docs corpus): repo-scoped publish-token issue/list/revoke.
//!
//! Publish tokens (`akp_<32hex>`) authorize the docs-publish endpoint for
//! exactly one repo. Mirrors the MCP token self-management endpoints
//! (`auth::account::{list_my_tokens, revoke_my_token}`) but scoped to a repo
//! instead of a GitLab user, and gated on GitLab repo permission at issue
//! time rather than session ownership alone.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

use akashic_context::AppState;

use super::super::error::AppError;
use super::super::extractors;
use super::require_gitlab_access;

/// Minimum GitLab access level required to issue a publish token for a repo.
/// Mirrors the Developer-level threshold `ingestion::trigger_ingest` uses for
/// GitLab-sourced writes (`require_gitlab_access(.., 30)`) — issuing a
/// docs-publish token is itself a repo-write capability grant, so it gets
/// the same bar as triggering an ingest.
const PUBLISH_TOKEN_MIN_ACCESS_LEVEL: i64 = 30;

// ── Request / Response types ────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct IssueTokenRequest {
    repo_name: String,
}

#[derive(Serialize)]
pub(super) struct IssueTokenResponse {
    id: uuid::Uuid,
    /// Shown once, never re-derivable — the caller must persist it now.
    token: String,
    repo_name: String,
}

#[derive(Serialize)]
struct PublishTokenSummary {
    id: uuid::Uuid,
    repo_name: String,
    created_at: chrono::DateTime<chrono::Utc>,
    last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// POST /api/v1/docs-tokens `{ "repo_name": "..." }`
///
/// Precondition: the caller must hold Developer+ GitLab access on
/// `repo_name` — the same `RepoService::get_repo_permissions` gateway call
/// that backs `GET /api/v1/repos/:name/permissions`, reused here via
/// `require_gitlab_access`. Records an `audit_log` entry on success (best
/// effort — an audit-write failure is logged but does not fail the issue).
pub(super) async fn issue_token(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<IssueTokenRequest>,
) -> Result<impl IntoResponse, AppError> {
    extractors::validate_repo_name(&body.repo_name)?;

    require_gitlab_access(
        &state,
        &headers,
        &body.repo_name,
        PUBLISH_TOKEN_MIN_ACCESS_LEVEL,
    )
    .await?;

    let session = crate::auth::oauth_device::extract_session_user(&state, &headers)
        .await
        .ok_or(AppError::Unauthorized)?;

    let (id, token) = state
        .auth_store
        .issue_publish_token(&body.repo_name, &session.user_login)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    if let Err(e) = state
        .auth_store
        .record_publish_token_audit(
            session.user_id,
            &format!("web_session:{}", session.user_login),
            "web_session",
            "issue_publish_token",
            &body.repo_name,
        )
        .await
    {
        warn!(event = "publish_token_audit_write_failed", error = %e, action = "issue");
    }

    Ok(Json(IssueTokenResponse {
        id,
        token,
        repo_name: body.repo_name,
    }))
}

/// GET /api/v1/docs-tokens — list the caller's own publish tokens. Metadata
/// only; the plaintext and the stored hash are never returned.
pub(super) async fn list_tokens(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let session = crate::auth::oauth_device::extract_session_user(&state, &headers)
        .await
        .ok_or(AppError::Unauthorized)?;

    let rows = state
        .auth_store
        .list_publish_tokens(&session.user_login)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    let summaries: Vec<PublishTokenSummary> = rows
        .into_iter()
        .map(|r| PublishTokenSummary {
            id: r.id,
            repo_name: r.repo_name,
            created_at: r.created_at,
            last_used_at: r.last_used_at,
            expires_at: r.expires_at,
            revoked_at: r.revoked_at,
        })
        .collect();

    Ok(Json(summaries))
}

/// POST /api/v1/docs-tokens/:id/revoke
///
/// Ownership-gated: 404s (not 403) when the token doesn't exist or belongs
/// to another user, mirroring `auth::account::revoke_my_token` so a probe
/// can't distinguish "not yours" from "doesn't exist". Records an
/// `audit_log` entry on success (best effort).
pub(super) async fn revoke_token(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id_str): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let session = crate::auth::oauth_device::extract_session_user(&state, &headers)
        .await
        .ok_or(AppError::Unauthorized)?;

    let token_id: uuid::Uuid = id_str
        .parse()
        .map_err(|_| AppError::BadRequest("invalid token id".into()))?;

    let owner = state
        .auth_store
        .check_publish_token_ownership(token_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    match owner {
        Some(created_by) if created_by == session.user_login => {}
        _ => {
            warn!(
                event = "publish_token_revoke_unauthorized_or_missing",
                token_id = %token_id,
                requesting_user = %session.user_login,
            );
            return Err(AppError::NotFound);
        }
    }

    state
        .auth_store
        .revoke_publish_token(token_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    if let Err(e) = state
        .auth_store
        .record_publish_token_audit(
            session.user_id,
            &format!("web_session:{}", session.user_login),
            "web_session",
            "revoke_publish_token",
            &token_id.to_string(),
        )
        .await
    {
        warn!(event = "publish_token_audit_write_failed", error = %e, action = "revoke");
    }

    Ok(StatusCode::NO_CONTENT)
}
