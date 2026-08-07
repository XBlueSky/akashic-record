//! MCP auth middleware (Slice E; fully fail-closed as of Task 3).
//!
//! A single axum/tower layer in front of the streamable-http
//! [`crate::mcp::http`] router. It validates the `Authorization: Bearer` token,
//! and on success:
//!   1. inserts `Arc<dyn Authenticated>` into the request extensions (read by
//!      the in-handler write-tool gate, [`crate::mcp::tools`]), and
//!   2. enters `akashic_quota::CURRENT_ACTOR.scope(...)` around the downstream
//!      service call so the tool handler's embedding/LLM usage is quota-counted.
//!
//! That second step is the fix for the long-standing **MCP-bypasses-quota gap**:
//! rmcp 0.1's SseServer ran tool handlers in a task where nothing set the
//! `CURRENT_ACTOR` task-local, so MCP embedding/LLM calls escaped B3 quota. The
//! streamable-http architecture lets a normal tower layer establish the scope,
//! exactly as the REST `require_auth` middleware already does.
//!
//! ## Full-auth, fail-closed (Task 3)
//!
//! EVERY `/mcp` request without a valid bearer gets an immediate HTTP 401
//! with a `WWW-Authenticate: Bearer resource_metadata="..."` challenge (RFC
//! 9728), via [`unauthorized_challenge`], before the request ever reaches
//! rmcp's streamable-http handler. There is no anonymous path anymore — not
//! for reads, not for `initialize`/`tools/list`, not for any request shape.
//! This applies uniformly regardless of *why* `resolved` came back `None`:
//! no bearer, a malformed bearer, an `ak_*` token that fails
//! `validate_mcp_token` (unknown/expired/revoked), or a `glpat-*` PAT that
//! fails `validate_passthrough_bearer` — including when that validation
//! itself errors (e.g. a GitLab upstream 5xx). All of these are
//! indistinguishable to the caller and all fail closed to 401; none of them
//! silently degrade to an anonymous request.
//!
//! Earlier revisions (Slice E, then Task 4) let anonymous requests through
//! so read tools worked without a token, with only a narrow in-handler gate
//! on write tools; Task 4 added a body-peek carve-out so an anonymous
//! `tools/call` naming a [`crate::mcp::tools::WRITE_TOOLS`] entry got the
//! RFC 9728 challenge instead of an in-band tool error. Task 3 replaces all
//! of that: the whole server now requires authentication, so the peek (and
//! the anonymous-pass-through it was narrowing) is gone. The in-handler
//! write-tool gate in [`crate::mcp::tools`] still exists as defense in
//! depth, but with no anonymous path left it can no longer be reached by an
//! unauthenticated caller — this layer already turned that request away.

use std::sync::Arc;

use akashic_context::AppState;
use akashic_kernel::actor::{AuthMethod, Authenticated};
use akashic_quota::{ActorIdentity, CURRENT_ACTOR};
use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

#[derive(Debug)]
pub struct DeviceFlowAuth {
    pub actor_id: String,
    pub actor_token_id: String,
}

impl Authenticated for DeviceFlowAuth {
    fn actor_id(&self) -> &str {
        &self.actor_id
    }
    fn actor_token_id(&self) -> &str {
        &self.actor_token_id
    }
    fn auth_method(&self) -> AuthMethod {
        AuthMethod::DeviceFlow
    }
}

#[derive(Debug)]
pub struct PassthroughAuth {
    pub actor_id: String,
    pub actor_token_id: String,
}

impl Authenticated for PassthroughAuth {
    fn actor_id(&self) -> &str {
        &self.actor_id
    }
    fn actor_token_id(&self) -> &str {
        &self.actor_token_id
    }
    fn auth_method(&self) -> AuthMethod {
        AuthMethod::GitLabPassthrough
    }
}

/// Validate the bearer token and, on success, install the `Authenticated`
/// extension + the `CURRENT_ACTOR` quota scope around the downstream MCP call.
pub async fn mcp_auth(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let bearer = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);

    // Resolve the bearer into (extension, quota actor). `ak_*` → device-flow MCP
    // token; `glpat-*` → GitLab passthrough PAT. Anything else / invalid → None.
    let resolved: Option<(Arc<dyn Authenticated>, ActorIdentity)> = match bearer.as_deref() {
        Some(token) if token.starts_with("ak_") => {
            state.auth_store.validate_mcp_token(token).await.map(|v| {
                let token_id = format!("mcp_token:{}", v.token_id);
                let auth = Arc::new(DeviceFlowAuth {
                    actor_id: format!("gitlab:user:{}", v.user_id),
                    actor_token_id: token_id.clone(),
                }) as Arc<dyn Authenticated>;
                let actor = ActorIdentity {
                    user_id: v.user_id,
                    token_id,
                    auth_method: AuthMethod::DeviceFlow,
                };
                (auth, actor)
            })
        }
        Some(token) if token.starts_with("glpat-") => {
            match state
                .auth_service
                .validate_passthrough_bearer(token.to_string())
                .await
            {
                Ok(Some(v)) => {
                    let token_id = format!("gitlab_pat:{}", v.token_id_prefix);
                    let auth = Arc::new(PassthroughAuth {
                        actor_id: format!("gitlab:user:{}", v.user_id),
                        actor_token_id: token_id.clone(),
                    }) as Arc<dyn Authenticated>;
                    let actor = ActorIdentity {
                        user_id: v.user_id,
                        token_id,
                        auth_method: AuthMethod::GitLabPassthrough,
                    };
                    Some((auth, actor))
                }
                // Validation failure or 5xx-fail-closed → treat as anonymous.
                _ => None,
            }
        }
        _ => None,
    };

    if let Some((auth, actor)) = resolved {
        req.extensions_mut().insert(auth);
        return CURRENT_ACTOR.scope(Some(actor), next.run(req)).await;
    }

    // No valid actor: every anonymous /mcp request — any method, any body
    // shape, reads and `tools/list`/`initialize` included — gets the RFC
    // 9728 challenge here, before the request ever reaches rmcp. This also
    // covers a `glpat-*` bearer that failed passthrough validation
    // (including an upstream 5xx): `resolved` is `None` either way, so
    // there is no separate fail-open branch to fall into — see the module
    // doc comment.
    unauthorized_challenge(&state)
}

