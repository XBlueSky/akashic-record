//! Port traits for identity / auth persistence (PostgreSQL side).
//!
//! **Scope (A1 Task 9):** session tokens, MCP tokens, passthrough tombstones,
//! per-actor quota DAL.  Account-audit SQL (`AccountAuditRepo`) lives in
//! `akashic-server::auth::account` and its adapter is deferred to A2.
//!
//! All traits are infra-free: they reference only domain types from
//! `akashic_domain::types`.  Adapter implementations live in
//! `akashic-store-pg`.
//!
//! ## TOCTOU / fail-open note
//!
//! `QuotaRepo::check` and `QuotaRepo::consume` are intentionally non-atomic.
//! The gap between `check` and `consume` allows a small burst over the cap,
//! which is acceptable by the quota spec (fail-open SLO).  Do NOT wrap these
//! in a single transaction without coordinating with the spec owner.

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::{
    AuditEntryRow, ConsumedOauthCode, DevicePollResult, McpTokenSummaryRow, PendingConsent,
    PendingConsentInput, PublishTokenSummaryRow, QuotaCheck, SessionRow, UsageKind,
    ValidatedMcpToken, ValidatedPublishToken,
};

// ── SessionTokenRepo ──────────────────────────────────────────────────────────

/// Repository contract for session-token persistence.
///
/// Sessions map an `api_key` cookie to GitLab user information and are stored
/// in the `sessions` PG table with an optional TTL.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn SessionTokenRepo>`.
#[async_trait]
pub trait SessionTokenRepo: Send + Sync {
    /// Validate an API key and return the associated user info, GitLab token,
    /// and numeric GitLab user id.
    ///
    /// Returns `None` if the key is unknown or expired.
    ///
    /// SQL moved verbatim from `akashic-identity::store::AuthStore::validate_session`
    /// (A1 Task 9).
    async fn validate_session(&self, api_key: &str) -> anyhow::Result<Option<SessionRow>>;

    /// Insert (or update on conflict) a session in the `sessions` table.
    ///
    /// When `ttl_secs == 0` the session does not expire (`expires_at = NULL`).
    ///
    /// SQL moved verbatim from `akashic-identity::store::AuthStore::insert_session`
    /// (A1 Task 9).
    #[allow(clippy::too_many_arguments)] // verbatim DAL signature (session fields + identity)
    async fn insert_session(
        &self,
        api_key: &str,
        username: &str,
        name: Option<&str>,
        avatar_url: Option<&str>,
        gitlab_token: &str,
        ttl_secs: u64,
        gitlab_user_id: Option<i64>,
    ) -> anyhow::Result<()>;

    /// Delete a session from the `sessions` table by API key. Idempotent.
    ///
    /// SQL moved verbatim from `akashic-identity::store::AuthStore::remove_session`
    /// (A1 Task 9).
    async fn remove_session(&self, api_key: &str) -> anyhow::Result<()>;

    /// Delete all rows from `sessions` where `expires_at < now()`.
    ///
    /// SQL moved verbatim from `akashic-identity::store::AuthStore::cleanup_expired`
    /// (A1 Task 9).
    async fn cleanup_expired_sessions(&self) -> anyhow::Result<()>;
}

// ── McpTokenRepo ──────────────────────────────────────────────────────────────

/// Repository contract for MCP bearer-token persistence.
///
/// MCP tokens are stored in the `mcp_tokens` PG table with a 90-day sliding
/// TTL.  The DB stores only the sha256 of the secret portion of the token.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn McpTokenRepo>`.
#[async_trait]
pub trait McpTokenRepo: Send + Sync {
    /// Issue a new MCP bearer token for a GitLab user.
    ///
    /// Returns `(token_id, plaintext)`.  The plaintext is shown to the user
    /// once and never persisted; the row stores `sha256(plaintext_bytes_after_ak_)`.
    ///
    /// Token format: `"ak_"` + 32 hex chars (UUIDv4 with hyphens removed).
    /// The hash stored in the DB is `sha256` of the 32-hex secret ONLY
    /// (bytes after the `ak_` prefix) — the validator must strip the prefix
    /// before hashing or the lookup will miss.
    ///
    /// SQL moved verbatim from `akashic-identity::store::AuthStore::issue_mcp_token`
    /// (A1 Task 9).
    async fn issue_mcp_token(
        &self,
        user_id: i64,
        user_login: &str,
        label: Option<&str>,
    ) -> anyhow::Result<(Uuid, String)>;

