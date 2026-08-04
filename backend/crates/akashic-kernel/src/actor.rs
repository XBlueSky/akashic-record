//! Actor-identity types for the MCP authentication pipeline.
//!
//! Hoisted from `mcp::proxy` to break the import cycle between `mcp` and
//! `{auth, llm, embedding}`. Previously `auth::quota`, `llm::quota_wrapped`,
//! and `embedding::quota_wrapped` all imported `Authenticated`/`AuthMethod`
//! from `mcp::proxy`, while `mcp::audit` imported back from `auth::quota`.
//! Moving the leaf types here gives all modules a common dependency that sits
//! below the cycle.

/// Identity contract that carries a caller's authentication context through
/// the MCP request pipeline.
///
/// ## Populators (B1, `auth/mcp_middleware.rs`)
///
/// Two `axum::middleware::from_fn` middlewares mount between `rate_limit_layer`
/// (A6) and `auth_gate` (A7). On successful auth each inserts:
///
/// ```ignore
/// // CRITICAL: the explicit `as Arc<dyn Authenticated>` cast is load-bearing.
/// // axum's `Extensions::get::<T>()` is keyed on `TypeId`. Inserting
/// // `Arc::new(MyAuth { ... })` WITHOUT the cast stores TypeId::of::<Arc<MyAuth>>(),
/// // NOT TypeId::of::<Arc<dyn Authenticated>>(), so `auth_gate`'s lookup
/// // returns None and the request is silently rejected as anonymous.
/// // See sub-spec §4 R-9 and `wrong_extension_type_silently_fails` in mcp/proxy.rs.
/// req.extensions_mut().insert(
///     Arc::new(my_auth) as Arc<dyn Authenticated>
/// );
/// ```
///
/// On auth failure the middleware passes the request through unchanged (no
/// extension), and `auth_gate` rejects any write-tool call as anonymous.
///
/// The two B1 implementations are:
///   - `DeviceFlowAuth` — `actor_token_id` is `"mcp_token:<uuid>"`
///   - `PassthroughAuth` — `actor_token_id` is `"gitlab_pat:<sha256[..16]>"`
///
/// ## Consumers
///
/// - **`auth_gate`** (A7, `mcp/proxy.rs`) — looks up `Arc<dyn Authenticated>` from
///   request extensions; rejects write-tool calls that lack it with JSON-RPC `-32001`.
/// - **B2** (audit log) — reads all three methods for log enrichment.
/// - **B3** (per-actor quota) — keys LLM/embedding counters on `actor_id()`.
/// - **B4** (token revocation) — targets `actor_token_id()` for revoke actions.
pub trait Authenticated: Send + Sync + std::fmt::Debug {
    /// Stable principal identifier. Format: "gitlab:user:<numeric-id>".
    /// Constant for a given GitLab user across token rotations.
    fn actor_id(&self) -> &str;

    /// Stable identifier for the credential itself. Format depends on auth_method.
    fn actor_token_id(&self) -> &str;

    /// Provenance of the credential. Used by B2 audit log for diagnostics.
    fn auth_method(&self) -> AuthMethod;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    DeviceFlow,
    /// `rename_all = "snake_case"` splits camel-case boundaries, which would
    /// produce `"git_lab_passthrough"`. The explicit rename locks the wire
    /// value to the intended `"gitlab_passthrough"`.
    #[serde(rename = "gitlab_passthrough")]
    GitLabPassthrough,
}

/// Parse the integer suffix from the `"gitlab:user:<N>"` actor-id format
/// produced by `Authenticated::actor_id()`. Returns `None` on any other shape.
/// Single source of truth for the inverse of that format (used by audit +
/// quota).
pub fn parse_actor_id(actor_id: &str) -> Option<i64> {
    actor_id.strip_prefix("gitlab:user:")?.parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::parse_actor_id;

    #[test]
    fn parse_actor_id_valid_and_invalid() {
        assert_eq!(parse_actor_id("gitlab:user:42"), Some(42));
        assert_eq!(
            parse_actor_id("gitlab:user:9223372036854775807"),
            Some(i64::MAX)
        );
        assert_eq!(parse_actor_id("github:user:42"), None);
        assert_eq!(parse_actor_id(""), None);
        assert_eq!(parse_actor_id("gitlab:user:"), None);
        assert_eq!(parse_actor_id("gitlab:user:abc"), None);
    }
}
