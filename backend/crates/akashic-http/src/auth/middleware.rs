use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

use akashic_context::AppState;

/// Axum middleware that enforces Bearer token authentication.
///
/// This middleware is only applied to the protected router (main.rs scopes it),
/// so no path-based check is needed — `/auth/*` routes are never seen here.
///
/// Set `DEV_SKIP_AUTH=1` to bypass auth for local development.
pub async fn require_auth(State(state): State<AppState>, request: Request, next: Next) -> Response {
    // Dev bypass — skip auth in debug builds only when DEV_SKIP_AUTH is set.
    // NOTE: this also bypasses B3 quota because CURRENT_ACTOR is not set,
    // so requests via this path are uncapped. Acceptable for local dev.
    #[cfg(debug_assertions)]
    if std::env::var("DEV_SKIP_AUTH").is_ok() {
        tracing::warn!("DEV_SKIP_AUTH: bypassing authentication (debug build only)");
        return next.run(request).await;
    }

    // Extract API key: check Authorization header first, then ak_session cookie
    // (via the canonical shared parser).
    let token = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(String::from)
        .or_else(|| crate::auth::session_cookie_value(request.headers()));

    let token = match token {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Missing or invalid Authorization header",
                    "hint": "Visit /auth/web/login in a browser or /auth/login for CLI"
                })),
            )
                .into_response();
        }
    };

    // Validate session
    match state.auth_store.validate_session(&token).await {
        Some((user_info, gitlab_token, gitlab_user_id)) => {
            // Post-B4: use the real numeric id from sessions.gitlab_user_id.
            // Legacy sessions (gitlab_user_id IS NULL) resolve the real id via
            // the GitLab API; if that fails we reject rather than collapse to a
            // shared sentinel id 0 (which would merge distinct users into one
            // quota bucket and audit identity).
            let Some(user_id) = crate::auth::oauth_device::resolve_gitlab_user_id(
                &state,
                gitlab_user_id,
                &gitlab_token,
            )
            .await
            else {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": "Could not resolve GitLab user identity for this session",
                        "hint": "Log out and re-authenticate via /auth/web/login"
                    })),
                )
                    .into_response();
            };
            let actor = akashic_quota::ActorIdentity {
                user_id,
                token_id: format!("ak_session:{}", user_info.username),
                auth_method: akashic_kernel::AuthMethod::DeviceFlow,
            };
            akashic_quota::CURRENT_ACTOR
                .scope(Some(actor), next.run(request))
                .await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Invalid or expired API key",
                "hint": "Visit /auth/login in a browser to re-authenticate"
            })),
        )
            .into_response(),
    }
}