    /// Validate a presented MCP bearer.
    ///
    /// Returns `Some(ValidatedMcpToken)` on hit (token exists, not expired,
    /// not revoked).  Returns `None` for: wrong prefix, malformed shape,
    /// unknown hash, expired, or revoked.
    ///
    /// On a hit, fires a background `UPDATE` to refresh `last_used_at` and
    /// slide `expires_at` to `now() + 90 days` — but only if the previous
    /// `last_used_at` is older than 60 s (debounce, spec R-11).
    ///
    /// SQL moved verbatim from `akashic-identity::store::AuthStore::validate_mcp_token`
    /// (A1 Task 9).
    async fn validate_mcp_token(
        &self,
        presented: &str,
    ) -> anyhow::Result<Option<ValidatedMcpToken>>;

    /// Mark an MCP token revoked. Idempotent.
    ///
    /// SQL moved verbatim from `akashic-identity::store::AuthStore::revoke_mcp_token`
    /// (A1 Task 9).
    async fn revoke_mcp_token(&self, token_id: Uuid) -> anyhow::Result<()>;
}

// ── PublishTokenRepo ──────────────────────────────────────────────────────────

/// Repository contract for repo-scoped docs-publish bearer-token persistence
/// (Task 6, B1).
///
/// Publish tokens are stored in the `publish_tokens` PG table. Unlike MCP
/// tokens (scoped to a human GitLab user), a publish token is scoped to a
/// single `repo_name` and authorizes only the docs-publish endpoint for that
/// repo. The DB stores only the sha256 of the secret portion of the token —
/// mirrors [`McpTokenRepo`]'s hashing discipline exactly, with a distinct
/// `akp_` prefix (vs. `ak_`) so the two token families are never confused at
/// a glance or in logs.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn PublishTokenRepo>`.
#[async_trait]
pub trait PublishTokenRepo: Send + Sync {
    /// Issue a new publish token for `repo_name`, attributed to `created_by`
    /// (the issuing user's GitLab username).
    ///
    /// Returns `(token_id, plaintext)`. The plaintext is shown once and never
    /// persisted; the row stores `sha256(plaintext_bytes_after_akp_)`.
    ///
    /// Token format: `"akp_"` + 32 hex chars (UUIDv4 with hyphens removed).
    /// `expires_at` is set to `now() + 90 days` and slides forward on each
    /// successful [`Self::validate_publish_token`] call — same sliding-TTL
    /// contract as [`McpTokenRepo::issue_mcp_token`].
    async fn issue_publish_token(
        &self,
        repo_name: &str,
        created_by: &str,
    ) -> anyhow::Result<(Uuid, String)>;

    /// Validate a presented publish bearer.
    ///
    /// Returns `Some(ValidatedPublishToken)` on hit (token exists, not
    /// expired, not revoked). Returns `None` for: wrong prefix, malformed
    /// shape, unknown hash, expired, or revoked.
    async fn validate_publish_token(
        &self,
        presented: &str,
    ) -> anyhow::Result<Option<ValidatedPublishToken>>;

    /// Mark a publish token revoked. Idempotent.
    async fn revoke_publish_token(&self, token_id: Uuid) -> anyhow::Result<()>;

    /// List all non-plaintext, non-hash metadata for tokens issued by
    /// `created_by`, newest first.
    async fn list_publish_tokens(
        &self,
        created_by: &str,
    ) -> anyhow::Result<Vec<PublishTokenSummaryRow>>;

    /// Check ownership of a publish token: returns the `created_by` username
    /// who issued it, or `None` if the token does not exist. Mirrors
    /// `AccountAuditRepo::check_token_ownership`, but keyed by username
    /// (publish tokens have no numeric owner) rather than `user_id`.
    async fn check_publish_token_ownership(&self, token_id: Uuid)
    -> anyhow::Result<Option<String>>;

    /// Record an `audit_log` row for a publish-token issue/revoke action.
    async fn record_publish_token_audit(
        &self,
        actor_user_id: i64,
        actor_token_id: &str,
        auth_method: &str,
        action: &str,
        target_id: &str,
    ) -> anyhow::Result<()>;
}

