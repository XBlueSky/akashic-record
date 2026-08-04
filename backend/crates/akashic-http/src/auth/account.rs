//! B4 account self-management endpoints (token list, revoke, audit).
//! All routes mounted on the protected_auth_router (require_auth).

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use akashic_context::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/tokens", get(list_my_tokens))
        .route("/api/v1/auth/tokens/{id}/revoke", post(revoke_my_token))
        .route("/api/v1/auth/passthrough/revoke", post(revoke_passthrough))
        .route("/api/v1/auth/audit", get(list_my_audit))
}

/// Extract the real numeric GitLab user_id from the session cookie. Returns
/// None if there is no valid session OR the legacy-session GitLab fallback
/// cannot resolve a concrete id — never a shared sentinel. Collapsing legacy
/// sessions to id 0 (the previous behavior) let every pre-B4 user list and
/// revoke each other's tokens and read each other's audit rows, since these
/// endpoints filter solely on this id.
async fn session_user_id(state: &AppState, headers: &HeaderMap) -> Option<i64> {
    crate::auth::oauth_device::extract_session_user(state, headers)
        .await
        .map(|s| s.user_id)
}

#[derive(Serialize)]
struct McpTokenSummary {
    id: uuid::Uuid,
    label: Option<String>,
    issued_at: chrono::DateTime<chrono::Utc>,
    last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    expires_at: chrono::DateTime<chrono::Utc>,
    revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn list_my_tokens(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let Some(user_id) = session_user_id(&state, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "no session").into_response();
    };

