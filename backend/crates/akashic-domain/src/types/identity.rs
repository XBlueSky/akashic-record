// ── Identity domain types (A1 Task 9) ────────────────────────────────────────

/// Session data row returned from the `sessions` PG table.
///
/// Used by `SessionTokenRepo::validate_session` (A1 Task 9).
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub username: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub gitlab_token: String,
    pub gitlab_user_id: Option<i64>,
}

/// Result of a successful MCP token validation (device-flow path).
///
/// Moved from `akashic-identity::types` so the port trait in `akashic-domain`
/// can reference it without depending on `akashic-identity` (A1 Task 9).
#[derive(Debug, Clone)]
pub struct ValidatedMcpToken {
    pub token_id: uuid::Uuid,
    pub user_id: i64,
    pub user_login: String,
}

/// Result of a successful GitLab PAT passthrough validation.
///
/// Moved from `akashic-identity::types` so the port trait in `akashic-domain`
/// can reference it without depending on `akashic-identity` (A1 Task 9).
#[derive(Debug, Clone)]
pub struct ValidatedPassthrough {
    pub user_id: i64,
    pub user_login: String,
    /// 16 hex chars: hex(sha256(token_bytes_after_glpat-_prefix))[..16]
    pub token_id_prefix: String,
}

/// Outcome of a quota rolling-window check.
///
/// `Ok` = under cap (or quota disabled). `Exceeded` = at or above cap.
///
/// **Intentional design**: the `check`/`consume` split is non-atomic — a TOCTOU
/// gap exists between check and consume. This is deliberate fail-open behaviour
/// per the quota spec: a small burst over the cap is acceptable and preferable
/// to blocking legitimate requests under DB load. Do NOT close this gap with a
/// transaction without coordinating with the quota-spec owner.
///
/// Moved from `akashic-identity::quota` so the port trait lives in
/// `akashic-domain` (A1 Task 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaCheck {
    Ok,
    Exceeded {
        used: u32,
        cap: u32,
        window_secs: i64,
    },
}

/// Identifies the category of a quota-usage row.
///
/// Moved from `akashic-identity::quota` (A1 Task 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageKind {
    Llm,
    Embedding,
}

impl UsageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageKind::Llm => "llm",
            UsageKind::Embedding => "embedding",
        }
    }
}

/// Result of a successful publish-token validation (docs-publish endpoint).
///
/// Publish tokens (`akp_<32hex>`) are repo-scoped bearer credentials that
/// authorize the docs-publish pipeline to push a corpus version for exactly
/// one repo. Mirrors [`ValidatedMcpToken`] (A1 Task 9) but carries
/// `repo_name` instead of `user_id`/`user_login` — a publish token
/// authenticates a repo's publish pipeline, not a human GitLab user
/// (Task 6, B1).
#[derive(Debug, Clone)]
pub struct ValidatedPublishToken {
    pub token_id: uuid::Uuid,
    pub repo_name: String,
}