// ── PassthroughTokenRepo ─────────────────────────────────────────────────────

/// Repository contract for GitLab PAT passthrough tombstone persistence.
///
/// Only the two DB-touching operations are ported here.  The full
/// `validate_passthrough_token` (GitLab HTTP probe + LRU cache + tombstone
/// check) remains on `AuthStore` because it requires IO beyond the PG boundary.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn PassthroughTokenRepo>`.
#[async_trait]
pub trait PassthroughTokenRepo: Send + Sync {
    /// Tombstone a passthrough PAT prefix.  Idempotent via ON CONFLICT DO NOTHING.
    ///
    /// `prefix` is `hex(sha256(token_bytes_after_glpat-_prefix))[..16]`.
    ///
    /// SQL moved verbatim from
    /// `akashic-identity::store::AuthStore::revoke_passthrough_token`
    /// (A1 Task 9).
    async fn revoke_passthrough_token(&self, prefix: &str, revoked_by: i64) -> anyhow::Result<()>;

    /// Check whether a prefix is in the `revoked_passthrough_tokens` tombstone
    /// table.  Returns `true` if the prefix is present (token is revoked).
    ///
    /// SQL moved verbatim from
    /// `akashic-identity::store::AuthStore::validate_passthrough_token`
    /// (the tombstone-check SELECT, A1 Task 9).
    async fn check_passthrough_revoked(&self, prefix: &str) -> anyhow::Result<bool>;
}

// ── QuotaRepo ─────────────────────────────────────────────────────────────────

/// Repository contract for the per-actor rolling-window quota DAL.
///
/// **Engine logic is NOT here.**  The `Quota` struct (enabled flag,
/// `cap_per_window`, `window_secs`, fail-open error handling, `CURRENT_ACTOR`
/// task-local, provider-decorator wrapping) remains in
/// `akashic-identity::quota`.  Only the two SQL operations are ported.
///
/// **TOCTOU / fail-open design**: `check` and `consume` are separate, non-atomic
/// calls.  A small burst past the cap is permissible — see module-level docs.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn QuotaRepo>`.
#[async_trait]
pub trait QuotaRepo: Send + Sync {
    /// Sum tokens used in a rolling window for an actor and compare against cap.
    ///
    /// Returns `QuotaCheck::Ok` if the sum is strictly below `cap`.
    /// On DB error: logs via `tracing::error!` and returns `Ok` (fail-open).
    ///
    /// SQL moved verbatim from `akashic-identity::quota::Quota::check`
    /// (A1 Task 9).
    async fn check(&self, actor_user_id: i64, cap_per_window: u32, window_secs: i64) -> QuotaCheck;

    /// Append a usage row to `llm_usage`. Discards DB errors via tracing.
    ///
    /// SQL moved verbatim from `akashic-identity::quota::Quota::consume`
    /// (A1 Task 9).
    async fn consume(
        &self,
        actor_user_id: i64,
        actor_token_id: &str,
        kind: UsageKind,
        tokens_used: u32,
        model: &str,
    );
}

// ── DeviceFlowRepo ────────────────────────────────────────────────────────────

/// Repository contract for the `device_flow_pending` table.
///
/// **Critical invariant**: the `poll_device_token` operation MUST execute
/// `SELECT … FOR UPDATE` and the subsequent `UPDATE` inside a SINGLE transaction
/// to prevent the poll race.  The adapter starts its own transaction and commits
/// before returning so the service holds no raw DB handle.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn DeviceFlowRepo>`.
#[async_trait]
pub trait DeviceFlowRepo: Send + Sync {
    /// Insert a new pending device-flow row (unique on device_code + user_code).
    ///
    /// Returns `Ok(())` on success, `Err` on UNIQUE collision or DB error.
    /// The caller retries with freshly-generated codes on collision.
    ///
    /// SQL moved verbatim from `akashic-server::auth::oauth_device::device_authorization`.
    async fn insert_pending(
        &self,
        device_code: &str,
        user_code: &str,
        client_id: &str,
    ) -> anyhow::Result<()>;

