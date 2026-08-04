use super::*;

// ── AuthService (15 methods) ──────────────────────────────────────────────────

/// Use-case port for authentication and identity operations.
///
/// Covers: OAuth browser flow (login / callback), code exchange, device-flow
/// (device_authorization / device_token / device_verify / device_approve),
/// account token management (list / revoke MCP tokens, revoke passthrough,
/// audit log), OAuth-config health probe, REST middleware auth enforcement,
/// and MCP/passthrough bearer validation.
///
/// **A1 note:** the browser/device OAuth flows and all account-management
/// endpoints are handler-heavy and coupled to `AppState` (pending codes /
/// states, JWT generation, HTTP response formatting) — they are stubbed with
/// `anyhow::bail!` labeled `(A2)`.  The following methods have **full impls**
/// in `akashic-identity::services::IdentityServices` because their
/// orchestration is app-resident in that crate:
///
/// - `validate_session` — `AuthStore::validate_session`
/// - `validate_mcp_bearer` — `AuthStore::validate_mcp_token`
/// - `validate_passthrough_bearer` — `AuthStore::validate_passthrough_token`
/// - `issue_mcp_token` — `AuthStore::issue_mcp_token`
/// - `revoke_mcp_token_by_id` — `AuthStore::revoke_mcp_token`
/// - `revoke_passthrough_token` — `AuthStore::revoke_passthrough_token`
/// - `cleanup_expired_sessions` — `AuthStore::cleanup_expired`
/// - `health_oauth` — `oauth_runtime::validate` (pure config + network probes)
#[async_trait]
pub trait AuthService: Send + Sync {
    // ── OAuth browser / device flows — A2 stubs ────────────────────────────

    /// Initiate the GitLab OAuth browser flow.
    ///
    /// Generates a CSRF state token, stores it in `AuthStore::pending_states`,
    /// and redirects the browser to the GitLab authorization endpoint.
    ///
    /// HTTP: `GET /auth/login`
    async fn login_initiate(&self) -> crate::DomainResult<String>;

    /// Handle the OAuth callback from GitLab after user authorization.
    ///
    /// Validates the CSRF state, exchanges the code for a token via GitLab,
    /// creates a session, and issues an `ak_session` cookie.
    ///
    /// HTTP: `GET /auth/callback?code=…&state=…`
    async fn callback_complete(&self, code: String, state: String) -> crate::DomainResult<String>;

    /// Complete the web-OAuth session login: consume CSRF state, exchange the
    /// code via the GitLab gateway (web redirect uri), fetch the user, persist a
    /// session. Returns the api-key + ttl; the handler owns cookie + redirect.
    ///
    /// HTTP: `GET /auth/web/callback?code=…&state=…`
    async fn complete_web_login(
        &self,
        code: String,
        state: String,
    ) -> Result<crate::ports::gitlab::WebLoginOutcome, crate::ports::gitlab::WebLoginError>;

    /// Resolve the real numeric GitLab user id for a session, preferring the
    /// cached value and falling back to `GET /api/v4/user` (via the gateway)
    /// for legacy sessions whose id is NULL. `Ok(None)` on roundtrip failure.
    async fn resolve_gitlab_user_id(
        &self,
        gitlab_user_id: Option<i64>,
        gitlab_token: String,
    ) -> crate::DomainResult<Option<i64>>;

    /// Exchange a one-time auth code (from the device flow) for a session token.
    ///
    /// The code was issued by `device_verify` / `device_approve`.
    ///
    /// HTTP: `POST /auth/exchange`
    async fn exchange_code(&self, code: String) -> crate::DomainResult<String>;

    /// Start a device-authorization grant: issue `device_code` + `user_code`.
    ///
    /// HTTP: `POST /oauth/device_authorization`
    async fn device_authorization(
        &self,
        client_id: String,
    ) -> crate::DomainResult<serde_json::Value>;

    /// Poll for a device-flow token after the user approves via the browser.
    ///
    /// Returns a bearer token on success or an error code on pending / denied.
    ///
    /// HTTP: `POST /oauth/token` (device_code grant)
    async fn device_token(&self, device_code: String) -> crate::DomainResult<serde_json::Value>;

    /// Mark a pending device-flow session as `pre_approved` for the resolved
    /// session user. Returns `true` when the `user_code` matched a pending,
    /// non-expired row (the handler then renders the confirm page); `false`
    /// when no such row exists.
    ///
    /// The session user is resolved by the handler (it needs `AppState`:
    /// auth_store + GitLab fallback), so it is passed in here.
    ///
    /// HTTP: `POST /device/verify`
    async fn device_verify(
        &self,
        user_code: String,
        user_id: i64,
        user_login: String,
    ) -> crate::DomainResult<bool>;

    /// Transition a `pre_approved` device-flow session to `approved` for the
    /// resolved session user, so the polling `device_token` call can succeed.
    /// Returns `true` when exactly one matching row transitioned.
    ///
    /// HTTP: `POST /device/approve`
    async fn device_approve(&self, user_code: String, user_id: i64) -> crate::DomainResult<bool>;

    // ── Account token management — A2 stubs ───────────────────────────────

    /// List MCP tokens issued to the calling user.
    ///
    /// HTTP: `GET /api/v1/auth/tokens`
    async fn list_my_tokens(
        &self,
        user_id: i64,
    ) -> crate::DomainResult<Vec<crate::types::McpTokenSummaryRow>>;

