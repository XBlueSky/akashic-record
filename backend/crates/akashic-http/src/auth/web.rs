use axum::{
    Router,
    extract::Query,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json, Redirect},
    routing::{get, post},
};
use std::time::{Duration, Instant};
use tracing::warn;

use akashic_context::AppState;
use akashic_identity::types::{CallbackQuery, PendingState};

/// Public routes (no auth required).
pub fn public_router() -> Router<AppState> {
    Router::new()
        .route("/auth/web/login", get(web_login))
        .route("/auth/web/callback", get(web_callback))
}

/// Protected routes (behind auth middleware).
pub fn protected_router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/me", get(auth_me))
        .route("/api/v1/auth/logout", post(auth_logout))
}

/// GET /auth/web/login query parameters.
#[derive(serde::Deserialize)]
struct WebLoginQuery {
    /// Task 4 (MCP OAuth): same-origin path to return to once the session
    /// cookie is set — e.g. the backend's own `/oauth/authorize?...` URL, so
    /// a client that hit `/oauth/authorize` with no session resumes the
    /// authorization-code flow after logging in instead of landing on the
    /// frontend home. Validated by [`is_same_origin_path`]; an invalid value
    /// is a 400, not a silently-dropped `None` — a malformed/malicious
    /// `next` should never be waved through.
    next: Option<String>,
}

/// True iff `next` is a same-origin, relative path: never an absolute URL, a
/// protocol-relative `//host` (the browser resolves that against a
/// DIFFERENT origin), or a `/\host` path (some browsers normalize a leading
/// backslash to a second slash, the same escape by a different spelling).
/// This is an allowlist, not a denylist — anything `url::Url` can parse as
/// absolute is rejected too, so this doesn't rely on enumerating tricks.
fn is_same_origin_path(next: &str) -> bool {
    if !next.starts_with('/') || next.starts_with("//") || next.starts_with("/\\") {
        return false;
    }
    url::Url::parse(next).is_err()
}

/// GET /auth/web/login — redirect to GitLab OAuth with web callback URI.
async fn web_login(
    State(state): State<AppState>,
    Query(q): Query<WebLoginQuery>,
) -> impl IntoResponse {
    let next = match q.next {
        Some(n) if is_same_origin_path(&n) => Some(n),
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_next",
                    "error_description": "next must be a same-origin path, e.g. /oauth/authorize?...",
                })),
            )
                .into_response();
        }
        None => None,
    };

    let state_param = uuid::Uuid::new_v4().to_string();

    state.auth_store.pending_states.insert(
        state_param.clone(),
        PendingState {
            expires_at: Instant::now() + Duration::from_mins(5),
            next,
        },
    );

    let gitlab_url = &state.config.gitlab_url;
    let client_id = &state.config.gitlab_app_id;
    let redirect_uri = &state.config.gitlab_web_redirect_uri;

    let authorize_url = format!(
        "{gitlab_url}/oauth/authorize\
         ?client_id={client_id}\
         &redirect_uri={redirect_uri}\
         &response_type=code\
         &state={state_param}\
         &scope=read_user+read_repository+read_api"
    );

    Redirect::temporary(&authorize_url).into_response()
}

/// GET /auth/web/callback — exchange code, set HttpOnly cookie, redirect to frontend.
async fn web_callback(
    State(state): State<AppState>,
    Query(params): Query<CallbackQuery>,
) -> impl IntoResponse {
    // Thinned (Phase2-GW): CSRF check + token exchange + user fetch + session
    // creation live in AuthService::complete_web_login (which routes GitLab HTTP
    // through the gateway). The handler keeps only the cookie + redirect.
    use akashic_domain::ports::gitlab::WebLoginError;
    let outcome = match state
        .auth_service
        .complete_web_login(params.code, params.state)
        .await
    {
        Ok(o) => o,
        Err(WebLoginError::InvalidState) => {
            return (
                StatusCode::BAD_REQUEST,
                "Invalid or expired state parameter",
            )
                .into_response();
        }
        Err(WebLoginError::Upstream(e)) => {
            warn!(%e, "Web OAuth: upstream failure");
            return (
                StatusCode::BAD_GATEWAY,
                "Failed to authenticate with GitLab",
            )
                .into_response();
        }
        Err(WebLoginError::Internal(e)) => {
            warn!(%e, "Web OAuth: session persistence failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Set HttpOnly cookie and redirect. Task 4 (MCP OAuth): if the pending
    // state carried a validated same-origin `next` (the user arrived via
    // `/oauth/authorize` with no session), land back there — on THIS
    // backend, not the frontend — so the authorization-code flow resumes;
    // otherwise fall back to `frontend_url`, the pre-Task-4 behavior.
    // Re-validated here too: `PendingState.next` is only ever constructed
    // already-validated (`web_login`), so this should never actually
    // reject anything live — it's a second gate against a future regression
    // at the point of insertion, not the primary defense.
    let location = match outcome.next.as_deref() {
        Some(next) if is_same_origin_path(next) => {
            format!(
                "{}{next}",
                state.config.public_base_url.trim_end_matches('/')
            )
        }
        _ => state.config.frontend_url.clone(),
    };

    let cookie = build_session_cookie(
        state.config.cookie_secure,
        &state.config.frontend_url,
        &outcome.api_key,
        outcome.ttl_secs,
    );

    let mut headers = HeaderMap::new();
    let cookie_val = match cookie.parse() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let location_val = match location.parse() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    headers.insert(header::SET_COOKIE, cookie_val);
    headers.insert(header::LOCATION, location_val);

    (StatusCode::FOUND, headers).into_response()
}

/// GET /api/v1/auth/me — return current user info from session.
async fn auth_me(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let api_key = extract_session_from_cookie(&headers);
    let api_key = match api_key {
        Some(k) => k,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Not logged in"})),
            )
                .into_response();
        }
    };

    match state.auth_store.validate_session(&api_key).await {
        Some((user_info, _, _)) => Json(serde_json::json!({
            "username": user_info.username,
            "avatar_url": user_info.avatar_url,
        }))
        .into_response(),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Session expired"})),
        )
            .into_response(),
    }
}