    /// Atomically poll a device_code: `SELECT … FOR UPDATE` + `UPDATE` in ONE Tx.
    ///
    /// Returns `None` if no row matches `device_code` (→ `invalid_grant`).
    /// Returns `Some(DevicePollResult)` with the pre-update state and the
    /// computed `slow_down` flag.  The interval bump has already been committed
    /// when this method returns.
    ///
    /// SQL moved verbatim from `akashic-server::auth::oauth_device::device_token`.
    async fn poll_device_token(
        &self,
        device_code: &str,
    ) -> anyhow::Result<Option<DevicePollResult>>;

    /// Update `status = 'pre_approved'`, `granted_user_id`, `granted_user_login`
    /// for a `user_code` where `status = 'pending'` and not expired.
    ///
    /// Returns `true` if exactly one row was updated (match found), `false` if
    /// the code was not found, expired, or already used.
    ///
    /// SQL moved verbatim from `akashic-server::auth::oauth_device::device_verify`.
    async fn set_pre_approved(
        &self,
        user_code: &str,
        user_id: i64,
        user_login: &str,
    ) -> anyhow::Result<bool>;

    /// Update `status = 'approved'` for a `user_code` where
    /// `status = 'pre_approved'`, `granted_user_id = user_id`, and not expired.
    ///
    /// Returns `true` if exactly one row was updated, `false` otherwise.
    ///
    /// SQL moved verbatim from `akashic-server::auth::oauth_device::device_approve`.
    async fn set_approved(&self, user_code: &str, user_id: i64) -> anyhow::Result<bool>;

    /// Mark a device-flow row as `status = 'exchanged'` and record the
    /// `granted_token_id`.  Only succeeds when current `status = 'approved'`
    /// (exactly-once exchange guard — returns `true` if 1 row was updated).
    ///
    /// SQL moved verbatim from the approved-branch of `device_token`.
    async fn mark_exchanged(&self, device_code: &str, token_id: uuid::Uuid)
    -> anyhow::Result<bool>;
}

// ── AccountAuditRepo ──────────────────────────────────────────────────────────

/// Repository contract for account self-management reads (MCP token list +
/// audit log).
///
/// **A2 deferred**: SQL for all methods currently lives in
/// `akashic-server::auth::account` (HTTP handlers) and will move to
/// `akashic-store-pg` in Slice A2.  This trait is defined here so domain
/// consumers can reference it without touching `akashic-server`.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn AccountAuditRepo>`.
#[async_trait]
pub trait AccountAuditRepo: Send + Sync {
    /// List all non-revoked MCP tokens for a user, newest-first.
    ///
    /// SQL will be moved from `akashic-server::auth::account::list_my_tokens`
    /// in A2.
    async fn list_my_tokens(&self, user_id: i64) -> anyhow::Result<Vec<McpTokenSummaryRow>>;

    /// Check ownership of an MCP token: returns the `user_id` who owns it, or
    /// `None` if the token does not exist.
    ///
    /// SQL will be moved from `akashic-server::auth::account::revoke_my_token`
    /// (the ownership-check SELECT) in A2.
    async fn check_token_ownership(&self, token_id: Uuid) -> anyhow::Result<Option<i64>>;

    /// List audit-log entries for a user, newest-first, up to `limit` rows.
    ///
    /// SQL will be moved from `akashic-server::auth::account::list_my_audit`
    /// in A2.
    async fn list_my_audit(&self, user_id: i64, limit: i64) -> anyhow::Result<Vec<AuditEntryRow>>;
}

// ── OauthCodeRepo ─────────────────────────────────────────────────────────────

/// Repository contract for MCP OAuth authorization-code persistence (RFC 6749
/// §4.1, Task 4 — "MCP OAuth (二)").
///
/// Codes are stored in the `mcp_oauth_codes` PG table (created by Task 3's
/// `init_auth_schema`). The DB stores only `sha256(code)` — the plaintext
/// code is returned once by [`Self::issue_code`] and never persisted, mirroring
/// [`McpTokenRepo`]'s hashing discipline.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn OauthCodeRepo>`.
#[async_trait]
pub trait OauthCodeRepo: Send + Sync {
    /// Issue a new one-time authorization code for `client_id` + `user_id`,
    /// binding the PKCE `code_challenge` and the exact `redirect_uri` this
    /// code may be redeemed against. `client_id` is a CIMD `client_id` URL
    /// (spec §4, MCP refactor 2026-08-07) — NOT a database identifier —
    /// persisted verbatim as the exact string the client presented, so the
    /// `/oauth/token` handler can compare it byte-for-byte against the
    /// `client_id` the token request presents. Returns the plaintext code
    /// (64 lowercase hex chars — 32 random bytes); the row stores only its
    /// sha256. `expires_at` is fixed at `now() + 10 minutes` (RFC 6749
    /// §4.1.2 recommends a short-lived code) and is not configurable per
    /// call.
    #[allow(clippy::too_many_arguments)]
    async fn issue_code(
        &self,
        client_id: &str,
        user_id: i64,
        user_login: &str,
        code_challenge: &str,
        redirect_uri: &str,
    ) -> anyhow::Result<String>;

