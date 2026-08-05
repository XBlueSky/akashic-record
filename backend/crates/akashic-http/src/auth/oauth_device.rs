//! RFC 8628 OAuth 2.0 Device Authorization Grant endpoints.
//!
//! Endpoint mapping:
//!   POST /oauth/device_authorization  → §3.1 client requests codes
//!   POST /oauth/token                 → §3.4 client polls for bearer
//!   GET  /device                      → user-facing code-entry page
//!   POST /device/verify               → user-facing code submission (auth required)
//!   POST /device/approve              → user-facing final consent (auth required)
//!
//! Tasks 8–11 add the handlers. Task 7 ships only the helpers used by them.

use axum::{
    Json, Router,
    extract::{Form, FromRequest, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use rand::{Rng, RngCore};
use tracing::{info, warn};

use crate::auth::types::{
    DeviceAuthorizationRequest, DeviceAuthorizationResponse, DeviceTokenRequest,
    DeviceUserCodeRequest,
};
use akashic_context::AppState;

const USER_CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXYZ23456789";

/// Generate an 8-char hyphenated user_code: `XXXX-XXXX`, alphabet
/// `BCDFGHJKLMNPQRSTVWXYZ23456789` (no vowels, no 0/1/I/O).
pub fn generate_user_code() -> String {
    let mut rng = rand::thread_rng();
    let mut s = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            s.push('-');
        }
        let idx: usize = rng.gen_range(0..USER_CODE_ALPHABET.len());
        s.push(USER_CODE_ALPHABET[idx] as char);
    }
    s
}

/// Generate a 64-char lowercase hex device_code (256 bits of entropy).
pub fn generate_device_code() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Build the public device-flow router. Routes:
/// - POST /oauth/device_authorization
/// - POST /oauth/token  (dispatches device-code vs authorization_code grant)
/// - GET  /device       (filled in by Task 10)
pub fn public_router() -> Router<AppState> {
    Router::new()
        .route("/oauth/device_authorization", post(device_authorization))
        .route("/oauth/token", post(oauth_token_dispatch))
        .route("/device", get(device_page))
}

/// `POST /oauth/token` dispatcher.
///
/// Task 3's metadata advertises a single `token_endpoint`, and axum can only
/// bind one handler per (method, path) — so BOTH grant types this server
/// supports share this one route: the RFC 8628 device-code grant (this
/// module, always JSON-bodied) and Task 4's RFC 6749 `authorization_code` +
/// PKCE grant (`mcp_oauth::token_exchange`, form-urlencoded per spec).
///
/// Dispatch is by `Content-Type`, decided BEFORE either body extractor runs
/// (a request body can only be consumed once): a form-urlencoded body is
/// unambiguously the new grant — the device flow has always required
/// `application/json` — so every pre-Task-4 caller hits the exact same
/// `Json<DeviceTokenRequest>` extraction path as before, byte-for-byte.
async fn oauth_token_dispatch(State(state): State<AppState>, req: Request) -> Response {
    let is_form = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/x-www-form-urlencoded"));

    if is_form {
        return match Form::<crate::auth::mcp_oauth::AuthCodeTokenRequest>::from_request(req, &state)
            .await
        {
            Ok(Form(form)) => crate::auth::mcp_oauth::token_exchange(&state, form).await,
            Err(_) => token_err(StatusCode::BAD_REQUEST, "invalid_request"),
        };
    }

    match Json::<DeviceTokenRequest>::from_request(req, &state).await {
        Ok(Json(json_req)) => device_token(State(state), Json(json_req))
            .await
            .into_response(),
        Err(_) => token_err(StatusCode::BAD_REQUEST, "invalid_request"),
    }
}

