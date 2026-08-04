//! MCP auth middleware (Slice E).
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
//! Anonymous requests (no/invalid token) pass through with no actor scope —
//! read tools work; write tools are rejected in-handler.
//!
//! Task 4 (MCP OAuth, docs-kit kit enablers) adds one exception: an
//! anonymous (missing OR invalid bearer) `tools/call` naming a
//! [`crate::mcp::tools::WRITE_TOOLS`] entry gets a genuine HTTP 401 with a
//! `WWW-Authenticate: Bearer resource_metadata="..."` challenge (RFC 9728)
//! right here, before the request ever reaches rmcp — so an OAuth-aware MCP
//! client (Claude Code) discovers this server's authorization endpoint and
//! completes the `/oauth/authorize` + `/oauth/token` dance from Task 4
//! instead of only ever seeing an in-band "unauthorized" tool-call error.
//! Every other request shape (reads, `initialize`, `tools/list`, anything
//! the peek can't confidently classify) is untouched — the peek only ever
//! narrows behavior, never widens it, and a real actor is still handled
//! exactly as before.

use std::sync::Arc;

use akashic_context::AppState;
use akashic_kernel::actor::{AuthMethod, Authenticated};
use akashic_quota::{ActorIdentity, CURRENT_ACTOR};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Cap on how many body bytes `mcp_auth` will buffer to peek the JSON-RPC
/// `method`/tool name for an UNAUTHENTICATED request only (an authenticated
/// request's body is never touched — see below). Legitimate anonymous
/// tool-call arguments are tiny; this generous cap is never hit in practice.
/// If it ever is, the peek fails open (treated as "not a recognized
/// write-tool call") rather than failing the request closed.
const PEEK_LIMIT_BYTES: usize = 2 * 1024 * 1024; // 2 MiB

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

    // No valid actor. Peek the JSON-RPC body before passing through
    // anonymously: an unauthenticated `tools/call` naming a write tool gets
    // the RFC 9728 challenge here instead of reaching rmcp at all. Every
    // other shape — reads, `initialize`, oversized/unreadable bodies — is
    // passed through with its body reconstructed byte-for-byte, unchanged
    // from pre-Task-4 behavior.
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, PEEK_LIMIT_BYTES).await {
        Ok(b) => b,
        Err(_) => {
            // Body unreadable while peeking (oversized/stream error): cannot
            // reconstruct the original bytes, so fail open on the PEEK, not
            // the request — hand the downstream stack an empty body and let
            // it react exactly as it would to any other malformed request.
            let req = Request::from_parts(parts, Body::empty());
            return next.run(req).await;
        }
    };

    if is_anonymous_write_tool_call(&bytes) {
        return unauthorized_challenge(&state);
    }

    let req = Request::from_parts(parts, Body::from(bytes));
    next.run(req).await
}

/// RFC 9728 `WWW-Authenticate` challenge response for an anonymous write-tool
/// call: HTTP 401 pointing at this server's protected-resource metadata
/// document, so an OAuth-aware MCP client knows where to start.
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

/// True iff `body` is a JSON-RPC `tools/call` envelope naming a tool in
/// [`crate::mcp::tools::WRITE_TOOLS`]. Any parse ambiguity — not JSON, not an
/// object, wrong/missing `method`, missing `params.name` — returns `false`:
/// this function only ever narrows which requests get the 401 challenge, so
/// anything it can't confidently classify falls through to the long-standing
/// anonymous-pass-through behavior instead of being rejected by mistake.
fn is_anonymous_write_tool_call(body: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    if v.get("method").and_then(|m| m.as_str()) != Some("tools/call") {
        return false;
    }
    let Some(name) = v
        .get("params")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
    else {
        return false;
    };
    crate::mcp::tools::WRITE_TOOLS.contains(&name)
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

    // ── is_anonymous_write_tool_call (Task 4: 401 + WWW-Authenticate challenge) ─

    #[test]
    fn recognizes_write_tool_call() {
        for name in super::super::mcp::tools::WRITE_TOOLS {
            let body = format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{name}","arguments":{{}}}}}}"#
            );
            assert!(
                super::is_anonymous_write_tool_call(body.as_bytes()),
                "expected write tool '{name}' to be recognized"
            );
        }
    }

    #[test]
    fn does_not_flag_read_tool_call() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_knowledge","arguments":{}}}"#;
        assert!(!super::is_anonymous_write_tool_call(body));
    }

    #[test]
    fn does_not_flag_non_tools_call_methods() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
        assert!(!super::is_anonymous_write_tool_call(body));
        let init = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        assert!(!super::is_anonymous_write_tool_call(init));
    }

    #[test]
    fn does_not_flag_malformed_or_empty_body() {
        assert!(!super::is_anonymous_write_tool_call(b""));
        assert!(!super::is_anonymous_write_tool_call(b"not json"));
        assert!(!super::is_anonymous_write_tool_call(
            br#"{"jsonrpc":"2.0","method":"tools/call"}"#
        ));
    }

    #[test]
    fn passthrough_prefix_is_glpat() {
        assert!("glpat-abc".starts_with("glpat-"));
        assert!(!"ak_abc".starts_with("glpat-"));
    }

    /// The MCP-bypasses-quota fix: `mcp_auth`, given a valid device-flow bearer,
    /// must run the downstream inside `CURRENT_ACTOR.scope(...)` so the quota
    /// decorators (which read the task-local) attribute MCP tool usage. A probe
    /// handler reads the task-local: authenticated → the bearer's user_id;
    /// anonymous → no scope is established (try_with errors → sentinel).
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

        // Probe: report the task-local actor's user_id, or -1 when unset (the
        // task-local is only established inside `mcp_auth`'s authenticated arm).
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

        // Anonymous → no scope established (try_with errors → "-1").
        let anon = app
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let anon_body = axum::body::to_bytes(anon.into_body(), 1024).await.unwrap();
        assert_eq!(
            String::from_utf8_lossy(&anon_body),
            "-1",
            "anonymous MCP request must NOT establish a quota actor scope"
        );
    }
}