/// RFC 9728 `WWW-Authenticate` challenge response for an unauthenticated
/// `/mcp` request: HTTP 401 pointing at this server's protected-resource
/// metadata document, so an OAuth-aware MCP client knows where to start.
fn unauthorized_challenge(state: &AppState) -> Response {
    let base = state.config.public_base_url.trim_end_matches('/');
    let challenge =
        format!(r#"Bearer resource_metadata="{base}/.well-known/oauth-protected-resource""#);
    let mut resp = (
        StatusCode::UNAUTHORIZED,
        axum::Json(serde_json::json!({
            "error": "unauthorized",
            "error_description": "this tool requires authentication — see WWW-Authenticate",
        })),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&challenge) {
        resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    resp
}

#[cfg(test)]
mod tests {
    //! Token-prefix routing is the only DB-free unit surface here; full auth +
    //! quota-scope + write-gate behavior is exercised end-to-end by the
    //! `mcp_contract` bench (Slice E E7).
    #[test]
    fn device_flow_prefix_is_ak() {
        assert!("ak_deadbeef".starts_with("ak_"));
        assert!(!"glpat-x".starts_with("ak_"));
    }

    #[test]
    fn passthrough_prefix_is_glpat() {
        assert!("glpat-abc".starts_with("glpat-"));
        assert!(!"ak_abc".starts_with("glpat-"));
    }

    /// Covers two things in one live-DB bench (per Task 3's brief: the only
    /// `AppState` construction path available to this crate's unit tests
    /// requires a live Postgres — `akashic_test_support::build_app_state` —
    /// so the standalone 401-challenge unit test the brief sketches is
    /// folded in here rather than duplicated against a nonexistent
    /// DB-free `AppState` builder):
    ///
    ///   1. The MCP-bypasses-quota fix: `mcp_auth`, given a valid
    ///      device-flow bearer, must run the downstream inside
    ///      `CURRENT_ACTOR.scope(...)` so the quota decorators (which read
    ///      the task-local) attribute MCP tool usage. A probe handler
    ///      reads the task-local and reports the bearer's user_id.
    ///   2. Task 3's fail-closed policy: an anonymous request to the same
    ///      layer never reaches the probe at all — `mcp_auth` returns 401
    ///      with the RFC 9728 `WWW-Authenticate` challenge directly.
    #[tokio::test]
    #[serial_test::serial]
    #[ignore = "requires live Postgres; run with --ignored + DATABASE_URL"]
    async fn mcp_auth_establishes_current_actor_scope() {
        use axum::{
            Router, body::Body, http::Request, middleware::from_fn_with_state, routing::get,
        };
        use tower::ServiceExt;

        let user_id: i64 = 9_700_077;
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let (_id, plaintext) = state
            .auth_store
            .issue_mcp_token(user_id, "test_quota_scope", None)
            .await
            .expect("issue mcp token");

        // Probe: report the task-local actor's user_id. Only reachable when
        // `mcp_auth` let the request through, i.e. the authenticated arm.
        async fn probe() -> String {
            akashic_quota::CURRENT_ACTOR
                .try_with(|a| a.as_ref().map(|x| x.user_id).unwrap_or(0))
                .unwrap_or(-1)
                .to_string()
        }
        let app = Router::new()
            .route("/probe", get(probe))
            .layer(from_fn_with_state(state.clone(), super::mcp_auth));

        // Authenticated → CURRENT_ACTOR scoped with the bearer's user_id.
        let authed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .header("authorization", format!("Bearer {plaintext}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(authed.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&body),
            user_id.to_string(),
            "authenticated MCP request must run inside CURRENT_ACTOR.scope (quota fix)"
        );

        // Anonymous → 401 + RFC 9728 challenge, never reaching the probe
        // (Task 3: full-auth, fail-closed — see the module doc comment).
        let anon = app
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            anon.status(),
            axum::http::StatusCode::UNAUTHORIZED,
            "anonymous MCP request must be rejected with 401, not reach the downstream probe"
        );
        assert!(
            anon.headers().contains_key("www-authenticate"),
            "401 response must carry the RFC 9728 WWW-Authenticate challenge"
        );
    }
}