/// A summary row for one publish token (user-facing list view, Task 6).
///
/// `expires_at` carries a 90-day sliding TTL, same contract as MCP tokens
/// (`McpTokenSummaryRow`). It stays `Option` in the type only to reflect the
/// DB column's nullability for any already-issued row that predates the
/// sliding TTL — such a row shows `NULL` here but no longer validates
/// (`validate_publish_token` requires `expires_at > now()`, which excludes
/// `NULL`), so a `NULL` `expires_at` reads as "expired," not "never expires."
#[derive(Debug, Clone)]
pub struct PublishTokenSummaryRow {
    pub id: uuid::Uuid,
    pub repo_name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A summary row for one MCP token (user-facing list view).
///
/// Used by `AccountAuditRepo::list_my_tokens` (A1 Task 9, deferred to A2 for
/// the adapter — SQL lives in `akashic-server::auth::account`).
#[derive(Debug, Clone)]
pub struct McpTokenSummaryRow {
    pub id: uuid::Uuid,
    pub label: Option<String>,
    pub issued_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A single audit-log entry returned by the user-facing audit endpoint.
///
/// Used by `AccountAuditRepo::list_my_audit` (A1 Task 9, deferred to A2 for
/// the adapter — SQL lives in `akashic-server::auth::account`).
#[derive(Debug, Clone)]
pub struct AuditEntryRow {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub action: String,
    pub target_id: Option<String>,
    pub actor_token_id: String,
    pub ip: Option<String>,
    pub response_summary: Option<String>,
}

/// Result of successfully redeeming an MCP OAuth authorization code
/// (RFC 6749 §4.1.3, Task 4 — "MCP OAuth (二)").
///
/// Returned by `OauthCodeRepo::consume_code` on the ONE call that wins the
/// atomic claim (`used_at` CAS). The `/oauth/token` handler still must check
/// `client_id`/`redirect_uri` match the request and that
/// `BASE64URL(SHA256(code_verifier)) == code_challenge` before minting a
/// token — a successful `consume_code` only proves the code was valid,
/// unused, and unexpired, not that this specific token request is the one
/// authorized to redeem it.
///
/// `client_id` is a CIMD `client_id` URL (spec §4, MCP refactor 2026-08-07),
/// not a database identifier — the `/oauth/token` handler compares it against
/// the request's `client_id` with plain string equality (`==`), byte-for-byte.
#[derive(Debug, Clone)]
pub struct ConsumedOauthCode {
    pub client_id: String,
    pub user_id: i64,
    pub user_login: String,
    pub code_challenge: String,
    pub redirect_uri: String,
}

/// Input to `OauthConsentRepo::issue_pending` (spec §4, MCP refactor
/// 2026-08-07): the CIMD-validated client + PKCE/state metadata carried from
/// `GET /oauth/authorize` into the pending-consent row.
///
/// `client_id` is the CIMD URL the client presented (not the document's
/// self-reported `client_id`, though CIMD validation already requires the two
/// to match modulo trailing slash — see `cimd::CimdFetcher::fetch_and_validate`)
/// so it round-trips byte-for-byte into `OauthCodeRepo::issue_code` and, from
/// there, into the `/oauth/token` handler's string comparison.
#[derive(Debug, Clone)]
pub struct PendingConsentInput {
    pub client_id: String,
    pub client_name: Option<String>,
    pub redirect_uri: String,
    pub oauth_state: String,
    pub code_challenge: String,
}

/// Result of successfully redeeming a pending consent
/// (`OauthConsentRepo::redeem_pending`, spec §4, MCP refactor 2026-08-07).
///
/// Same fields as [`PendingConsentInput`] plus the session identity
/// (`user_id`/`user_login`) the row was bound to at `issue_pending` time —
/// the `/oauth/authorize/consent` handler needs these to call
/// `OauthCodeRepo::issue_code` on approval.
#[derive(Debug, Clone)]
pub struct PendingConsent {
    pub client_id: String,
    pub client_name: Option<String>,
    pub redirect_uri: String,
    pub oauth_state: String,
    pub code_challenge: String,
    pub user_id: i64,
    pub user_login: String,
}

/// Result from `DeviceFlowRepo::poll_device_token`.
///
/// Carries the pre-update row state plus the computed `slow_down` flag.
/// The interval bump has already been committed by the time this is returned.
#[derive(Debug, Clone)]
pub struct DevicePollResult {
    /// Current `status` value BEFORE the poll update.
    pub status: String,
    /// `expires_at` — compared by the service after the Tx commits.
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// `granted_user_id` — `Some` only when status is `approved`.
    pub granted_user_id: Option<i64>,
    /// `granted_user_login` — `Some` only when status is `approved`.
    pub granted_user_login: Option<String>,
    /// Whether the client polled too fast (previous `last_polled_at` was within
    /// `interval_secs` of now). The service returns `slow_down` when `true`.
    pub slow_down: bool,
}