async fn device_authorization(
    State(state): State<AppState>,
    Json(req): Json<DeviceAuthorizationRequest>,
) -> impl IntoResponse {
    match state.auth_service.device_authorization(req.client_id).await {
        Ok(resp_val) => {
            // Map service JSON to the typed response struct the handler returns.
            let resp = DeviceAuthorizationResponse {
                device_code: resp_val["device_code"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                user_code: resp_val["user_code"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                verification_uri: resp_val["verification_uri"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                verification_uri_complete: resp_val["verification_uri_complete"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                expires_in: resp_val["expires_in"].as_i64().unwrap_or(600),
                interval: resp_val["interval"].as_i64().unwrap_or(5),
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(akashic_domain::DomainError::BadRequest(_)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_client"})),
        )
            .into_response(),
        Err(e) => {
            warn!(event = "device_authorization_failed", error = %e);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "server_error"})),
            )
                .into_response()
        }
    }
}

const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

async fn device_token(
    State(state): State<AppState>,
    Json(req): Json<DeviceTokenRequest>,
) -> impl IntoResponse {
    if req.grant_type != DEVICE_CODE_GRANT {
        return token_err(StatusCode::BAD_REQUEST, "unsupported_grant_type");
    }

    // The Tx-atomic SELECT FOR UPDATE + UPDATE lives inside
    // DeviceFlowRepo::poll_device_token (service delegates to it).
    let resp_val = match state.auth_service.device_token(req.device_code).await {
        Ok(v) => v,
        Err(e) => {
            warn!(event = "device_token_service_failed", error = %e);
            return token_err(StatusCode::INTERNAL_SERVER_ERROR, "server_error");
        }
    };

    // Map the service's JSON response to HTTP status + typed struct.
    if let Some(err_code) = resp_val["error"].as_str() {
        let status = match err_code {
            "unsupported_grant_type"
            | "invalid_grant"
            | "expired_token"
            | "access_denied"
            | "slow_down"
            | "authorization_pending" => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        return token_err(
            status,
            match err_code {
                "invalid_grant" => "invalid_grant",
                "expired_token" => "expired_token",
                "access_denied" => "access_denied",
                "slow_down" => "slow_down",
                "authorization_pending" => "authorization_pending",
                _ => "server_error",
            },
        );
    }

    if let Some(token) = resp_val["access_token"].as_str() {
        (
            StatusCode::OK,
            Json(crate::auth::types::DeviceTokenResponse {
                access_token: token.to_string(),
                token_type: "Bearer",
                expires_in: resp_val["expires_in"].as_i64().unwrap_or(7_776_000),
            }),
        )
            .into_response()
    } else {
        token_err(StatusCode::INTERNAL_SERVER_ERROR, "server_error")
    }
}

fn token_err(status: StatusCode, code: &'static str) -> axum::response::Response {
    (status, Json(serde_json::json!({"error": code}))).into_response()
}

/// Minimal HTML escaper for interpolating untrusted text into element content
/// or double-quoted attribute values. Used on every value reflected into the
/// device-flow HTML pages so a crafted `?user_code=` cannot break out of the
/// attribute and inject script.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

async fn device_page(
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let prefilled = html_escape(q.get("user_code").map_or("", String::as_str));
    let html = format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Authorize Akashic MCP</title>
<style>body{{font-family:system-ui;max-width:480px;margin:4rem auto;padding:1rem}}
input{{font-size:1.5rem;padding:.5rem;width:100%;font-family:monospace;text-align:center;letter-spacing:.2em}}
button{{font-size:1rem;padding:.5rem 1.5rem;margin-top:1rem}}
form{{margin-top:1rem}}</style></head>
<body><h1>Authorize Akashic MCP</h1>
<p>Enter the code shown in your client (e.g. <code>ABCD-1234</code>):</p>
<form method="post" action="/device/verify">
  <input name="user_code" value="{prefilled}" autofocus required pattern="[A-Z2-9]{{4}}-[A-Z2-9]{{4}}" />
  <button type="submit">Continue</button>
</form>
<p><small>You will be asked to log in via GitLab if you haven't already.</small></p>
</body></html>"#
    );
    Html(html)
}

/// Build the auth-required device-flow router. Routes:
/// - POST /device/verify   (sets status='pre_approved', renders confirmation page)
/// - POST /device/approve  (filled in by Task 11)
///
/// Both routes require an `ak_session` cookie validated by the existing
/// `auth::middleware::require_auth` mounted by `auth::protected_auth_router`.
pub fn protected_router() -> Router<AppState> {
    Router::new()
        .route("/device/verify", post(device_verify))
        .route("/device/approve", post(device_approve))
}

async fn device_verify(
    State(state): State<AppState>,
    cookies: HeaderMap,
    Form(req): Form<DeviceUserCodeRequest>,
) -> impl IntoResponse {
    let user_code = req.user_code.trim().to_ascii_uppercase();
    let session = match extract_session_user(&state, &cookies).await {
        Some(s) => s,
        None => {
            return Html(
                r#"<p>Session expired. <a href="/auth/web/login">Log in</a> and try again.</p>"#,
            )
            .into_response();
        }
    };

    let updated = state
        .auth_service
        .device_verify(
            user_code.clone(),
            session.user_id,
            session.user_login.clone(),
        )
        .await;

    match updated {
        Ok(true) => {
            // Defense-in-depth: although user_code only reaches here after
            // matching a server-generated row, escape every reflected value so
            // this stays safe even if the match condition is ever loosened.
            let safe_code = html_escape(&user_code);
            let safe_login = html_escape(&session.user_login);
            let html = format!(
                r#"<!doctype html><body style="font-family:system-ui;max-width:480px;margin:4rem auto;padding:1rem">
<h1>Confirm authorization</h1>
<p>Code <code>{safe_code}</code> will authorize a new MCP token for <strong>{safe_login}</strong>.</p>
<form method="post" action="/device/approve">
<input type="hidden" name="user_code" value="{safe_code}" />
<button type="submit" style="font-size:1rem;padding:.5rem 1.5rem">Authorize</button>
</form></body>"#,
            );
            Html(html).into_response()
        }
        _ => Html(
            r#"<p>Code not recognized, expired, or already used. <a href="/device">Try again</a></p>"#
        ).into_response(),
    }
}

async fn device_approve(
    State(state): State<AppState>,
    cookies: HeaderMap,
    Form(req): Form<DeviceUserCodeRequest>,
) -> impl IntoResponse {
    let user_code = req.user_code.trim().to_ascii_uppercase();
    let session = match extract_session_user(&state, &cookies).await {
        Some(s) => s,
        None => {
            return Html(
                r#"<p>Session expired. <a href="/auth/web/login">Log in</a> and try again.</p>"#,
            )
            .into_response();
        }
    };

    let updated = state
        .auth_service
        .device_approve(user_code.clone(), session.user_id)
        .await;

    match updated {
        Ok(true) => {
            info!(event = "device_flow_approved", user_code = %user_code, user_id = session.user_id);
            Html(
                r#"<!doctype html><body style="font-family:system-ui;max-width:480px;margin:4rem auto;padding:1rem;text-align:center">
<h1>Done</h1><p>Authorization complete. You can close this window — your client will receive its token shortly.</p>
</body>"#
            ).into_response()
        }
        _ => Html(
            r#"<p>Authorization failed (code expired, already approved, or wrong user). <a href="/device">Restart</a></p>"#
        ).into_response(),
    }
}

/// Resolve the `ak_session` cookie's user. Prefers the cached `gitlab_user_id`
/// from the session row. For legacy sessions (gitlab_user_id IS NULL), falls back
/// to GitLab `/api/v4/user` API. Returns None on missing/invalid/expired session
/// or GitLab roundtrip failure.
pub(crate) struct SessionUser {
    pub user_id: i64,
    pub user_login: String,
}

/// Resolve the real numeric GitLab user id, preferring the cached value and
/// falling back to the GitLab `/api/v4/user` API for legacy sessions whose
/// `gitlab_user_id` column is NULL. Returns None on roundtrip failure.
///
/// Shared by `extract_session_user` and `auth::middleware::require_auth` so
/// that NO caller ever collapses a legacy session to a sentinel id (e.g. 0),
/// which would let distinct users share one identity bucket.
pub(crate) async fn resolve_gitlab_user_id(
    state: &AppState,
    gitlab_user_id: Option<i64>,
    gitlab_token: &str,
) -> Option<i64> {
    // Thinned (Phase2-GW): the GitLab `/api/v4/user` fallback now goes through
    // AuthService (which holds the gateway); the cached-id fast path is inside
    // the service.
    state
        .auth_service
        .resolve_gitlab_user_id(gitlab_user_id, gitlab_token.to_string())
        .await
        .ok()
        .flatten()
}

pub(crate) async fn extract_session_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<SessionUser> {
    let api_key = crate::auth::session_cookie_value(headers)?;
    let (info, gitlab_token, gitlab_user_id) = state.auth_store.validate_session(&api_key).await?;
    let user_id = resolve_gitlab_user_id(state, gitlab_user_id, &gitlab_token).await?;
    Some(SessionUser {
        user_id,
        user_login: info.username,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_code_shape_xxxx_xxxx_in_allowed_alphabet() {
        for _ in 0..200 {
            let c = generate_user_code();
            assert_eq!(c.len(), 9, "got {c}");
            assert_eq!(c.chars().nth(4), Some('-'));
            for (i, ch) in c.chars().enumerate() {
                if i == 4 {
                    continue;
                }
                assert!(
                    USER_CODE_ALPHABET.contains(&(ch as u8)),
                    "char {ch} at {i} not in alphabet"
                );
            }
        }
    }

    #[test]
    fn user_code_alphabet_excludes_vowels_and_confusables() {
        for forbidden in [b'A', b'E', b'I', b'O', b'U', b'0', b'1'] {
            assert!(
                !USER_CODE_ALPHABET.contains(&forbidden),
                "alphabet includes {forbidden}"
            );
        }
    }

    #[test]
    fn device_code_64_lowercase_hex() {
        for _ in 0..20 {
            let d = generate_device_code();
            assert_eq!(d.len(), 64);
            assert!(
                d.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
        }
    }

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    #[serial_test::serial]
    async fn device_authorization_inserts_pending_row_and_returns_codes() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let pg = akashic_test_support::test_pg_pool().await;

        let app = public_router().with_state(state);
        let req = Request::builder()
            .method("POST")
            .uri("/oauth/device_authorization")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"client_id":"claude-code"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["device_code"].as_str().unwrap().len(), 64);
        assert!(v["user_code"].as_str().unwrap().contains('-'));
        assert_eq!(v["expires_in"], 600);
        assert_eq!(v["interval"], 5);

        let device_code = v["device_code"].as_str().unwrap().to_string();
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM device_flow_pending WHERE device_code = $1")
                .bind(&device_code)
                .fetch_one(&pg)
                .await
                .unwrap();
        assert_eq!(n, 1);

        sqlx::query("DELETE FROM device_flow_pending WHERE device_code = $1")
            .bind(&device_code)
            .execute(&pg)
            .await
            .ok();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn device_authorization_rejects_unknown_client_id() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let app = public_router().with_state(state);
        let req = Request::builder()
            .method("POST")
            .uri("/oauth/device_authorization")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"client_id":"unknown-tool"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 400);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"], "invalid_client");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn device_token_unsupported_grant_type() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let app = public_router().with_state(state);
        let req = Request::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"grant_type":"password","device_code":"x"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 400);
        let v: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1024).await.unwrap())
                .unwrap();
        assert_eq!(v["error"], "unsupported_grant_type");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn device_token_unknown_device_code() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let app = public_router().with_state(state);
        let body =
            format!(r#"{{"grant_type":"{DEVICE_CODE_GRANT}","device_code":"deadbeef-not-real"}}"#);
        let req = Request::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 400);
        let v: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1024).await.unwrap())
                .unwrap();
        assert_eq!(v["error"], "invalid_grant");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn device_token_authorization_pending_then_approved_then_token() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let pg = akashic_test_support::test_pg_pool().await;

        // Seed a pending row directly.
        let device_code = generate_device_code();
        let user_code = generate_user_code();
        sqlx::query(
            "INSERT INTO device_flow_pending \
                (device_code, user_code, client_id, expires_at, status) \
             VALUES ($1, $2, 'claude-code', now() + INTERVAL '600 seconds', 'pending')",
        )
        .bind(&device_code)
        .bind(&user_code)
        .execute(&pg)
        .await
        .unwrap();

        let app = public_router().with_state(state.clone());
        let body =
            format!(r#"{{"grant_type":"{DEVICE_CODE_GRANT}","device_code":"{device_code}"}}"#);

        // First poll: pending.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
        let v: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1024).await.unwrap())
                .unwrap();
        assert_eq!(v["error"], "authorization_pending");

        // Approve directly.
        sqlx::query(
            "UPDATE device_flow_pending \
             SET status = 'approved', granted_user_id = 7777, granted_user_login = 'tester' \
             WHERE device_code = $1",
        )
        .bind(&device_code)
        .execute(&pg)
        .await
        .unwrap();

        // Wait > interval (5s) to dodge slow_down.
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;

        // Second poll: token issued.
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let v: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1024).await.unwrap())
                .unwrap();
        let token = v["access_token"].as_str().unwrap();
        assert!(token.starts_with("ak_"));
        assert_eq!(v["token_type"], "Bearer");
        assert_eq!(v["expires_in"], 7_776_000);

        // Cleanup
        sqlx::query("DELETE FROM device_flow_pending WHERE device_code=$1")
            .bind(&device_code)
            .execute(&pg)
            .await
            .ok();
        sqlx::query("DELETE FROM mcp_tokens WHERE user_id = 7777")
            .execute(&pg)
            .await
            .ok();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn device_token_expired_token() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let pg = akashic_test_support::test_pg_pool().await;

        let device_code = generate_device_code();
        sqlx::query(
            "INSERT INTO device_flow_pending \
                (device_code, user_code, client_id, expires_at, status) \
             VALUES ($1, $2, 'claude-code', now() - INTERVAL '1 second', 'pending')",
        )
        .bind(&device_code)
        .bind(generate_user_code())
        .execute(&pg)
        .await
        .unwrap();

        let app = public_router().with_state(state);
        let body =
            format!(r#"{{"grant_type":"{DEVICE_CODE_GRANT}","device_code":"{device_code}"}}"#);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1024).await.unwrap())
                .unwrap();
        assert_eq!(v["error"], "expired_token");

        sqlx::query("DELETE FROM device_flow_pending WHERE device_code=$1")
            .bind(&device_code)
            .execute(&pg)
            .await
            .ok();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn device_token_access_denied() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let pg = akashic_test_support::test_pg_pool().await;

        let device_code = generate_device_code();
        sqlx::query(
            "INSERT INTO device_flow_pending \
                (device_code, user_code, client_id, expires_at, status) \
             VALUES ($1, $2, 'claude-code', now() + INTERVAL '600 seconds', 'denied')",
        )
        .bind(&device_code)
        .bind(generate_user_code())
        .execute(&pg)
        .await
        .unwrap();

        let app = public_router().with_state(state);
        let body =
            format!(r#"{{"grant_type":"{DEVICE_CODE_GRANT}","device_code":"{device_code}"}}"#);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1024).await.unwrap())
                .unwrap();
        assert_eq!(v["error"], "access_denied");

        sqlx::query("DELETE FROM device_flow_pending WHERE device_code=$1")
            .bind(&device_code)
            .execute(&pg)
            .await
            .ok();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn device_token_slow_down_on_too_fast_poll() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let pg = akashic_test_support::test_pg_pool().await;

        let device_code = generate_device_code();
        sqlx::query(
            "INSERT INTO device_flow_pending \
                (device_code, user_code, client_id, expires_at, status) \
             VALUES ($1, $2, 'claude-code', now() + INTERVAL '600 seconds', 'pending')",
        )
        .bind(&device_code)
        .bind(generate_user_code())
        .execute(&pg)
        .await
        .unwrap();

        let app = public_router().with_state(state);
        let body =
            format!(r#"{{"grant_type":"{DEVICE_CODE_GRANT}","device_code":"{device_code}"}}"#);

        // First poll: sets last_polled_at.
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Immediate second poll: slow_down.
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1024).await.unwrap())
                .unwrap();
        assert_eq!(v["error"], "slow_down");

        // Verify interval_secs incremented by 5.
        let interval: i32 = sqlx::query_scalar(
            "SELECT interval_secs FROM device_flow_pending WHERE device_code = $1",
        )
        .bind(&device_code)
        .fetch_one(&pg)
        .await
        .unwrap();
        assert_eq!(interval, 10);

        sqlx::query("DELETE FROM device_flow_pending WHERE device_code=$1")
            .bind(&device_code)
            .execute(&pg)
            .await
            .ok();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn device_page_renders_form_with_prefill() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let app = public_router().with_state(state);
        let req = Request::builder()
            .method("GET")
            .uri("/device?user_code=ABCD-1234")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = String::from_utf8(
            axum::body::to_bytes(resp.into_body(), 8192)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            body.contains(r#"value="ABCD-1234""#),
            "prefilled value not in body"
        );
        assert!(
            body.contains(r#"action="/device/verify""#),
            "form action wrong"
        );
    }

    // ── oauth_token_dispatch (Task 4): Content-Type-based grant routing ─────

    /// A form-urlencoded `authorization_code` grant body must reach
    /// `mcp_oauth::token_exchange`, not the device grant. Asserted precisely:
    /// an unknown `code` under the authorization_code grant is `invalid_grant`
    /// (from `token_exchange`'s `consume_oauth_code` miss), which is
    /// distinguishable from what the DEVICE grant would say for a bogus
    /// `grant_type` (`unsupported_grant_type`) or an extraction failure
    /// (`invalid_request`) — so a 400/`invalid_grant` here can only mean
    /// dispatch reached the right handler.
    #[tokio::test]
    #[serial_test::serial]
    async fn oauth_token_dispatch_routes_form_body_to_authorization_code_grant() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let app = public_router().with_state(state);
        let req = Request::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(
                "grant_type=authorization_code&code=deadbeef&\
                 redirect_uri=http%3A%2F%2Fx&\
                 client_id=00000000-0000-0000-0000-000000000000&\
                 code_verifier=v",
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 400);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"], "invalid_grant");
    }

    /// A request with NO `Content-Type` header at all must 400
    /// `invalid_request` — never panic, never silently fall through to
    /// either grant's success path. Regression guard for the dispatcher's
    /// `Content-Type` sniff (`.and_then(|v| v.to_str().ok())` on a missing
    /// header short-circuits to `None`/`is_form = false`, landing on the
    /// JSON branch, where `Json::from_request` itself rejects the missing
    /// content-type).
    #[tokio::test]
    #[serial_test::serial]
    async fn oauth_token_dispatch_missing_content_type_is_400_not_panic() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let app = public_router().with_state(state);
        let req = Request::builder()
            .method("POST")
            .uri("/oauth/token")
            // Deliberately no content-type header.
            .body(Body::from(
                r#"{"grant_type":"authorization_code","code":"x"}"#,
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 400);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"], "invalid_request");
    }

    /// A malformed/unrelated `Content-Type` (neither form nor JSON) must also
    /// 400 `invalid_request` cleanly — the dispatcher's `starts_with` check
    /// fails safe (routes to the JSON branch), and `Json::from_request`
    /// rejects the content-type mismatch rather than attempting to parse an
    /// arbitrary body as JSON.
    #[tokio::test]
    #[serial_test::serial]
    async fn oauth_token_dispatch_malformed_content_type_is_400_not_panic() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let app = public_router().with_state(state);
        let req = Request::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("content-type", "text/plain")
            .body(Body::from("grant_type=authorization_code"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 400);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"], "invalid_request");
    }
}