    /// Atomically redeem a presented code: exactly one caller can ever
    /// observe `Some` for a given code, even under concurrent exchange
    /// attempts — the adapter's `UPDATE ... SET used_at = now() WHERE
    /// code_hash = $1 AND used_at IS NULL AND expires_at > now() RETURNING
    /// ...` claims the row and reads its data in one statement, so there is
    /// no separate check-then-claim race window.
    ///
    /// Returns `None` for: unknown code, already-used code, or expired code
    /// — the caller (the `/oauth/token` handler) collapses all three into a
    /// single RFC 6749 `invalid_grant` response, which is both spec-correct
    /// and avoids leaking which specific reason a presented code failed.
    async fn consume_code(&self, presented: &str) -> anyhow::Result<Option<ConsumedOauthCode>>;
}

// ── OauthConsentRepo ──────────────────────────────────────────────────────────

/// Repository contract for the MCP OAuth pending-consent handshake (spec §4,
/// MCP refactor 2026-08-07 — CIMD validation + consent screen replace DCR).
///
/// `GET /oauth/authorize` no longer mints a code directly: once CIMD
/// validation passes and a web session is present, it creates a *pending
/// consent* row (bound to the session's `user_id` — see the CSRF note below)
/// and renders a confirmation page. `POST /oauth/authorize/consent` redeems
/// that row and, on approval, calls [`OauthCodeRepo::issue_code`] to mint the
/// actual authorization code. Rows live in `mcp_oauth_pending_consents` with a
/// fixed 10-minute TTL, mirroring [`OauthCodeRepo`]'s short-lived-artifact
/// discipline.
///
/// **CSRF note**: [`Self::redeem_pending`] requires the SAME `user_id` that
/// created the row (`AND user_id = $2` in the adapter's SQL, not just
/// `consent_id`). Combined with the id being an unguessable `UUID` and the
/// atomic CAS making it single-use, a third party cannot forge or replay a
/// consent approval even if they can make the victim's browser POST to this
/// endpoint (classic CSRF) — the row simply won't belong to whatever session
/// the forged request carries, if any.
///
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn OauthConsentRepo>`.
#[async_trait]
pub trait OauthConsentRepo: Send + Sync {
    /// Create a new pending-consent row for `user_id`/`user_login`, carrying
    /// the CIMD-validated client + PKCE/state metadata from the authorize
    /// request. Returns the generated `consent_id` (rendered into the
    /// consent page's hidden form field). `expires_at` is fixed at
    /// `now() + 10 minutes`.
    async fn issue_pending(
        &self,
        user_id: i64,
        user_login: &str,
        meta: &PendingConsentInput,
    ) -> anyhow::Result<Uuid>;

    /// Atomically redeem a presented `consent_id`: the adapter's `UPDATE ...
    /// SET used_at = now() WHERE consent_id = $1 AND user_id = $2 AND
    /// used_at IS NULL AND expires_at > now() RETURNING ...` mirrors
    /// [`OauthCodeRepo::consume_code`]'s CAS exactly, with the additional
    /// `user_id` bind that makes this the CSRF defense described on the
    /// trait doc comment.
    ///
    /// Returns `None` for: unknown consent_id, already-used, expired, or
    /// `user_id` mismatch — the `/oauth/authorize/consent` handler collapses
    /// all four into a single 400 `invalid_request`, avoiding an oracle that
    /// would tell a caller which specific reason a presented id failed.
    async fn redeem_pending(
        &self,
        consent_id: Uuid,
        user_id: i64,
    ) -> anyhow::Result<Option<PendingConsent>>;
}
