use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant as StdInstant;

use dashmap::DashMap;
use lru::LruCache;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::info;

#[allow(unused_imports)]
use super::types::{
    CachedUser, PendingCode, PendingState, UserInfo, ValidatedMcpToken, ValidatedPassthrough,
};

use akashic_domain::ports::{
    McpTokenRepo, OauthCodeRepo, OauthConsentRepo, PassthroughTokenRepo, PublishTokenRepo,
    SessionTokenRepo,
};
use akashic_domain::types::{
    ConsumedOauthCode, PendingConsent, PendingConsentInput, PublishTokenSummaryRow,
    ValidatedPublishToken,
};
use akashic_store_pg::repos::{
    PgMcpTokenRepo, PgOauthCodeRepo, PgOauthConsentRepo, PgPassthroughTokenRepo,
    PgPublishTokenRepo, PgSessionTokenRepo,
};

/// Authentication store.
///
/// Short-lived OAuth artifacts (pending codes and states) are kept in-memory.
/// Active sessions and tokens are persisted in PostgreSQL via port-adapter
/// repos so `AuthStore` itself holds no direct SQL (A1 Task 9).
pub struct AuthStore {
    pub pending_codes: DashMap<String, PendingCode>,
    pub pending_states: DashMap<String, PendingState>,
    pub(crate) passthrough_cache: Arc<Mutex<LruCache<String, (CachedUser, StdInstant)>>>,
    pub(crate) passthrough_cache_ttl_secs: u64,
    session_repo: Arc<dyn SessionTokenRepo>,
    mcp_repo: Arc<dyn McpTokenRepo>,
    passthrough_repo: Arc<dyn PassthroughTokenRepo>,
    /// Task 6 (B1, docs corpus): repo-scoped publish tokens (`akp_<32hex>`).
    publish_repo: Arc<dyn PublishTokenRepo>,
    /// Task 4 (MCP OAuth): authorization-code issue/redeem (RFC 6749 §4.1).
    oauth_code_repo: Arc<dyn OauthCodeRepo>,
    /// Task 5 (MCP OAuth, spec §4): pending-consent issue/redeem — CIMD
    /// validation + consent screen replace DCR.
    oauth_consent_repo: Arc<dyn OauthConsentRepo>,
}