/// POST /api/v1/auth/logout — clear cookie and remove session.
async fn auth_logout(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(api_key) = extract_session_from_cookie(&headers) {
        state.auth_store.remove_session(&api_key).await;
    }

    let clear_cookie = build_session_cookie(
        state.config.cookie_secure,
        &state.config.frontend_url,
        "",
        0,
    );

    let mut resp_headers = HeaderMap::new();
    if let Ok(v) = clear_cookie.parse() {
        resp_headers.insert(header::SET_COOKIE, v);
    }

    (
        StatusCode::OK,
        resp_headers,
        Json(serde_json::json!({"ok": true})),
    )
        .into_response()
}

/// Extract the hostname (without port) from a URL, for use as cookie Domain.
fn extract_domain(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default()
}

/// Build the `ak_session` Set-Cookie value with attributes that must match
/// between login (set) and logout (clear) for clear to actually invalidate
/// the prior set on Chrome / Firefox. The single source of truth.
pub(crate) fn build_session_cookie(
    cookie_secure: bool,
    frontend_url: &str,
    value: &str,
    max_age_secs: u64,
) -> String {
    let secure = if cookie_secure { "; Secure" } else { "" };
    format!(
        "ak_session={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age_secs}{secure}; Domain={}",
        extract_domain(frontend_url),
    )
}

/// Extract api_key from the ak_session cookie.
fn extract_session_from_cookie(headers: &HeaderMap) -> Option<String> {
    super::session_cookie_value(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_attribute<'a>(cookie: &'a str, prefix: &str) -> Option<&'a str> {
        cookie.split("; ").find(|part| part.starts_with(prefix))
    }

    #[test]
    fn test_session_cookie_set_clear_attribute_parity() {
        for cookie_secure in [true, false] {
            let frontend = "https://akashic.example";
            let set = build_session_cookie(cookie_secure, frontend, "session-key", 86400);
            let clear = build_session_cookie(cookie_secure, frontend, "", 0);

            // Attributes that must match for the clear to invalidate the set.
            for prefix in ["HttpOnly", "SameSite=", "Path=", "Domain="] {
                assert_eq!(
                    extract_attribute(&set, prefix),
                    extract_attribute(&clear, prefix),
                    "{prefix} differs between set and clear (cookie_secure={cookie_secure})",
                );
            }

            // Secure is a flag with no value — assert presence parity.
            assert_eq!(
                set.contains("; Secure"),
                clear.contains("; Secure"),
                "Secure presence differs (cookie_secure={cookie_secure})",
            );
        }
    }

    // ── is_same_origin_path (Task 4: /auth/web/login?next= open-redirect guard) ─

    #[test]
    fn is_same_origin_path_accepts_relative_path() {
        assert!(is_same_origin_path(
            "/oauth/authorize?client_id=abc&state=xyz"
        ));
        assert!(is_same_origin_path("/path"));
    }

    #[test]
    fn is_same_origin_path_rejects_absolute_and_protocol_relative() {
        assert!(!is_same_origin_path("https://evil.com"));
        assert!(!is_same_origin_path("//evil.com"));
        assert!(!is_same_origin_path("http://x"));
    }

    #[test]
    fn is_same_origin_path_rejects_backslash_and_missing_leading_slash() {
        assert!(!is_same_origin_path("/\\evil.com"));
        assert!(!is_same_origin_path("evil.com"));
        assert!(!is_same_origin_path(""));
    }
}
