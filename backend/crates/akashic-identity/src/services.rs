//! Application-service implementation for `AuthService`.
//!
//! `AuthService` is defined in `akashic-domain::ports::services` and
//! implemented here in `akashic-identity` — the crate that already owns
//! `AuthStore`, `oauth_runtime`, and the underlying token/session repo ports.
//!
//! **A2a scope — FILLED impls:**
//!
//! - `login_initiate` → generate CSRF state, store in `AuthStore::pending_states`,
//!   return the GitLab authorize URL (handler issues the redirect).
//! - `callback_complete` → validate CSRF, exchange code via GitLab, insert session,
//!   return `(session_api_key, username)` serialised as JSON.
//! - `exchange_code` → look up and consume `pending_codes`, insert session, return
//!   `(api_key, username)` serialised as JSON.
//! - `device_authorization` → generate codes, insert via `DeviceFlowRepo`,
//!   return RFC 8628 JSON body.
//! - `device_token` → `DeviceFlowRepo::poll_device_token` (Tx-scoped, atomic),
//!   issue token or return error JSON.
//! - `device_verify` → `DeviceFlowRepo::set_pre_approved`, return HTML.
//! - `device_approve` → `DeviceFlowRepo::set_approved`, return HTML.
//! - `list_my_tokens` → `AccountAuditRepo::list_my_tokens`.
//! - `revoke_my_token` → ownership check via `AccountAuditRepo::check_token_ownership`
//!   + `AuthStore::revoke_mcp_token` (preserves token-ownership authz).
//! - `revoke_passthrough` → validate token via `AuthStore::validate_passthrough_token`
//!   (ownership proof) + `AuthStore::revoke_passthrough_token`.
//! - `list_my_audit` → `AccountAuditRepo::list_my_audit`.
//! - `require_auth` → validates session token, returns actor identity string.
//!
//! **AuthStore in-mem stays:** `pending_states` (CSRF) and `pending_codes`
//! (one-time) are kept in `Arc<AuthStore>` DashMaps — NOT DB-backed.
//! `DeviceFlowRepo` only abstracts the `device_flow_pending` table.
//!
//! **device_token Tx:** poll_device_token on `DeviceFlowRepo` opens its own
//! transaction, runs `SELECT … FOR UPDATE + UPDATE` atomically, commits, and
//! returns the pre-update state.  The service never splits the Tx.

use std::sync::Arc;
use std::time::{Duration, Instant};

use akashic_domain::{DomainError, DomainResult};
use async_trait::async_trait;
use tracing::{info, warn};
use uuid::Uuid;

use akashic_config::Config;
use akashic_domain::ports::gitlab::{GitLabGateway, WebLoginError, WebLoginOutcome};
use akashic_domain::ports::services::{AuthService, AuthSessionInfo};
use akashic_domain::ports::{AccountAuditRepo, DeviceFlowRepo};
use akashic_domain::types::{
    AuditEntryRow, McpTokenSummaryRow, ValidatedMcpToken, ValidatedPassthrough, ValidationReport,
};

use crate::store::AuthStore;
use crate::types::{PendingCode, PendingState, UserInfo};

// ── code generation helpers (re-used from oauth_device) ────────────────────────
use rand::{Rng, RngCore};

const USER_CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXYZ23456789";