    /// Revoke one of the calling user's MCP tokens by ID.
    ///
    /// HTTP: `POST /api/v1/auth/tokens/:id/revoke`
    async fn revoke_my_token(&self, user_id: i64, token_id: uuid::Uuid) -> crate::DomainResult<()>;

    /// Tombstone a `glpat-*` passthrough token (revoke by prefix hash).
    ///
    /// HTTP: `POST /api/v1/auth/passthrough/revoke`
    async fn revoke_passthrough(&self, user_id: i64, token: String) -> crate::DomainResult<()>;

    /// Return the calling user's recent audit-log entries.
    ///
    /// HTTP: `GET /api/v1/auth/audit`
    async fn list_my_audit(
        &self,
        user_id: i64,
        limit: i64,
    ) -> crate::DomainResult<Vec<crate::types::AuditEntryRow>>;

    // ── Middleware helpers — A2 stub ───────────────────────────────────────

    /// Validate a Bearer token and return the actor identifier string.
    ///
    /// The returned string is the actor ID used for quota tracking
    /// (e.g. `"gitlab:user:12345"`).  Returns `Err` for missing / invalid /
    /// expired tokens.
    ///
    /// REST middleware: applied on every protected route.
    async fn require_auth(&self, token: String) -> crate::DomainResult<String>;

    // ── App-resident token validators — full impls ─────────────────────────

    /// Validate an `ak_session` cookie value and return the user info tuple.
    ///
    /// Returns `Ok(Some((username, gitlab_token, gitlab_user_id)))` on hit,
    /// `Ok(None)` for unknown / expired sessions.
    ///
    /// Used by every session-cookie consumer in `akashic-server`.
    async fn validate_session(
        &self,
        api_key: String,
    ) -> crate::DomainResult<Option<AuthSessionInfo>>;

    /// Validate a presented `ak_*` MCP bearer and return token metadata.
    ///
    /// Returns `Ok(Some(…))` on hit; `Ok(None)` for unknown / revoked /
    /// malformed tokens.
    ///
    /// MCP toolkit middleware: `auth::mcp_middleware::device_flow_auth`.
    async fn validate_mcp_bearer(
        &self,
        bearer: String,
    ) -> crate::DomainResult<Option<crate::types::ValidatedMcpToken>>;

    /// Validate a `glpat-*` GitLab PAT via tombstone check + LRU cache +
    /// GitLab `/api/v4/user` probe.
    ///
    /// Returns `Ok(Some(…))` on success; `Ok(None)` for tombstoned /
    /// network-error / wrong-prefix tokens.
    ///
    /// MCP toolkit middleware: `auth::mcp_middleware::passthrough_auth`.
    async fn validate_passthrough_bearer(
        &self,
        bearer: String,
    ) -> crate::DomainResult<Option<crate::types::ValidatedPassthrough>>;

    /// Issue a new MCP bearer token for a GitLab user.
    ///
    /// Returns `(token_id, plaintext)`.  The plaintext is shown once and never
    /// re-retrievable.  Delegated to `AuthStore::issue_mcp_token`.
    async fn issue_mcp_token(
        &self,
        user_id: i64,
        user_login: String,
        label: Option<String>,
    ) -> crate::DomainResult<(uuid::Uuid, String)>;

    /// Revoke an MCP token by its database UUID.  Idempotent.
    async fn revoke_mcp_token_by_id(&self, token_id: uuid::Uuid) -> crate::DomainResult<()>;

    /// Tombstone a passthrough PAT by its 16-hex prefix hash.  Idempotent.
    async fn revoke_passthrough_token(
        &self,
        prefix: String,
        revoked_by: i64,
    ) -> crate::DomainResult<()>;

    /// Remove expired pending-codes / pending-states and sweep expired sessions
    /// from the DB.
    ///
    /// Called by `AuthStore::spawn_cleanup_task` on a periodic interval.
    async fn cleanup_expired_sessions(&self) -> crate::DomainResult<()>;

    /// Run the OAuth-config health suite and return the typed report.
    ///
    /// Runs 6 checks (3 static URI consistency + 3 network probes against the
    /// configured GitLab instance) via the GitLab gateway. The HTTP handler
    /// applies its own 60 s cache and serialises to JSON; this method always
    /// re-runs the checks.
    ///
    /// HTTP: `GET /api/v1/health/oauth`
    async fn health_oauth(&self) -> crate::DomainResult<crate::types::ValidationReport>;
}

// ── AuthService domain response types ─────────────────────────────────────────

/// User info returned by `AuthService::validate_session`.
///
/// Contains enough identity context for auth enforcement without exposing the
/// raw DB row or GitLab API types to callers.
#[derive(Debug, Clone)]
pub struct AuthSessionInfo {
    /// GitLab username (login handle).
    pub username: String,
    /// Display name (may be absent for service accounts).
    pub name: Option<String>,
    /// Avatar URL (may be absent).
    pub avatar_url: Option<String>,
    /// GitLab OAuth token for proxied API calls.
    pub gitlab_token: String,
    /// Numeric GitLab user id (used for quota and audit).
    pub gitlab_user_id: Option<i64>,
}
