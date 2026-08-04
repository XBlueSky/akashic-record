use serde::{Deserialize, Serialize};
use std::time::Instant;

// Re-export shared token types from akashic-domain so that crates importing
// `akashic_identity::types::{ValidatedMcpToken, ValidatedPassthrough}` continue
// to compile unchanged after the port refactor (A1 Task 9).
pub use akashic_domain::types::ValidatedMcpToken;
pub use akashic_domain::types::ValidatedPassthrough;

/// User information retrieved from GitLab OAuth.
#[derive(Debug, Clone, Serialize)]
pub struct UserInfo {
    pub username: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

/// A pending verification code awaiting CLI exchange.
pub struct PendingCode {
    pub user_info: UserInfo,
    pub gitlab_token: String,
    pub gitlab_user_id: Option<i64>,
    pub expires_at: Instant,
}

/// A pending OAuth state parameter for CSRF protection.
pub struct PendingState {
    pub expires_at: Instant,
    /// Task 4 (MCP OAuth, docs-kit kit enablers): the same-origin path to
    /// redirect to after the web-login cookie is set, carried through from
    /// `/auth/web/login?next=...`. Validated same-origin-path-only at the
    /// handler layer before being stored here. `None` for the CLI OAuth flows
    /// (`oauth.rs`'s `/auth/login`, `AuthService::login_initiate`), which have
    /// no post-login redirect target of their own.
    pub next: Option<String>,
}

/// POST /auth/exchange request body.
#[derive(Deserialize)]
pub struct ExchangeRequest {
    pub code: String,
}

/// POST /auth/exchange response body.
#[derive(Serialize)]
pub struct ExchangeResponse {
    pub api_key: String,
    pub username: String,
}

/// GET /auth/callback query parameters.
#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: String,
    pub state: String,
}

/// Cached GitLab user info for the passthrough cache.
#[derive(Debug, Clone)]
pub struct CachedUser {
    pub user_id: i64,
    pub user_login: String,
}

/// POST /oauth/device_authorization request body.
#[derive(Deserialize)]
pub struct DeviceAuthorizationRequest {
    pub client_id: String,
}

/// POST /oauth/device_authorization response body (RFC 8628 §3.2).
#[derive(Serialize)]
pub struct DeviceAuthorizationResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: i64,
    pub interval: i64,
}

/// POST /oauth/token request body (RFC 8628 §3.4 device_code grant).
#[derive(Deserialize)]
pub struct DeviceTokenRequest {
    pub grant_type: String,
    pub device_code: String,
}

/// POST /oauth/token success response.
#[derive(Serialize)]
pub struct DeviceTokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
}

/// POST /device/verify and POST /device/approve form body.
#[derive(Deserialize)]
pub struct DeviceUserCodeRequest {
    pub user_code: String,
}