fn generate_user_code() -> String {
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

fn generate_device_code() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const ALLOWED_CLIENT_IDS: &[&str] = &["claude-code"];

// ── IdentityServices ───────────────────────────────────────────────────────────

/// Application service implementing `AuthService`.
///
/// **In-mem invariants:**
/// - `auth_store.pending_states` (CSRF): stored in `Arc<AuthStore>` DashMap.
/// - `auth_store.pending_codes` (one-time CLI codes): same.
/// - `DeviceFlowRepo` handles the `device_flow_pending` PG table.
/// - `AccountAuditRepo` handles MCP token listing + audit reads.
pub struct IdentityServices {
    auth_store: Arc<AuthStore>,
    config: Arc<Config>,
    gateway: Arc<dyn GitLabGateway>,
    device_flow_repo: Arc<dyn DeviceFlowRepo>,
    account_audit_repo: Arc<dyn AccountAuditRepo>,
}

impl IdentityServices {
    pub fn new(
        auth_store: Arc<AuthStore>,
        config: Arc<Config>,
        gateway: Arc<dyn GitLabGateway>,
        device_flow_repo: Arc<dyn DeviceFlowRepo>,
        account_audit_repo: Arc<dyn AccountAuditRepo>,
    ) -> Self {
        Self {
            auth_store,
            config,
            gateway,
            device_flow_repo,
            account_audit_repo,
        }
    }
}

// ── AuthService impl ───────────────────────────────────────────────────────────

#[async_trait]
impl AuthService for IdentityServices {
    // ── OAuth browser / device flows — A2a filled ──────────────────────────

    /// Generate CSRF state, store in `pending_states`, return the GitLab
    /// authorize URL.  The handler issues the HTTP redirect.
    async fn login_initiate(&self) -> DomainResult<String> {
        let state_param = Uuid::new_v4().to_string();
        self.auth_store.pending_states.insert(
            state_param.clone(),
            PendingState {
                expires_at: Instant::now() + Duration::from_mins(5),
                next: None,
            },
        );

        let gitlab_url = &self.config.gitlab_url;
        let client_id = urlencoding::encode(&self.config.gitlab_app_id);
        let redirect_uri = urlencoding::encode(&self.config.gitlab_redirect_uri);
        let state_q = urlencoding::encode(&state_param);
        let scope = urlencoding::encode("read_user read_repository");

        let authorize_url = format!(
            "{gitlab_url}/oauth/authorize\
             ?client_id={client_id}\
             &redirect_uri={redirect_uri}\
             &response_type=code\
             &state={state_q}\
             &scope={scope}"
        );
        Ok(authorize_url)
    }

    /// Validate CSRF, exchange code via GitLab, insert session.
    ///
    /// Returns a JSON string `{"api_key":"…","username":"…"}`.
    /// The handler sets the cookie and issues the HTML response.
    async fn callback_complete(&self, code: String, state: String) -> DomainResult<String> {
        // Validate and consume the state parameter.
        let removed = self.auth_store.pending_states.remove(&state);
        let (_, pending_state) = match removed {
            Some(entry) => entry,
            None => return Err(DomainError::BadRequest("invalid_or_expired_state".into())),
        };
        if Instant::now() > pending_state.expires_at {
            return Err(DomainError::BadRequest("state_expired".into()));
        }

        // Exchange authorization code → access token (device/deprecated flow
        // uses the non-web redirect uri), then fetch the user — via the gateway.
        let token = self
            .gateway
            .exchange_oauth_code(&code, &self.config.gitlab_redirect_uri)
            .await?;
        let access_token = token.access_token;

        let gl_user = self.gateway.fetch_user(&access_token).await?;
        let gitlab_user_id = gl_user.id;
        let user_info = UserInfo {
            username: gl_user.username,
            name: gl_user.name,
            avatar_url: gl_user.avatar_url,
        };

        // Generate verification code (one-time CLI exchange).
        let verification_code = generate_user_code();
        let ttl_secs = self.config.auth_code_ttl_secs;
        self.auth_store.pending_codes.insert(
            verification_code.clone(),
            PendingCode {
                user_info: user_info.clone(),
                gitlab_token: access_token.clone(),
                gitlab_user_id,
                expires_at: Instant::now() + Duration::from_secs(ttl_secs),
            },
        );

        info!(username = %user_info.username, "OAuth callback successful, verification code issued");
        Ok(serde_json::json!({
            "username": user_info.username,
            "code": verification_code,
            "ttl_secs": ttl_secs,
        })
        .to_string())
    }

    /// Look up and consume a one-time pending code, insert a session.
    ///
    /// Returns a JSON string `{"api_key":"…","username":"…"}`.
    async fn exchange_code(&self, code: String) -> DomainResult<String> {
        let removed = self.auth_store.pending_codes.remove(&code);
        let (_, pending) = match removed {
            Some(entry) => entry,
            None => return Err(DomainError::Unauthorized),
        };
        if Instant::now() > pending.expires_at {
            return Err(DomainError::Unauthorized);
        }

        let api_key = format!("ak-{}", Uuid::new_v4());
        let username = pending.user_info.username.clone();

        self.auth_store
            .insert_session(
                &api_key,
                &pending.user_info,
                &pending.gitlab_token,
                self.config.api_key_ttl_secs,
                pending.gitlab_user_id,
            )
            .await
            .map_err(|e| anyhow::anyhow!("session_insert_failed: {e}"))?;

        info!(%username, "API key issued via exchange_code");
        Ok(serde_json::json!({"api_key": api_key, "username": username}).to_string())
    }

    /// Issue device_code + user_code, insert pending row, return RFC 8628 JSON.
    async fn device_authorization(&self, client_id: String) -> DomainResult<serde_json::Value> {
        if !ALLOWED_CLIENT_IDS.contains(&client_id.as_str()) {
            return Err(DomainError::BadRequest("invalid_client".into()));
        }

        // Retry up to 3 times on UNIQUE collision (spec R-8).
        for attempt in 0..3u8 {
            let device_code = generate_device_code();
            let user_code = generate_user_code();

            match self
                .device_flow_repo
                .insert_pending(&device_code, &user_code, &client_id)
                .await
            {
                Ok(()) => {
                    let base = self.config.public_base_url.trim_end_matches('/');
                    let resp = serde_json::json!({
                        "device_code": device_code,
                        "user_code": user_code.clone(),
                        "verification_uri": format!("{base}/device"),
                        "verification_uri_complete": format!("{base}/device?user_code={user_code}"),
                        "expires_in": 600,
                        "interval": 5,
                    });
                    info!(event = "device_flow_issued", user_code = %user_code, %client_id);
                    return Ok(resp);
                }
                Err(e) if attempt < 2 => {
                    warn!(event = "device_flow_collision", attempt, error = %e);
                    continue;
                }
                Err(e) => {
                    return Err(DomainError::Internal(anyhow::anyhow!(
                        "device_authorization_insert_failed: {e}"
                    )));
                }
            }
        }
        unreachable!("loop returns on every branch");
    }

    /// Poll for a device-flow token.
    ///
    /// Returns a JSON value that is either:
    /// - `{"access_token":"…","token_type":"Bearer","expires_in":7776000}` on success
    /// - `{"error":"…"}` on any RFC 8628 error condition
    ///
    /// The `SELECT … FOR UPDATE + UPDATE` runs inside a single Tx held by
    /// `DeviceFlowRepo::poll_device_token`.
    async fn device_token(&self, device_code: String) -> DomainResult<serde_json::Value> {
        // Validate grant type is embedded in device_code as a JSON object
        // passed from the handler. For the service boundary, device_code
        // may be prefixed "grant_type=…\ndevice_code=…" or just the raw code.
        // The handler already validates grant_type and passes only device_code.

        let poll = match self.device_flow_repo.poll_device_token(&device_code).await {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(serde_json::json!({"error": "invalid_grant"})),
            Err(e) => {
                warn!(event = "device_flow_poll_failed", error = %e);
                return Ok(serde_json::json!({"error": "server_error"}));
            }
        };

        let now = chrono::Utc::now();

        if poll.expires_at < now {
            let prefix = device_code.chars().take(8).collect::<String>();
            tracing::debug!(event = "device_flow_expired_token_poll", device_code_prefix = %prefix);
            return Ok(serde_json::json!({"error": "expired_token"}));
        }
        if poll.slow_down {
            return Ok(serde_json::json!({"error": "slow_down"}));
        }

        match poll.status.as_str() {
            "pending" | "pre_approved" => Ok(serde_json::json!({"error": "authorization_pending"})),
            "denied" => Ok(serde_json::json!({"error": "access_denied"})),
            "exchanged" => Ok(serde_json::json!({"error": "invalid_grant"})),
            "approved" => {
                let user_id = match poll.granted_user_id {
                    Some(u) => u,
                    None => {
                        warn!(event = "device_flow_approved_without_user");
                        return Ok(serde_json::json!({"error": "server_error"}));
                    }
                };
                let login = poll.granted_user_login.unwrap_or_default();
                let (token_id, plaintext) =
                    match self.auth_store.issue_mcp_token(user_id, &login, None).await {
                        Ok(t) => t,
                        Err(e) => {
                            warn!(event = "mcp_token_issue_failed", error = %e);
                            return Ok(serde_json::json!({"error": "server_error"}));
                        }
                    };
                // Exactly-once exchange guard: WHERE status = 'approved'.
                match self
                    .device_flow_repo
                    .mark_exchanged(&device_code, token_id)
                    .await
                {
                    Ok(true) => {
                        info!(event = "device_flow_exchanged", token_id = %token_id, user_id);
                        Ok(serde_json::json!({
                            "access_token": plaintext,
                            "token_type": "Bearer",
                            "expires_in": 7_776_000_i64,
                        }))
                    }
                    Ok(false) => {
                        // Concurrent poll already exchanged. Revoke the orphan token.
                        if let Err(e) = self.auth_store.revoke_mcp_token(token_id).await {
                            warn!(
                                event = "device_flow_orphan_revoke_failed",
                                token_id = %token_id,
                                error = %e,
                            );
                        }
                        warn!(event = "device_flow_concurrent_exchange", %device_code);
                        Ok(serde_json::json!({"error": "invalid_grant"}))
                    }
                    Err(e) => {
                        warn!(event = "device_flow_exchange_update_failed", error = %e);
                        Ok(serde_json::json!({"error": "server_error"}))
                    }
                }
            }
            unknown => {
                warn!(event = "device_flow_unknown_status", status = %unknown);
                Ok(serde_json::json!({"error": "server_error"}))
            }
        }
    }

    /// Mark the user_code as `pre_approved` for the resolved session user.
    ///
    /// The handler resolves the session (it needs `AppState`) and passes the
    /// user in; this method only owns the DB state transition.
    async fn device_verify(
        &self,
        user_code: String,
        user_id: i64,
        user_login: String,
    ) -> DomainResult<bool> {
        self.device_flow_repo
            .set_pre_approved(&user_code, user_id, &user_login)
            .await
            .map_err(DomainError::Internal)
    }

    /// Transition the user_code from `pre_approved` to `approved` for the
    /// resolved session user.
    async fn device_approve(&self, user_code: String, user_id: i64) -> DomainResult<bool> {
        self.device_flow_repo
            .set_approved(&user_code, user_id)
            .await
            .map_err(DomainError::Internal)
    }

    // ── Account token management ───────────────────────────────────────────

    /// List all MCP tokens for the user (including revoked), newest-first.
    async fn list_my_tokens(&self, user_id: i64) -> DomainResult<Vec<McpTokenSummaryRow>> {
        self.account_audit_repo
            .list_my_tokens(user_id)
            .await
            .map_err(DomainError::Internal)
    }

    /// Revoke one of the calling user's MCP tokens — preserves token-ownership authz.
    ///
    /// Returns `Ok(())` on success, `Err("not_found")` if the token doesn't
    /// exist or belongs to a different user.
    async fn revoke_my_token(&self, user_id: i64, token_id: Uuid) -> DomainResult<()> {
        // Ownership check: 404 on mismatch (or missing) to avoid leaking
        // whether the token id exists (mirrors the original handler logic).
        let owner = self
            .account_audit_repo
            .check_token_ownership(token_id)
            .await?;
        match owner {
            Some(uid) if uid == user_id => {
                self.auth_store
                    .revoke_mcp_token(token_id)
                    .await
                    .map_err(|e| anyhow::anyhow!("revoke_mcp_token: {e}"))?;
                info!(event = "mcp_token_revoked_by_user", token_id = %token_id, user_id);
                Ok(())
            }
            Some(_) | None => Err(DomainError::NotFound("token".into())),
        }
    }

    /// Tombstone a passthrough PAT — preserves ownership proof via GitLab validation.
    ///
    /// Accepts the full `glpat-…` token (not just the prefix).  Validates
    /// ownership via GitLab, then tombstones.  Returns `Err("not_found")` if
    /// the token is invalid/revoked/upstream-unreachable or resolves to a
    /// different user.
    async fn revoke_passthrough(&self, user_id: i64, token: String) -> DomainResult<()> {
        use sha2::{Digest, Sha256};

        let raw = token
            .strip_prefix("glpat-")
            .filter(|r| !r.is_empty())
            .ok_or_else(|| DomainError::BadRequest("invalid_token_format".into()))?;
        let hex = format!("{:x}", Sha256::digest(raw.as_bytes()));
        let prefix = hex[..16].to_string();

        // Ownership check: validate against GitLab, confirm user_id matches.
        let validated = self
            .auth_store
            .validate_passthrough_token(&token, &self.gateway)
            .await;
        match validated {
            Some(v) if v.user_id == user_id => {
                self.auth_store
                    .revoke_passthrough_token(&prefix, user_id)
                    .await
                    .map_err(|e| anyhow::anyhow!("revoke_passthrough_token: {e}"))?;
                info!(
                    event = "passthrough_revoked_by_user",
                    prefix = %prefix,
                    revoked_by = user_id,
                );
                Ok(())
            }
            _ => Err(DomainError::NotFound("token".into())),
        }
    }

    /// Return the calling user's recent audit-log entries.
    async fn list_my_audit(&self, user_id: i64, limit: i64) -> DomainResult<Vec<AuditEntryRow>> {
        self.account_audit_repo
            .list_my_audit(user_id, limit)
            .await
            .map_err(DomainError::Internal)
    }

    // ── Middleware helper ──────────────────────────────────────────────────

    /// Validate a Bearer token and return the actor identity string.
    ///
    /// Tries MCP bearer first, then session cookie.  Used by the `require_auth`
    /// Axum middleware inner body (the middleware itself stays as a Tower layer;
    /// this method handles only the validation logic).
    async fn require_auth(&self, token: String) -> DomainResult<String> {
        // Try session token first (ak-UUID format).
        if let Some(info) = self.auth_store.validate_session(&token).await {
            return Ok(format!("session:{}", info.0.username));
        }
        // Try MCP bearer (ak_<32hex> format).
        if let Some(v) = self.auth_store.validate_mcp_token(&token).await {
            return Ok(format!("gitlab:user:{}", v.user_id));
        }
        Err(DomainError::Unauthorized)
    }

    // ── App-resident token validators — full impls ─────────────────────────

    /// Delegates to `AuthStore::validate_session`.
    async fn validate_session(&self, api_key: String) -> DomainResult<Option<AuthSessionInfo>> {
        let opt = self.auth_store.validate_session(&api_key).await;
        Ok(opt.map(
            |(user_info, gitlab_token, gitlab_user_id)| AuthSessionInfo {
                username: user_info.username,
                name: user_info.name,
                avatar_url: user_info.avatar_url,
                gitlab_token,
                gitlab_user_id,
            },
        ))
    }

    /// Delegates to `AuthStore::validate_mcp_token`.
    async fn validate_mcp_bearer(&self, bearer: String) -> DomainResult<Option<ValidatedMcpToken>> {
        Ok(self.auth_store.validate_mcp_token(&bearer).await)
    }

    /// Delegates to `AuthStore::validate_passthrough_token`.
    async fn validate_passthrough_bearer(
        &self,
        bearer: String,
    ) -> DomainResult<Option<ValidatedPassthrough>> {
        Ok(self
            .auth_store
            .validate_passthrough_token(&bearer, &self.gateway)
            .await)
    }

    /// Delegates to `AuthStore::issue_mcp_token`.
    async fn issue_mcp_token(
        &self,
        user_id: i64,
        user_login: String,
        label: Option<String>,
    ) -> DomainResult<(Uuid, String)> {
        self.auth_store
            .issue_mcp_token(user_id, &user_login, label.as_deref())
            .await
            .map_err(|e| DomainError::Internal(anyhow::anyhow!("issue_mcp_token: {e}")))
    }

    /// Delegates to `AuthStore::revoke_mcp_token`.
    async fn revoke_mcp_token_by_id(&self, token_id: Uuid) -> DomainResult<()> {
        self.auth_store
            .revoke_mcp_token(token_id)
            .await
            .map_err(|e| DomainError::Internal(anyhow::anyhow!("revoke_mcp_token: {e}")))
    }

    /// Delegates to `AuthStore::revoke_passthrough_token`.
    async fn revoke_passthrough_token(&self, prefix: String, revoked_by: i64) -> DomainResult<()> {
        self.auth_store
            .revoke_passthrough_token(&prefix, revoked_by)
            .await
            .map_err(|e| DomainError::Internal(anyhow::anyhow!("revoke_passthrough_token: {e}")))
    }

    /// Delegates to `AuthStore::cleanup_expired`.
    async fn cleanup_expired_sessions(&self) -> DomainResult<()> {
        self.auth_store.cleanup_expired().await;
        Ok(())
    }

    /// Delegates to the GitLab gateway's runtime report.
    async fn health_oauth(&self) -> DomainResult<ValidationReport> {
        Ok(self.gateway.runtime_report().await)
    }

    /// Web-OAuth session login: consume CSRF state, exchange the code via the
    /// gateway (WEB redirect uri), fetch the user, persist a session. The
    /// handler owns the cookie + redirect.
    async fn complete_web_login(
        &self,
        code: String,
        state: String,
    ) -> std::result::Result<WebLoginOutcome, WebLoginError> {
        // CSRF: consume the pending state.
        let (_, pending) = self
            .auth_store
            .pending_states
            .remove(&state)
            .ok_or(WebLoginError::InvalidState)?;
        if Instant::now() > pending.expires_at {
            return Err(WebLoginError::InvalidState);
        }
        // Exchange + fetch user via the gateway (WEB redirect uri).
        let token = self
            .gateway
            .exchange_oauth_code(&code, &self.config.gitlab_web_redirect_uri)
            .await
            .map_err(|e| WebLoginError::Upstream(e.to_string()))?;
        let gl_user = self
            .gateway
            .fetch_user(&token.access_token)
            .await
            .map_err(|e| WebLoginError::Upstream(e.to_string()))?;
        let gitlab_user_id = gl_user.id;
        let user_info = UserInfo {
            username: gl_user.username,
            name: gl_user.name,
            avatar_url: gl_user.avatar_url,
        };
        // Persist session.
        let api_key = format!("ak-{}", Uuid::new_v4());
        let ttl_secs = self.config.api_key_ttl_secs;
        self.auth_store
            .insert_session(
                &api_key,
                &user_info,
                &token.access_token,
                ttl_secs,
                gitlab_user_id,
            )
            .await
            .map_err(|e| WebLoginError::Internal(e.to_string()))?;
        info!(username = %user_info.username, "Web OAuth login successful");
        Ok(WebLoginOutcome {
            api_key,
            ttl_secs,
            username: user_info.username,
            next: pending.next,
        })
    }

    /// Resolve the real numeric GitLab user id, falling back to a gateway
    /// `GET /api/v4/user` for legacy sessions whose id is NULL.
    async fn resolve_gitlab_user_id(
        &self,
        gitlab_user_id: Option<i64>,
        gitlab_token: String,
    ) -> DomainResult<Option<i64>> {
        if let Some(uid) = gitlab_user_id {
            return Ok(Some(uid));
        }
        match self.gateway.fetch_user(&gitlab_token).await {
            Ok(u) => Ok(u.id),
            Err(_) => Ok(None),
        }
    }
}
