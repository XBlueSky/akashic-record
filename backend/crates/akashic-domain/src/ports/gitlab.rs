//! GitLab gateway port.
//!
//! The single infra-free abstraction over every GitLab HTTP call the backend
//! makes. The only adapter is `akashic_gitlab::ReqwestGitLabGateway`, which
//! owns the one `reqwest::Client`. Services depend on `Arc<dyn GitLabGateway>`;
//! no handler touches GitLab HTTP directly.

use async_trait::async_trait;

use crate::ports::services::GitLabBranch;
use crate::types::ValidationReport;

/// OAuth token-exchange result. Only `access_token` is consumed today; extra
/// GitLab fields are ignored on deserialize.
#[derive(Debug, Clone)]
pub struct GitLabToken {
    pub access_token: String,
}

/// A GitLab user as returned by `GET /api/v4/user`. `id` is optional because
/// the web/device OAuth flows tolerate a missing id (legacy), while passthrough
/// validation treats `None` as "not a usable identity".
#[derive(Debug, Clone)]
pub struct GitLabUser {
    pub id: Option<i64>,
    pub username: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

/// Result of a successful web-OAuth login, handed back to the HTTP handler
/// which owns the cookie + redirect (HTTP concerns stay handler-side).
#[derive(Debug, Clone)]
pub struct WebLoginOutcome {
    pub api_key: String,
    pub ttl_secs: u64,
    pub username: String,
    /// Task 4 (MCP OAuth, docs-kit kit enablers): the same-origin path the
    /// browser should land on after the cookie is set, carried from the
    /// `PendingState` the CSRF `state` param resolved to. `Some` only when
    /// `/auth/web/login` was entered with a validated `?next=` (e.g. the
    /// backend's own `/oauth/authorize?...` URL, so a user who hits
    /// `/oauth/authorize` with no session resumes the authorization-code
    /// flow after logging in instead of landing on the frontend home).
    /// `None` reproduces the pre-Task-4 behavior (redirect to `frontend_url`).
    pub next: Option<String>,
}

/// Typed error for `AuthService::complete_web_login` so the handler can map to
/// exact HTTP statuses without string-matching an `anyhow` chain.
#[derive(Debug)]
pub enum WebLoginError {
    /// CSRF state missing/expired → 400 Bad Request.
    InvalidState,
    /// GitLab unreachable / malformed response → 502 Bad Gateway.
    Upstream(String),
    /// Session persistence failed → 500 Internal Server Error.
    Internal(String),
}

impl std::fmt::Display for WebLoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebLoginError::InvalidState => write!(f, "invalid_or_expired_state"),
            WebLoginError::Upstream(e) => write!(f, "gitlab_upstream: {e}"),
            WebLoginError::Internal(e) => write!(f, "internal: {e}"),
        }
    }
}
impl std::error::Error for WebLoginError {}

#[async_trait]
pub trait GitLabGateway: Send + Sync {
    /// `POST {gitlab}/oauth/token` (authorization_code grant). `redirect_uri`
    /// is caller-supplied so the web flow (web redirect uri) and the device /
    /// deprecated-browser flow (redirect uri) share one method.
    async fn exchange_oauth_code(
        &self,
        code: &str,
        redirect_uri: &str,
    ) -> anyhow::Result<GitLabToken>;

    /// `GET /api/v4/user` with a bearer access token.
    async fn fetch_user(&self, access_token: &str) -> anyhow::Result<GitLabUser>;

    /// PAT probe: `GET /api/v4/user` with a `glpat-*`. Returns `Ok(None)` on
    /// 401 / 5xx / network error (fail-closed); `Ok(Some(user))` on 200.
    async fn validate_passthrough(&self, pat: &str) -> anyhow::Result<Option<GitLabUser>>;

    /// Resolve the caller's effective access level for `repo` (max of project
    /// and group access) via `GET /api/v4/projects`. `0` when no match.
    async fn project_access_level(&self, repo: &str, token: &str) -> anyhow::Result<i32>;

    /// Paginated branch list for `repo` using the caller's OAuth token.
    async fn list_branches(&self, repo: &str, token: &str) -> anyhow::Result<Vec<GitLabBranch>>;

    /// The 6-check OAuth runtime suite (3 static URI-consistency + 3 network
    /// probes) against the configured GitLab instance. Infallible — failures
    /// are encoded as `CheckStatus::Fail`/`Warn` inside the report.
    async fn runtime_report(&self) -> ValidationReport;
}