    match state.auth_service.list_my_tokens(user_id).await {
        Ok(rows) => {
            let summaries: Vec<McpTokenSummary> = rows
                .into_iter()
                .map(|r| McpTokenSummary {
                    id: r.id,
                    label: r.label,
                    issued_at: r.issued_at,
                    last_used_at: r.last_used_at,
                    expires_at: r.expires_at,
                    revoked_at: r.revoked_at,
                })
                .collect();
            (StatusCode::OK, Json(summaries)).into_response()
        }
        Err(e) => {
            warn!(event = "list_my_tokens_db_err", error = %e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

async fn revoke_my_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id_str): Path<String>,
) -> impl IntoResponse {
    let Some(user_id) = session_user_id(&state, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "no session").into_response();
    };

    let token_id: uuid::Uuid = match id_str.parse() {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid token id").into_response(),
    };

    match state.auth_service.revoke_my_token(user_id, token_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(akashic_domain::DomainError::NotFound(_)) => {
            warn!(
                event = "mcp_token_revoke_unauthorized_or_missing",
                token_id = %token_id,
                requesting_user_id = user_id,
            );
            (StatusCode::NOT_FOUND, "not found").into_response()
        }
        Err(e) => {
            warn!(event = "revoke_mcp_token_db_err", error = %e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PassthroughRevokeBody {
    token: Option<String>,
    prefix: Option<String>,
}

/// Compute the 16-hex tombstone prefix for a raw `glpat-...` token, matching
/// `AuthStore::validate_passthrough_token` / `revoke_passthrough_token`.
fn passthrough_prefix(token: &str) -> Option<String> {
    let raw = token.strip_prefix("glpat-").filter(|r| !r.is_empty())?;
    let hex = format!("{:x}", Sha256::digest(raw.as_bytes()));
    Some(hex[..16].to_string())
}

async fn revoke_passthrough(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PassthroughRevokeBody>,
) -> impl IntoResponse {
    let Some(user_id) = session_user_id(&state, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "no session").into_response();
    };

    // FIX (Medium: revoke_passthrough missing ownership check): the previous
    // implementation tombstoned any 16-hex prefix on behalf of the caller with
    // NO ownership/actor check, letting any authenticated user permanently
    // revoke any GitLab PAT identified solely by its prefix. Mirror the
    // ownership gate that `revoke_my_token` enforces.
    //
    // Ownership of a passthrough PAT can only be proven by presenting the token
    // itself: we validate it against GitLab and confirm the resolved user_id
    // matches the caller. A bare `prefix` carries no ownership proof — the
    // `revoked_passthrough_tokens` table records no owner before revocation and
    // the prefix is a one-way hash — so the prefix-only path is exactly the
    // exploited hole and is rejected for self-service callers.
    match (body.token.as_deref(), body.prefix.as_deref()) {
        (Some(_), Some(_)) | (None, None) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "supply exactly one of `token` or `prefix`"})),
        )
            .into_response(),
        (None, Some(p)) => {
            // Validate format so we don't leak the reason via error shape, but
            // refuse regardless: a prefix alone proves nothing about ownership.
            let p = p.trim().to_lowercase();
            if p.len() != 16 || !p.chars().all(|c| c.is_ascii_hexdigit()) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "invalid_prefix_format"})),
                )
                    .into_response();
            }
            warn!(
                event = "passthrough_revoke_prefix_unauthorized",
                requesting_user_id = user_id,
            );
            (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": "prefix_revocation_forbidden",
                    "detail": "ownership cannot be proven from a prefix; present the full token"
                })),
            )
                .into_response()
        }
        (Some(token), None) => {
            // Reject malformed tokens before hitting GitLab.
            let Some(prefix) = passthrough_prefix(token) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "invalid_token_format"})),
                )
                    .into_response();
            };

            match state
                .auth_service
                .revoke_passthrough(user_id, token.to_string())
                .await
            {
                Ok(()) => (
                    StatusCode::OK,
                    Json(serde_json::json!({"revoked": true, "prefix": prefix})),
                )
                    .into_response(),
                Err(akashic_domain::DomainError::BadRequest(_)) => (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "invalid_token_format"})),
                )
                    .into_response(),
                Err(akashic_domain::DomainError::NotFound(_)) => {
                    warn!(
                        event = "passthrough_revoke_unauthorized",
                        prefix = %prefix,
                        requesting_user_id = user_id,
                    );
                    (StatusCode::NOT_FOUND, "not found").into_response()
                }
                Err(e) => {
                    warn!(event = "revoke_passthrough_db_err", error = %e);
                    (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct AuditQuery {
    limit: Option<u32>,
}

#[derive(Serialize)]
struct AuditEntry {
    ts: chrono::DateTime<chrono::Utc>,
    action: String,
    target_id: Option<String>,
    actor_token_id: String,
    ip: Option<String>,
    response_summary: Option<String>,
}

async fn list_my_audit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> impl IntoResponse {
    let Some(user_id) = session_user_id(&state, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "no session").into_response();
    };

    let limit = q.limit.unwrap_or(50).min(200) as i64;

    match state.auth_service.list_my_audit(user_id, limit).await {
        Ok(rows) => {
            let entries: Vec<AuditEntry> = rows
                .into_iter()
                .map(|r| AuditEntry {
                    ts: r.ts,
                    action: r.action,
                    target_id: r.target_id,
                    actor_token_id: r.actor_token_id,
                    ip: r.ip,
                    response_summary: r.response_summary,
                })
                .collect();
            (StatusCode::OK, Json(entries)).into_response()
        }
        Err(e) => {
            tracing::warn!(event = "list_my_audit_db_err", error = %e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn build_state_with_session(
        api_key: &str,
        gitlab_user_id: i64,
        username: &str,
    ) -> (AppState, Router) {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let user_info = crate::auth::types::UserInfo {
            username: username.into(),
            name: None,
            avatar_url: None,
        };
        state
            .auth_store
            .insert_session(
                api_key,
                &user_info,
                "fake-gitlab-token",
                3600,
                Some(gitlab_user_id),
            )
            .await
            .expect("insert_session");
        let app = router().with_state(state.clone());
        (state, app)
    }

    #[tokio::test]
    async fn list_tokens_returns_only_callers_tokens() {
        let user_a: i64 = 9_500_001;
        let user_b: i64 = 9_500_002;
        let (state, app) = build_state_with_session("test-akey-a", user_a, "test_b4_userA").await;
        let pg = akashic_test_support::test_pg_pool().await;

        sqlx::query("DELETE FROM mcp_tokens WHERE user_id IN ($1, $2)")
            .bind(user_a)
            .bind(user_b)
            .execute(&pg)
            .await
            .ok();

        let _ = state
            .auth_store
            .issue_mcp_token(user_a, "test_b4_userA", Some("a1"))
            .await
            .unwrap();
        let _ = state
            .auth_store
            .issue_mcp_token(user_a, "test_b4_userA", Some("a2"))
            .await
            .unwrap();
        let _ = state
            .auth_store
            .issue_mcp_token(user_b, "test_b4_userB", Some("b1"))
            .await
            .unwrap();

        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/auth/tokens")
            .header("cookie", "ak_session=test-akey-a")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = v.as_array().expect("array");
        assert_eq!(arr.len(), 2, "user A should see exactly their 2 tokens");

        sqlx::query("DELETE FROM mcp_tokens WHERE user_id IN ($1, $2)")
            .bind(user_a)
            .bind(user_b)
            .execute(&pg)
            .await
            .ok();
        sqlx::query("DELETE FROM sessions WHERE api_key = 'test-akey-a'")
            .execute(&pg)
            .await
            .ok();
    }

    #[tokio::test]
    async fn revoke_my_token_404s_on_another_users_token() {
        let user_a: i64 = 9_500_011;
        let user_b: i64 = 9_500_012;
        let (state, app) =
            build_state_with_session("test-akey-a2", user_a, "test_b4_revokeA").await;
        let pg = akashic_test_support::test_pg_pool().await;

        sqlx::query("DELETE FROM mcp_tokens WHERE user_id IN ($1, $2)")
            .bind(user_a)
            .bind(user_b)
            .execute(&pg)
            .await
            .ok();

        let (b_token_id, _) = state
            .auth_store
            .issue_mcp_token(user_b, "test_b4_revokeB", None)
            .await
            .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/auth/tokens/{b_token_id}/revoke"))
            .header("cookie", "ak_session=test-akey-a2")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);

        let revoked: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT revoked_at FROM mcp_tokens WHERE id = $1")
                .bind(b_token_id)
                .fetch_one(&pg)
                .await
                .unwrap();
        assert_eq!(revoked, None);

        sqlx::query("DELETE FROM mcp_tokens WHERE user_id IN ($1, $2)")
            .bind(user_a)
            .bind(user_b)
            .execute(&pg)
            .await
            .ok();
        sqlx::query("DELETE FROM sessions WHERE api_key = 'test-akey-a2'")
            .execute(&pg)
            .await
            .ok();
    }

    #[tokio::test]
    async fn revoke_my_token_breaks_subsequent_validate() {
        let user: i64 = 9_500_013;
        let (state, app) =
            build_state_with_session("test-akey-revoke-self", user, "test_b4_revokeSelf").await;
        let pg = akashic_test_support::test_pg_pool().await;

        sqlx::query("DELETE FROM mcp_tokens WHERE user_id = $1")
            .bind(user)
            .execute(&pg)
            .await
            .ok();

        let (token_id, plaintext) = state
            .auth_store
            .issue_mcp_token(user, "test_b4_revokeSelf", None)
            .await
            .unwrap();

        // Sanity: token currently valid.
        assert!(
            state
                .auth_store
                .validate_mcp_token(&plaintext)
                .await
                .is_some()
        );

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/auth/tokens/{token_id}/revoke"))
            .header("cookie", "ak_session=test-akey-revoke-self")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 204);

        assert!(
            state
                .auth_store
                .validate_mcp_token(&plaintext)
                .await
                .is_none()
        );

        sqlx::query("DELETE FROM mcp_tokens WHERE user_id = $1")
            .bind(user)
            .execute(&pg)
            .await
            .ok();
        sqlx::query("DELETE FROM sessions WHERE api_key = 'test-akey-revoke-self'")
            .execute(&pg)
            .await
            .ok();
    }

    /// Build an AppState whose `gitlab_url` points at the supplied mock URI, plus
    /// a session bound to `gitlab_user_id`. Mirrors `build_state_with_session`
    /// but lets the test wire a wiremock GitLab so passthrough validation (and
    /// thus the ownership check) can resolve a concrete user_id.
    async fn build_state_with_gitlab(
        api_key: &str,
        gitlab_user_id: i64,
        username: &str,
        gitlab_url: String,
    ) -> (AppState, Router) {
        let state = akashic_test_support::build_app_state(gitlab_url).await;
        let user_info = crate::auth::types::UserInfo {
            username: username.into(),
            name: None,
            avatar_url: None,
        };
        state
            .auth_store
            .insert_session(
                api_key,
                &user_info,
                "fake-gitlab-token",
                3600,
                Some(gitlab_user_id),
            )
            .await
            .expect("insert_session");
        let app = router().with_state(state.clone());
        (state, app)
    }

    #[tokio::test]
    async fn passthrough_revoke_from_full_token_owner_match() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let user: i64 = 9_500_021;

        // Synthetic test fixture — split literal so gitleaks generic-api-key
        // heuristic doesn't match an obviously-fake 8-char "PAT" as a real secret.
        let token = format!("glpat-{}", "abc12345");
        let raw = "abc12345";
        let expected_prefix =
            format!("{:x}", sha2::Sha256::digest(raw.as_bytes()))[..16].to_string();

        // GitLab resolves this token to the SAME numeric user as the session:
        // ownership proven, revocation must succeed.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/user"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": user, "username": "test_b4_ptT"})),
            )
            .mount(&server)
            .await;

        let (_state, app) =
            build_state_with_gitlab("test-akey-pt-token", user, "test_b4_ptT", server.uri()).await;
        let pg = akashic_test_support::test_pg_pool().await;

        sqlx::query("DELETE FROM revoked_passthrough_tokens WHERE token_id_hash = $1")
            .bind(&expected_prefix)
            .execute(&pg)
            .await
            .ok();

        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/passthrough/revoke")
            .header("cookie", "ak_session=test-akey-pt-token")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::BIGINT FROM revoked_passthrough_tokens WHERE token_id_hash = $1 AND revoked_by = $2"
        ).bind(&expected_prefix).bind(user).fetch_one(&pg).await.unwrap();
        assert_eq!(n, 1);

        sqlx::query("DELETE FROM revoked_passthrough_tokens WHERE token_id_hash = $1")
            .bind(&expected_prefix)
            .execute(&pg)
            .await
            .ok();
        sqlx::query("DELETE FROM sessions WHERE api_key = 'test-akey-pt-token'")
            .execute(&pg)
            .await
            .ok();
    }

    #[tokio::test]
    async fn passthrough_revoke_from_full_token_owner_mismatch_404s() {
        // FIX core regression guard (Medium: revoke_passthrough missing
        // ownership check): a token that GitLab resolves to a DIFFERENT user
        // than the caller must 404 and must NOT tombstone — otherwise any
        // authenticated user could revoke another user's PAT.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let caller: i64 = 9_500_023;
        let real_owner: i64 = 9_500_024;

        let token = format!("glpat-{}", "deadbeef");
        let raw = "deadbeef";
        let expected_prefix =
            format!("{:x}", sha2::Sha256::digest(raw.as_bytes()))[..16].to_string();

        // GitLab says the token belongs to `real_owner`, NOT the caller.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id": real_owner, "username": "test_b4_ptOwner"}),
            ))
            .mount(&server)
            .await;

        let (_state, app) = build_state_with_gitlab(
            "test-akey-pt-mismatch",
            caller,
            "test_b4_ptCaller",
            server.uri(),
        )
        .await;
        let pg = akashic_test_support::test_pg_pool().await;

        sqlx::query("DELETE FROM revoked_passthrough_tokens WHERE token_id_hash = $1")
            .bind(&expected_prefix)
            .execute(&pg)
            .await
            .ok();

        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/passthrough/revoke")
            .header("cookie", "ak_session=test-akey-pt-mismatch")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404, "revoking another user's PAT must 404");

        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::BIGINT FROM revoked_passthrough_tokens WHERE token_id_hash = $1",
        )
        .bind(&expected_prefix)
        .fetch_one(&pg)
        .await
        .unwrap();
        assert_eq!(n, 0, "owner-mismatch must not tombstone any token");

        sqlx::query("DELETE FROM revoked_passthrough_tokens WHERE token_id_hash = $1")
            .bind(&expected_prefix)
            .execute(&pg)
            .await
            .ok();
        sqlx::query("DELETE FROM sessions WHERE api_key = 'test-akey-pt-mismatch'")
            .execute(&pg)
            .await
            .ok();
    }

    #[test]
    fn passthrough_prefix_is_deterministic_and_rejects_malformed() {
        // Pure unit test (no DB / no HTTP): the prefix derivation must match
        // validate_passthrough_token / revoke_passthrough_token exactly.
        let raw = "abc12345";
        let expected = format!("{:x}", sha2::Sha256::digest(raw.as_bytes()))[..16].to_string();
        assert_eq!(
            super::passthrough_prefix(&format!("glpat-{raw}")).as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            super::passthrough_prefix(&format!("glpat-{raw}")).map(|p| p.len()),
            Some(16)
        );

        // Malformed inputs must yield None (handler maps these to 400).
        assert_eq!(super::passthrough_prefix("glpat-"), None); // empty secret
        assert_eq!(super::passthrough_prefix("abc12345"), None); // missing prefix
        assert_eq!(super::passthrough_prefix(""), None);
        assert_eq!(super::passthrough_prefix("ak_notapat"), None);
    }

    #[tokio::test]
    async fn passthrough_revoke_from_prefix_is_forbidden() {
        // FIX regression guard (Medium: revoke_passthrough missing ownership
        // check): a bare prefix carries no ownership proof, so the prefix-only
        // path must be refused and must NOT write a tombstone — otherwise any
        // authenticated user could revoke any PAT by its 16-hex prefix.
        let user: i64 = 9_500_022;
        let (_state, app) =
            build_state_with_session("test-akey-pt-prefix", user, "test_b4_ptP").await;
        let pg = akashic_test_support::test_pg_pool().await;

        let prefix = "0123456789abcdef";
        sqlx::query("DELETE FROM revoked_passthrough_tokens WHERE token_id_hash = $1")
            .bind(prefix)
            .execute(&pg)
            .await
            .ok();

        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/passthrough/revoke")
            .header("cookie", "ak_session=test-akey-pt-prefix")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"prefix":"{prefix}"}}"#)))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 403, "prefix-only revoke must be forbidden");

        // Critically: no tombstone may have been written.
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::BIGINT FROM revoked_passthrough_tokens WHERE token_id_hash = $1",
        )
        .bind(prefix)
        .fetch_one(&pg)
        .await
        .unwrap();
        assert_eq!(n, 0, "forbidden request must not tombstone any token");

        sqlx::query("DELETE FROM revoked_passthrough_tokens WHERE token_id_hash = $1")
            .bind(prefix)
            .execute(&pg)
            .await
            .ok();
        sqlx::query("DELETE FROM sessions WHERE api_key = 'test-akey-pt-prefix'")
            .execute(&pg)
            .await
            .ok();
    }

    #[tokio::test]
    async fn list_audit_returns_only_callers_rows_newest_first() {
        let user_a: i64 = 9_500_031;
        let user_b: i64 = 9_500_032;
        let (_state, app) =
            build_state_with_session("test-akey-audit-a", user_a, "test_b4_audA").await;
        let pg = akashic_test_support::test_pg_pool().await;

        sqlx::query("DELETE FROM audit_log WHERE actor_user_id IN ($1, $2)")
            .bind(user_a)
            .bind(user_b)
            .execute(&pg)
            .await
            .ok();

        // 3 rows for A, 2 for B
        for action in ["save_note", "supersede_note", "save_note"] {
            sqlx::query(
                "INSERT INTO audit_log (actor_user_id, actor_token_id, auth_method, action, after_hash) \
                 VALUES ($1, 'mcp_token:test', 'device_flow', $2, '\\x00')"
            ).bind(user_a).bind(action).execute(&pg).await.unwrap();
        }
        for _ in 0..2 {
            sqlx::query(
                "INSERT INTO audit_log (actor_user_id, actor_token_id, auth_method, action, after_hash) \
                 VALUES ($1, 'mcp_token:test', 'device_flow', 'save_note', '\\x00')"
            ).bind(user_b).execute(&pg).await.unwrap();
        }

        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/auth/audit?limit=50")
            .header("cookie", "ak_session=test-akey-audit-a")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
        let v: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 65536).await.unwrap())
                .unwrap();
        let arr = v.as_array().expect("array");
        assert_eq!(arr.len(), 3, "user A should see exactly their 3 rows");

        // AC-7 spec: "newest first". Assert ts is monotonically descending.
        let t0 = arr[0]["ts"].as_str().unwrap();
        let t1 = arr[1]["ts"].as_str().unwrap();
        let t2 = arr[2]["ts"].as_str().unwrap();
        assert!(t0 >= t1, "row 0 ts ({t0}) should be >= row 1 ts ({t1})");
        assert!(t1 >= t2, "row 1 ts ({t1}) should be >= row 2 ts ({t2})");

        sqlx::query("DELETE FROM audit_log WHERE actor_user_id IN ($1, $2)")
            .bind(user_a)
            .bind(user_b)
            .execute(&pg)
            .await
            .ok();
        sqlx::query("DELETE FROM sessions WHERE api_key = 'test-akey-audit-a'")
            .execute(&pg)
            .await
            .ok();
    }
}