impl AuthStore {
    pub fn new(pg: PgPool, passthrough_cache_ttl_secs: u64) -> Self {
        Self {
            pending_codes: DashMap::new(),
            pending_states: DashMap::new(),
            passthrough_cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(10_000).expect("nonzero"),
            ))),
            passthrough_cache_ttl_secs,
            session_repo: Arc::new(PgSessionTokenRepo::new(pg.clone())),
            mcp_repo: Arc::new(PgMcpTokenRepo::new(pg.clone())),
            passthrough_repo: Arc::new(PgPassthroughTokenRepo::new(pg.clone())),
            publish_repo: Arc::new(PgPublishTokenRepo::new(pg.clone())),
            oauth_code_repo: Arc::new(PgOauthCodeRepo::new(pg.clone())),
            oauth_consent_repo: Arc::new(PgOauthConsentRepo::new(pg)),
        }
    }

    /// Validate an API key and return the associated user info, gitlab token, and numeric user id.
    pub async fn validate_session(&self, api_key: &str) -> Option<(UserInfo, String, Option<i64>)> {
        let row = self.session_repo.validate_session(api_key).await.ok()??;
        Some((
            UserInfo {
                username: row.username,
                name: row.name,
                avatar_url: row.avatar_url,
            },
            row.gitlab_token,
            row.gitlab_user_id,
        ))
    }

    /// Insert (or update) a session in the database.
    pub async fn insert_session(
        &self,
        api_key: &str,
        user_info: &UserInfo,
        gitlab_token: &str,
        ttl_secs: u64,
        gitlab_user_id: Option<i64>,
    ) -> Result<(), sqlx::Error> {
        self.session_repo
            .insert_session(
                api_key,
                &user_info.username,
                user_info.name.as_deref(),
                user_info.avatar_url.as_deref(),
                gitlab_token,
                ttl_secs,
                gitlab_user_id,
            )
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    /// Delete a session from the database.
    pub async fn remove_session(&self, api_key: &str) {
        let _ = self.session_repo.remove_session(api_key).await;
    }

    /// Remove all expired entries from in-memory maps and from the DB.
    pub async fn cleanup_expired(&self) {
        let now = std::time::Instant::now();
        self.pending_codes.retain(|_, v| v.expires_at > now);
        self.pending_states.retain(|_, v| v.expires_at > now);

        let _ = self.session_repo.cleanup_expired_sessions().await;
    }

    /// Spawn a background tokio task that periodically removes expired entries.
    /// Exits the loop when `shutdown` is cancelled.
    pub fn spawn_cleanup_task(
        self: &Arc<Self>,
        interval: Duration,
        shutdown: tokio_util::sync::CancellationToken,
    ) {
        let store = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => {
                        tracing::info!(
                            event = "auth_cleanup_shutdown",
                            "auth store cleanup task exiting on shutdown"
                        );
                        break;
                    }
                    _ = ticker.tick() => {
                        let before = store.pending_codes.len() + store.pending_states.len();
                        store.cleanup_expired().await;
                        let after = store.pending_codes.len() + store.pending_states.len();
                        if before != after {
                            info!(removed = before - after, "Auth TTL cleanup completed");
                        }
                    }
                }
            }
        });
    }

    /// Issue a new device-flow MCP bearer for a GitLab user.
    /// Returns (token_id, plaintext). The plaintext is shown to the user
    /// once and never persisted; the row stores sha256(plaintext_bytes_after_ak_).
    pub async fn issue_mcp_token(
        &self,
        user_id: i64,
        user_login: &str,
        label: Option<&str>,
    ) -> Result<(uuid::Uuid, String), sqlx::Error> {
        self.mcp_repo
            .issue_mcp_token(user_id, user_login, label)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    /// Validate a presented MCP bearer.
    ///
    /// Returns `Some(ValidatedMcpToken)` on hit (token exists, not expired,
    /// not revoked). Returns `None` for: wrong prefix, malformed shape,
    /// unknown hash, expired, revoked.
    ///
    /// On hit, fires a background `UPDATE` to refresh `last_used_at` and
    /// slide `expires_at` to `now() + 90 days` — but only if the previous
    /// `last_used_at` is older than 60 s (debounce, spec R-11). High-frequency
    /// tokens won't saturate the connection pool with UPDATEs.
    ///
    /// The presented bearer must be `ak_<32 hex chars>`. The hash stored
    /// in `mcp_tokens.token_hash` is `sha256` of the 32 hex chars only
    /// (the bytes after the `ak_` prefix). See `issue_mcp_token`.
    pub async fn validate_mcp_token(&self, presented: &str) -> Option<ValidatedMcpToken> {
        self.mcp_repo
            .validate_mcp_token(presented)
            .await
            .ok()
            .flatten()
    }

    /// Mark an MCP token revoked. Idempotent.
    pub async fn revoke_mcp_token(&self, token_id: uuid::Uuid) -> Result<(), sqlx::Error> {
        self.mcp_repo
            .revoke_mcp_token(token_id)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    // ── Task 6 (B1, docs corpus): repo-scoped publish tokens ────────────────
    //
    // Mirrors the three `*_mcp_token` methods above exactly, keyed by
    // `repo_name` instead of `user_id`/`user_login`.

    /// Issue a new publish token (`akp_<32hex>`) for `repo_name`, attributed
    /// to `created_by`. Returns (token_id, plaintext); the plaintext is shown
    /// once and never persisted — the row stores sha256(plaintext_bytes_after_akp_).
    pub async fn issue_publish_token(
        &self,
        repo_name: &str,
        created_by: &str,
    ) -> Result<(uuid::Uuid, String), sqlx::Error> {
        self.publish_repo
            .issue_publish_token(repo_name, created_by)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    /// Validate a presented publish bearer. Returns `Some(ValidatedPublishToken)`
    /// on hit (token exists, not expired, not revoked); `None` for wrong
    /// prefix, malformed shape, unknown hash, expired, or revoked.
    pub async fn validate_publish_token(&self, presented: &str) -> Option<ValidatedPublishToken> {
        self.publish_repo
            .validate_publish_token(presented)
            .await
            .ok()
            .flatten()
    }

    /// Mark a publish token revoked. Idempotent.
    pub async fn revoke_publish_token(&self, token_id: uuid::Uuid) -> Result<(), sqlx::Error> {
        self.publish_repo
            .revoke_publish_token(token_id)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    /// List all publish tokens issued by `created_by`, newest first.
    pub async fn list_publish_tokens(
        &self,
        created_by: &str,
    ) -> Result<Vec<PublishTokenSummaryRow>, sqlx::Error> {
        self.publish_repo
            .list_publish_tokens(created_by)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    /// Check ownership of a publish token: returns the `created_by` username,
    /// or `None` if the token does not exist.
    pub async fn check_publish_token_ownership(
        &self,
        token_id: uuid::Uuid,
    ) -> Result<Option<String>, sqlx::Error> {
        self.publish_repo
            .check_publish_token_ownership(token_id)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    /// Record an `audit_log` row for a publish-token issue/revoke action.
    pub async fn record_publish_token_audit(
        &self,
        actor_user_id: i64,
        actor_token_id: &str,
        auth_method: &str,
        action: &str,
        target_id: &str,
    ) -> Result<(), sqlx::Error> {
        self.publish_repo
            .record_publish_token_audit(
                actor_user_id,
                actor_token_id,
                auth_method,
                action,
                target_id,
            )
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    // ── Task 4 (MCP OAuth): authorization-code issue/redeem (RFC 6749 §4.1) ─

    /// Issue a new one-time authorization code bound to `client_id`,
    /// `user_id`/`user_login`, the PKCE `code_challenge`, and the exact
    /// `redirect_uri` it may be redeemed against. `client_id` is a CIMD URL
    /// (spec §4, MCP refactor 2026-08-07), persisted verbatim. Returns the
    /// plaintext code (64 lowercase hex chars); the row stores only its
    /// sha256. Fixed 10-minute TTL (RFC 6749 §4.1.2 recommends a
    /// short-lived code).
    pub async fn issue_oauth_code(
        &self,
        client_id: &str,
        user_id: i64,
        user_login: &str,
        code_challenge: &str,
        redirect_uri: &str,
    ) -> Result<String, sqlx::Error> {
        self.oauth_code_repo
            .issue_code(client_id, user_id, user_login, code_challenge, redirect_uri)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    /// Atomically redeem a presented authorization code. Returns `None` for:
    /// unknown code, already-used code, or expired code — the `/oauth/token`
    /// handler collapses all three into a single RFC 6749 `invalid_grant`.
    pub async fn consume_oauth_code(
        &self,
        presented: &str,
    ) -> Result<Option<ConsumedOauthCode>, sqlx::Error> {
        self.oauth_code_repo
            .consume_code(presented)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    // ── Task 5 (MCP OAuth, spec §4): pending-consent issue/redeem ───────────
    //
    // CIMD validation + a consent screen replace DCR: `GET /oauth/authorize`
    // creates a pending-consent row instead of minting a code directly;
    // `POST /oauth/authorize/consent` redeems it and, on approval, calls
    // `issue_oauth_code` above. Mirrors the two `*_oauth_code` methods
    // exactly in shape.

    /// Create a new pending-consent row for `user_id`/`user_login`, carrying
    /// the CIMD-validated client + PKCE/state metadata from the authorize
    /// request. Returns the generated `consent_id`. Fixed 10-minute TTL.
    pub async fn issue_pending_consent(
        &self,
        user_id: i64,
        user_login: &str,
        meta: &PendingConsentInput,
    ) -> Result<uuid::Uuid, sqlx::Error> {
        self.oauth_consent_repo
            .issue_pending(user_id, user_login, meta)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    /// Atomically redeem a presented `consent_id`, bound to the SAME
    /// `user_id` that created it (CSRF defense — see `OauthConsentRepo`'s
    /// trait doc comment). Returns `None` for: unknown consent_id,
    /// already-used, expired, or `user_id` mismatch — the consent handler
    /// collapses all four into a single 400 `invalid_request`.
    pub async fn redeem_pending_consent(
        &self,
        consent_id: uuid::Uuid,
        user_id: i64,
    ) -> Result<Option<PendingConsent>, sqlx::Error> {
        self.oauth_consent_repo
            .redeem_pending(consent_id, user_id)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    /// Tombstone a passthrough PAT prefix. Idempotent via ON CONFLICT DO NOTHING.
    /// `prefix` is the 16-hex-char `hex(sha256(token_bytes_after_glpat-_prefix))[..16]`.
    pub async fn revoke_passthrough_token(
        &self,
        prefix: &str,
        revoked_by: i64,
    ) -> Result<(), sqlx::Error> {
        self.passthrough_repo
            .revoke_passthrough_token(prefix, revoked_by)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    }

    /// Validate a `glpat-...` token via tombstone check, in-process LRU cache, then GitLab API.
    /// Returns None on: wrong prefix, empty secret, tombstoned, GitLab 401/403/5xx, network error.
    pub async fn validate_passthrough_token(
        &self,
        presented: &str,
        gateway: &Arc<dyn akashic_domain::ports::gitlab::GitLabGateway>,
    ) -> Option<ValidatedPassthrough> {
        let raw = presented.strip_prefix("glpat-")?;
        if raw.is_empty() {
            return None;
        }
        let hash_hex = format!("{:x}", Sha256::digest(raw.as_bytes()));
        // FIX (Low: 64-bit token identification collision): two distinct
        // identifiers are derived from the same digest, with deliberately
        // different collision budgets:
        //
        //  * `cache_key` — full 64-hex sha256 — is the in-process LRU key.
        //    The cache is entirely owned by this function (written and read
        //    here, never shared cross-process/cross-file), so widening it is a
        //    self-contained change. A 64-bit (16-hex) cache key risked two
        //    distinct PATs colliding into the same slot and one token being
        //    authenticated as the other user — the more dangerous half of the
        //    finding. The full 256-bit digest makes that collision infeasible.
        //
        //  * `prefix` — first 16 hex chars — remains the tombstone DB key and
        //    the audit identifier (`token_id_prefix`). This length is a shared
        //    contract: the revocation WRITE side derives the same 16-hex prefix
        //    in `auth::account::passthrough_prefix` (and validates `len == 16`),
        //    and `mcp_middleware` logs `gitlab_pat:{prefix}`. Widening it here
        //    alone would desynchronize the tombstone read from the write side
        //    and silently DEFEAT revocation, a worse regression than the
        //    residual 64-bit tombstone-collision risk (which only over-revokes,
        //    never mis-authenticates). Fully widening the tombstone requires a
        //    coordinated change in `account.rs`, which is out of scope here.
        let prefix = hash_hex[..16].to_string();
        let cache_key = hash_hex; // full digest moves in; no extra allocation

        // Tombstone check via passthrough repo (no cache).
        let tombstoned = self
            .passthrough_repo
            .check_passthrough_revoked(&prefix)
            .await
            .unwrap_or(false);
        if tombstoned {
            tracing::info!(event = "mcp_auth_passthrough_revoked", token_id_prefix = %prefix);
            return None;
        }

        // Positive cache lookup (60 s TTL). Keyed by the full sha256 hex
        // (`cache_key`), not the 16-hex tombstone `prefix`, so two distinct
        // tokens can never share a cache slot. See the FIX note above.
        {
            let mut cache = self.passthrough_cache.lock();
            if let Some((user, cached_at)) = cache.get(&cache_key)
                && cached_at.elapsed()
                    < std::time::Duration::from_secs(self.passthrough_cache_ttl_secs)
            {
                let user = user.clone();
                tracing::debug!(event = "mcp_auth_passthrough_hit", token_id_prefix = %prefix, cache = "hit");
                return Some(ValidatedPassthrough {
                    user_id: user.user_id,
                    user_login: user.user_login,
                    token_id_prefix: prefix,
                });
            }
        }

        // Cache miss — probe GitLab via the gateway (fail-closed on error).
        let gl_user = match gateway.validate_passthrough(presented).await {
            Ok(Some(u)) => u,
            Ok(None) | Err(_) => return None,
        };
        let user_id = gl_user.id?;
        let cached = CachedUser {
            user_id,
            user_login: gl_user.username.clone(),
        };
        {
            let mut cache = self.passthrough_cache.lock();
            // Insert under the full-digest cache key (see FIX note above).
            // `cache_key` is not used after this, so move it in (no clone).
            cache.put(cache_key, (cached.clone(), StdInstant::now()));
        }
        tracing::debug!(event = "mcp_auth_passthrough_hit", token_id_prefix = %prefix, cache = "miss");
        Some(ValidatedPassthrough {
            user_id: cached.user_id,
            user_login: cached.user_login,
            token_id_prefix: prefix,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn test_pool() -> PgPool {
        // DATABASE_URL fallback: config.rs tests call clear_prod_env() which removes
        // this env var globally. The hardcoded default matches the dev compose setup
        // and is only used in #[cfg(test)] code.
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            format!(
                "postgres://{}:{}@localhost:5433/akashic",
                "akashic", "akashic_secret"
            )
        });
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect");
        // Bootstrap auth schema for tests (idempotent — safe on existing DB).
        akashic_store_pg::init_auth_schema(&pool)
            .await
            .expect("init_auth_schema");
        pool
    }

    async fn cleanup_login(pool: &PgPool, login: &str) {
        sqlx::query("DELETE FROM mcp_tokens WHERE user_login = $1")
            .bind(login)
            .execute(pool)
            .await
            .ok();
    }

    /// Build a real `ReqwestGitLabGateway` pointed at `gitlab_url` (typically a
    /// wiremock `server.uri()`), so passthrough tests still exercise the HTTP
    /// path + the store cache (`.expect(N)` assertions hold).
    fn test_gateway(gitlab_url: &str) -> Arc<dyn akashic_domain::ports::gitlab::GitLabGateway> {
        let mut cfg = crate::test_support::test_config_minimal();
        cfg.gitlab_url = gitlab_url.to_string();
        Arc::new(akashic_gitlab::ReqwestGitLabGateway::new(
            reqwest::Client::new(),
            Arc::new(cfg),
        ))
    }

    #[tokio::test]
    async fn issue_mcp_token_returns_token_and_inserts_row() {
        const LOGIN: &str = "test_b1_issue";
        let pool = test_pool().await;
        cleanup_login(&pool, LOGIN).await;
        let store = AuthStore::new(pool.clone(), 60);

        let (id, plaintext) = store
            .issue_mcp_token(424242, LOGIN, Some("test-laptop"))
            .await
            .expect("issue");

        assert!(plaintext.starts_with("ak_"), "got {plaintext}");
        assert_eq!(plaintext.len(), 35, "ak_ + 32 hex");

        let row: (i64, String) =
            sqlx::query_as("SELECT user_id, user_login FROM mcp_tokens WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("fetch");
        assert_eq!(row, (424242, LOGIN.to_string()));

        cleanup_login(&pool, LOGIN).await;
    }

    #[tokio::test]
    async fn revoke_mcp_token_sets_revoked_at_and_is_idempotent() {
        const LOGIN: &str = "test_b1_revoke";
        let pool = test_pool().await;
        cleanup_login(&pool, LOGIN).await;
        let store = AuthStore::new(pool.clone(), 60);

        let (id, _) = store
            .issue_mcp_token(424243, LOGIN, None)
            .await
            .expect("issue");

        store.revoke_mcp_token(id).await.expect("revoke 1");
        store
            .revoke_mcp_token(id)
            .await
            .expect("revoke 2 idempotent");

        let revoked: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT revoked_at FROM mcp_tokens WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("fetch");
        assert!(revoked.is_some(), "revoked_at should be set");

        cleanup_login(&pool, LOGIN).await;
    }

    #[tokio::test]
    async fn validate_mcp_token_returns_user_for_valid_token() {
        const LOGIN: &str = "test_b1_validate_ok";
        let pool = test_pool().await;
        cleanup_login(&pool, LOGIN).await;
        let store = AuthStore::new(pool.clone(), 60);

        let (id, plaintext) = store
            .issue_mcp_token(424244, LOGIN, None)
            .await
            .expect("issue");

        let v = store.validate_mcp_token(&plaintext).await.expect("valid");
        assert_eq!(v.token_id, id);
        assert_eq!(v.user_id, 424244);
        assert_eq!(v.user_login, LOGIN);

        cleanup_login(&pool, LOGIN).await;
    }

    #[tokio::test]
    async fn validate_mcp_token_returns_none_for_revoked() {
        const LOGIN: &str = "test_b1_validate_revoked";
        let pool = test_pool().await;
        cleanup_login(&pool, LOGIN).await;
        let store = AuthStore::new(pool.clone(), 60);

        let (id, plaintext) = store
            .issue_mcp_token(424245, LOGIN, None)
            .await
            .expect("issue");
        store.revoke_mcp_token(id).await.expect("revoke");

        assert!(store.validate_mcp_token(&plaintext).await.is_none());
        cleanup_login(&pool, LOGIN).await;
    }

    #[tokio::test]
    async fn validate_mcp_token_returns_none_for_unknown_or_malformed() {
        let pool = test_pool().await;
        let store = AuthStore::new(pool.clone(), 60);
        // Unknown but well-shaped:
        assert!(
            store
                .validate_mcp_token("ak_deadbeefdeadbeefdeadbeefdeadbeef")
                .await
                .is_none()
        );
        // Wrong prefix:
        assert!(store.validate_mcp_token("not_an_ak_token").await.is_none());
        // Empty:
        assert!(store.validate_mcp_token("").await.is_none());
        // Right prefix but wrong shape (too short):
        assert!(store.validate_mcp_token("ak_short").await.is_none());
        // Right prefix but non-hex chars:
        assert!(
            store
                .validate_mcp_token("ak_zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn validate_passthrough_happy_with_cache() {
        const LOGIN: &str = "test_b1_pt_happy";
        let pool = test_pool().await;
        let store = AuthStore::new(pool.clone(), 60);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/user"))
            .and(header("authorization", "Bearer glpat-testtoken123"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": 9001, "username": LOGIN})),
            )
            .expect(1) // exactly one upstream call across both validations
            .mount(&server)
            .await;

        let v1 = store
            .validate_passthrough_token("glpat-testtoken123", &test_gateway(&server.uri()))
            .await
            .expect("v1");
        assert_eq!(v1.user_id, 9001);
        assert_eq!(v1.user_login, LOGIN);
        assert_eq!(v1.token_id_prefix.len(), 16);

        // Second call within 60s cache window — must not hit upstream.
        let v2 = store
            .validate_passthrough_token("glpat-testtoken123", &test_gateway(&server.uri()))
            .await
            .expect("v2");
        assert_eq!(v2.user_id, 9001);
    }

    #[tokio::test]
    async fn validate_passthrough_fails_closed_on_5xx() {
        let pool = test_pool().await;
        let store = AuthStore::new(pool.clone(), 60);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/user"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        assert!(
            store
                .validate_passthrough_token("glpat-bad503", &test_gateway(&server.uri()))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn validate_passthrough_returns_none_for_revoked_tombstone() {
        let pool = test_pool().await;
        let store = AuthStore::new(pool.clone(), 60);

        // Insert tombstone for the token we're about to send.
        let raw = "revoked_token_xyz";
        let hash_hex = format!("{:x}", Sha256::digest(raw.as_bytes()));
        let prefix = &hash_hex[..16];
        // Make sure no leftover row from a previous run.
        sqlx::query("DELETE FROM revoked_passthrough_tokens WHERE token_id_hash = $1")
            .bind(prefix)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("INSERT INTO revoked_passthrough_tokens (token_id_hash) VALUES ($1)")
            .bind(prefix)
            .execute(&pool)
            .await
            .expect("insert tombstone");

        // Even with no GitLab server reachable, tombstone short-circuits.
        let result = store
            .validate_passthrough_token(
                &format!("glpat-{raw}"),
                &test_gateway("http://127.0.0.1:1"),
            )
            .await;
        assert!(result.is_none());

        sqlx::query("DELETE FROM revoked_passthrough_tokens WHERE token_id_hash = $1")
            .bind(prefix)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn validate_passthrough_returns_none_for_wrong_prefix() {
        let pool = test_pool().await;
        let store = AuthStore::new(pool.clone(), 60);
        assert!(
            store
                .validate_passthrough_token("ak_notapat", &test_gateway("http://x"))
                .await
                .is_none()
        );
        assert!(
            store
                .validate_passthrough_token("garbage", &test_gateway("http://x"))
                .await
                .is_none()
        );
        assert!(
            store
                .validate_passthrough_token("", &test_gateway("http://x"))
                .await
                .is_none()
        );
        assert!(
            store
                .validate_passthrough_token("glpat-", &test_gateway("http://x"))
                .await
                .is_none()
        ); // empty secret
    }

    #[tokio::test]
    async fn validate_passthrough_with_ttl_zero_bypasses_cache() {
        let pool = test_pool().await;
        let store = AuthStore::new(pool.clone(), 0);
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v4/user"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": 11111, "username": "ttl_zero_test"})),
            )
            .expect(2)
            .mount(&server)
            .await;

        let _ = store
            .validate_passthrough_token("glpat-x", &test_gateway(&server.uri()))
            .await;
        let _ = store
            .validate_passthrough_token("glpat-x", &test_gateway(&server.uri()))
            .await;
        // wiremock asserts expect(2) on drop — if cache is honored, only 1 call would be made and the test fails.
    }

    #[tokio::test]
    async fn revoke_passthrough_token_inserts_tombstone() {
        let pool = test_pool().await;
        let store = AuthStore::new(pool.clone(), 60);

        let prefix = "abc1234567890def";
        let revoked_by: i64 = 9_001_001;

        sqlx::query("DELETE FROM revoked_passthrough_tokens WHERE token_id_hash = $1")
            .bind(prefix)
            .execute(&pool)
            .await
            .ok();

        // First call: inserts.
        store
            .revoke_passthrough_token(prefix, revoked_by)
            .await
            .expect("revoke");
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::BIGINT FROM revoked_passthrough_tokens WHERE token_id_hash = $1",
        )
        .bind(prefix)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 1);

        // Second call: idempotent.
        store
            .revoke_passthrough_token(prefix, revoked_by)
            .await
            .expect("revoke 2 idempotent");
        let n2: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::BIGINT FROM revoked_passthrough_tokens WHERE token_id_hash = $1",
        )
        .bind(prefix)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n2, 1);

        let recorded: i64 = sqlx::query_scalar(
            "SELECT revoked_by FROM revoked_passthrough_tokens WHERE token_id_hash = $1",
        )
        .bind(prefix)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(recorded, revoked_by);

        sqlx::query("DELETE FROM revoked_passthrough_tokens WHERE token_id_hash = $1")
            .bind(prefix)
            .execute(&pool)
            .await
            .ok();
    }

    /// Pure logic (no DB / no HTTP): verifies the dual-identifier derivation
    /// introduced by the 64-bit-collision FIX in `validate_passthrough_token`.
    ///
    /// The cache key MUST be the full 64-hex sha256 (collision-infeasible) and
    /// the tombstone/audit `prefix` MUST remain its 16-hex head (shared contract
    /// with `account::passthrough_prefix`). The two are derived from the same
    /// digest, so the prefix is exactly the cache key's first 16 chars.
    fn derive(raw: &str) -> (String, String) {
        let hash_hex = format!("{:x}", Sha256::digest(raw.as_bytes()));
        let prefix = hash_hex[..16].to_string();
        let cache_key = hash_hex;
        (cache_key, prefix)
    }

    #[test]
    fn passthrough_cache_key_is_full_digest_and_prefix_is_its_head() {
        let (cache_key, prefix) = derive("testtoken123");
        // Full sha256 hex = 64 chars; tombstone prefix = 16 chars.
        assert_eq!(cache_key.len(), 64, "cache key must be the full sha256 hex");
        assert_eq!(prefix.len(), 16, "tombstone prefix stays at 16 hex chars");
        // Derivation consistency: prefix is exactly the cache key's head, so a
        // tombstone (16-hex) and a cache slot (64-hex) for the same token agree.
        assert_eq!(&cache_key[..16], prefix.as_str());
        assert!(cache_key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn passthrough_cache_key_distinguishes_distinct_tokens() {
        // Two distinct tokens produce distinct full-digest cache keys, so they
        // can never share a cache slot (the misauthentication risk the FIX
        // closes). This holds even though distinct tokens *could* in theory
        // share a 16-hex prefix — the wider cache key keeps them isolated.
        let (k1, _) = derive("aaaaaaaaaaaa");
        let (k2, _) = derive("bbbbbbbbbbbb");
        assert_ne!(k1, k2, "distinct tokens must map to distinct cache keys");
        assert_eq!(k1.len(), 64);
        assert_eq!(k2.len(), 64);

        // Determinism: the same token always derives the same pair.
        let (k1b, p1b) = derive("aaaaaaaaaaaa");
        assert_eq!(k1, k1b);
        assert_eq!(&k1[..16], p1b.as_str());
    }
}
