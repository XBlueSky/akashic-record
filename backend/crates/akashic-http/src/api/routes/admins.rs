//! A2d-3 Admin trust-chain endpoints.
//!
//! All three routes are `require_admin`-gated.
//!
//! - `GET  /api/v1/admins`          — list current admin set (roots + delegated)
//! - `POST /api/v1/admins/grant`    — grant admin to a username
//! - `POST /api/v1/admins/revoke`   — revoke admin (cascade via CTE)

use axum::{Json, extract::State, response::IntoResponse};
use serde::{Deserialize, Serialize};
use tracing::info;

use akashic_context::AppState;

use super::super::error::AppError;
use super::{require_admin, username_is_admin};

// ── Request / Response types ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct AdminTargetRequest {
    username: String,
}

#[derive(Serialize)]
struct AdminEntryResponse {
    username: String,
    granter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    granted_at: Option<String>,
}

#[derive(Serialize)]
struct GrantResponse {
    granted: String,
}

#[derive(Serialize)]
struct RevokeResponse {
    revoked: u64,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// GET /api/v1/admins — list current admin set (env roots + delegated).
pub(super) async fn list_admins(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&state, &headers).await?;

    let entries = state
        .admin_grant_repo
        .list_admins(&state.config.admin_users)
        .await
        .map_err(AppError::Internal)?;

    let response: Vec<AdminEntryResponse> = entries
        .into_iter()
        .map(|e| AdminEntryResponse {
            username: e.username,
            granter: e.granter,
            granted_at: e.granted_at.map(|t| t.to_rfc3339()),
        })
        .collect();

    Ok(Json(response))
}

/// POST /api/v1/admins/grant `{ "username": "..." }` — grant admin to a user.
///
/// Rejects grants targeting env roots (they're already admin; extra rows
/// are harmless but confusing, so we reject them with Conflict).
pub(super) async fn grant_admin(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<AdminTargetRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_admin(&state, &headers).await?;

    // Prevent granting to env roots (immutable anchors; no grant row needed).
    if username_is_admin(&body.username, &state.config.admin_users) {
        return Err(AppError::Conflict(format!(
            "'{}' is already an env-root admin; no grant needed",
            body.username
        )));
    }

    let _grant_id = state
        .admin_grant_repo
        .grant(&body.username, &caller)
        .await
        .map_err(AppError::Internal)?;

    info!(
        event = "admin_grant",
        caller = %caller,
        grantee = %body.username,
        "admin grant recorded",
    );

    Ok(Json(GrantResponse {
        granted: body.username,
    }))
}

/// POST /api/v1/admins/revoke `{ "username": "..." }` — revoke admin.
///
/// A root may revoke any grant; a non-root may only revoke grants they made.
/// Returns 404 if there are no active grants; 403 if a non-root targets grants
/// they didn't make.
pub(super) async fn revoke_admin(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<AdminTargetRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_admin(&state, &headers).await?;

    // Env roots cannot be revoked via this endpoint (they're config, not grants).
    if username_is_admin(&body.username, &state.config.admin_users) {
        return Err(AppError::Forbidden(format!(
            "cannot revoke an env-root admin ('{}') via this endpoint; \
             update AKASHIC_ADMIN_USERS in the environment instead",
            body.username
        )));
    }

    let caller_is_root = username_is_admin(&caller, &state.config.admin_users);

    let revoked = state
        .admin_grant_repo
        .revoke(&body.username, &caller, caller_is_root, &caller)
        .await
        .map_err(AppError::Internal)?;

    if revoked == 0 {
        if caller_is_root {
            // Root tried but there are no active grants — 404.
            return Err(AppError::NotFound);
        }
        // Non-root: either no grants exist or none were made by caller — 403.
        return Err(AppError::Forbidden(format!(
            "no active grants for '{}' made by '{caller}'; \
             only the granter or an env-root admin can revoke",
            body.username
        )));
    }

    info!(
        event = "admin_revoke",
        caller = %caller,
        grantee = %body.username,
        revoked_count = revoked,
        "admin grants revoked",
    );

    Ok(Json(RevokeResponse { revoked }))
}
