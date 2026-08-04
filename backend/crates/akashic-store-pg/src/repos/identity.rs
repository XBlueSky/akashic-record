//! PostgreSQL adapters for identity/auth port traits.
//!
//! SQL moved verbatim from `akashic-identity::store` and `akashic-identity::quota`.
//!
//! ## Trait coverage
//!
//! - `SessionTokenRepo` → [`PgSessionTokenRepo`] — SQL from `AuthStore::validate_session`,
//!   `insert_session`, `remove_session`, and the DB half of `cleanup_expired`.
//! - `McpTokenRepo` → [`PgMcpTokenRepo`] — SQL from `AuthStore::issue_mcp_token`,
//!   `validate_mcp_token`, and `revoke_mcp_token`.
//! - `PassthroughTokenRepo` → [`PgPassthroughTokenRepo`] — SQL from
//!   `AuthStore::revoke_passthrough_token` and the tombstone SELECT inside
//!   `validate_passthrough_token`.
//! - `QuotaRepo` → [`PgQuotaRepo`] — SQL from `Quota::check` and `Quota::consume`.
//!
//! ## AccountAuditRepo + DeviceFlowRepo
//!
//! `PgAccountAuditRepo` and `PgDeviceFlowRepo` adapters are added in A2.
//! SQL moved verbatim from `akashic-server::auth::account` and
//! `akashic-server::auth::oauth_device`.

use anyhow::Result;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::{
    AccountAuditRepo, DeviceFlowRepo, McpTokenRepo, PassthroughTokenRepo, QuotaRepo,
    SessionTokenRepo,
};
use akashic_domain::types::{
    AuditEntryRow, DevicePollResult, McpTokenSummaryRow, QuotaCheck, SessionRow, UsageKind,
    ValidatedMcpToken,
};

// ── PgSessionTokenRepo ────────────────────────────────────────────────────────

/// PostgreSQL adapter implementing [`SessionTokenRepo`].
#[derive(Clone)]
pub struct PgSessionTokenRepo {
    pool: PgPool,
}

impl PgSessionTokenRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SessionTokenRepo for PgSessionTokenRepo {
    async fn validate_session(&self, api_key: &str) -> Result<Option<SessionRow>> {
        let row: Option<(String, Option<String>, Option<String>, String, Option<i64>)> =
            sqlx::query_as(
                "SELECT username, name, avatar_url, gitlab_token, gitlab_user_id \
                 FROM sessions \
                 WHERE api_key = $1 AND (expires_at IS NULL OR expires_at > now())",
            )
            .bind(api_key)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(
            |(username, name, avatar_url, gitlab_token, gitlab_user_id)| SessionRow {
                username,
                name,
                avatar_url,
                gitlab_token,
                gitlab_user_id,
            },
        ))
    }

    async fn insert_session(
        &self,
        api_key: &str,
        username: &str,
        name: Option<&str>,
        avatar_url: Option<&str>,
        gitlab_token: &str,
        ttl_secs: u64,
        gitlab_user_id: Option<i64>,
    ) -> Result<()> {
        let expires_at = if ttl_secs == 0 {
            None
        } else {
            Some(chrono::Utc::now() + chrono::Duration::seconds(ttl_secs as i64))
        };

        sqlx::query(
            "INSERT INTO sessions \
                (api_key, username, name, avatar_url, gitlab_token, expires_at, gitlab_user_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (api_key) DO UPDATE SET \
               gitlab_token   = EXCLUDED.gitlab_token, \
               expires_at     = EXCLUDED.expires_at, \
               gitlab_user_id = EXCLUDED.gitlab_user_id",
        )
        .bind(api_key)
        .bind(username)
        .bind(name)
        .bind(avatar_url)
        .bind(gitlab_token)
        .bind(expires_at)
        .bind(gitlab_user_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn remove_session(&self, api_key: &str) -> Result<()> {
        let _ = sqlx::query("DELETE FROM sessions WHERE api_key = $1")
            .bind(api_key)
            .execute(&self.pool)
            .await;
        Ok(())
    }

    async fn cleanup_expired_sessions(&self) -> Result<()> {
        let _ =
            sqlx::query("DELETE FROM sessions WHERE expires_at IS NOT NULL AND expires_at < now()")
                .execute(&self.pool)
                .await;
        Ok(())
    }
}

// ── PgMcpTokenRepo ────────────────────────────────────────────────────────────

/// PostgreSQL adapter implementing [`McpTokenRepo`].
#[derive(Clone)]
pub struct PgMcpTokenRepo {
    pool: PgPool,
}

impl PgMcpTokenRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl McpTokenRepo for PgMcpTokenRepo {
    async fn issue_mcp_token(
        &self,
        user_id: i64,
        user_login: &str,
        label: Option<&str>,
    ) -> Result<(Uuid, String)> {
        // Token format: "ak_" + 32 hex chars (UUIDv4 with hyphens removed).
        // The hash stored in the DB is sha256 of the secret PORTION only — the
        // bytes after the "ak_" prefix. Task 4's validator must strip the prefix
        // before hashing, or the lookup will miss.
        let token_secret = uuid::Uuid::new_v4().to_string().replace('-', "");
        let plaintext = format!("ak_{token_secret}");
        let hash = Sha256::digest(token_secret.as_bytes());
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO mcp_tokens (token_hash, user_id, user_login, label, expires_at) \
             VALUES ($1, $2, $3, $4, now() + INTERVAL '90 days') \
             RETURNING id",
        )
        .bind(hash.as_slice())
        .bind(user_id)
        .bind(user_login)
        .bind(label)
        .fetch_one(&self.pool)
        .await?;
        Ok((id, plaintext))
    }

    async fn validate_mcp_token(&self, presented: &str) -> Result<Option<ValidatedMcpToken>> {
        let token_secret = match presented.strip_prefix("ak_") {
            Some(s) => s,
            None => return Ok(None),
        };
        if token_secret.len() != 32 || !token_secret.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(None);
        }
        let hash = Sha256::digest(token_secret.as_bytes());

        let row: Option<(Uuid, i64, String, Option<chrono::DateTime<chrono::Utc>>)> =
            sqlx::query_as(
                "SELECT id, user_id, user_login, last_used_at \
                 FROM mcp_tokens \
                 WHERE token_hash = $1 \
                   AND revoked_at IS NULL \
                   AND expires_at > now()",
            )
            .bind(hash.as_slice())
            .fetch_optional(&self.pool)
            .await?;

        let (id, user_id, user_login, last_used) = match row {
            Some(r) => r,
            None => return Ok(None),
        };

        // 60 s debounce on the sliding-TTL update (spec R-11).
        let should_update =
            last_used.is_none_or(|t| chrono::Utc::now() - t > chrono::Duration::seconds(60));
        if should_update {
            let pool = self.pool.clone();
            tokio::spawn(async move {
                let _ = sqlx::query(
                    "UPDATE mcp_tokens \
                     SET last_used_at = now(), \
                         expires_at  = now() + INTERVAL '90 days' \
                     WHERE id = $1",
                )
                .bind(id)
                .execute(&pool)
                .await;
            });
        }

        Ok(Some(ValidatedMcpToken {
            token_id: id,
            user_id,
            user_login,
        }))
    }

    async fn revoke_mcp_token(&self, token_id: Uuid) -> Result<()> {
        // WHERE revoked_at IS NULL avoids writing to already-revoked rows
        // (no row-version churn, no lock acquired). 0 rows affected = idempotent
        // success: token doesn't exist OR was already revoked.
        sqlx::query(
            "UPDATE mcp_tokens SET revoked_at = now() \
             WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(token_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

// ── PgPassthroughTokenRepo ────────────────────────────────────────────────────

/// PostgreSQL adapter implementing [`PassthroughTokenRepo`].
///
/// Only the two DB operations are here.  The full `validate_passthrough_token`
/// (GitLab HTTP probe + LRU cache) stays on `AuthStore`.
#[derive(Clone)]
pub struct PgPassthroughTokenRepo {
    pool: PgPool,
}

impl PgPassthroughTokenRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PassthroughTokenRepo for PgPassthroughTokenRepo {
    async fn revoke_passthrough_token(&self, prefix: &str, revoked_by: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO revoked_passthrough_tokens (token_id_hash, revoked_by) \
             VALUES ($1, $2) \
             ON CONFLICT (token_id_hash) DO NOTHING",
        )
        .bind(prefix)
        .bind(revoked_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn check_passthrough_revoked(&self, prefix: &str) -> Result<bool> {
        let tombstoned: Option<(String,)> = sqlx::query_as(
            "SELECT token_id_hash FROM revoked_passthrough_tokens WHERE token_id_hash = $1",
        )
        .bind(prefix)
        .fetch_optional(&self.pool)
        .await?;
        Ok(tombstoned.is_some())
    }
}

// ── PgQuotaRepo ───────────────────────────────────────────────────────────────

/// PostgreSQL adapter implementing [`QuotaRepo`].
///
/// Engine logic (`enabled` flag, `cap_per_window`, `window_secs`, fail-open
/// error handling, `CURRENT_ACTOR` task-local, decorator wrapping) remains in
/// `akashic-identity::quota::Quota`.  Only the two SQL operations are here.
#[derive(Clone)]
pub struct PgQuotaRepo {
    pool: PgPool,
}

impl PgQuotaRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl QuotaRepo for PgQuotaRepo {
    async fn check(&self, actor_user_id: i64, cap_per_window: u32, window_secs: i64) -> QuotaCheck {
        // Sum as BIGINT, not INT4: a heavy actor's windowed total can exceed
        // i32::MAX, and an `::INT4` cast would raise a Postgres overflow error
        // that the `Err` arm below silently turns into fail-open.
        let used: i64 = match sqlx::query_scalar(
            "SELECT COALESCE(SUM(tokens_used), 0)::BIGINT \
             FROM llm_usage \
             WHERE actor_user_id = $1 \
               AND ts > now() - ($2::INT8 * INTERVAL '1 second')",
        )
        .bind(actor_user_id)
        .bind(window_secs)
        .fetch_one(&self.pool)
        .await
        {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(event = "mcp_quota_check_failed", error = %e);
                return QuotaCheck::Ok;
            }
        };
        let used_u32 = used.clamp(0, u32::MAX as i64) as u32;
        if used_u32 >= cap_per_window {
            QuotaCheck::Exceeded {
                used: used_u32,
                cap: cap_per_window,
                window_secs,
            }
        } else {
            QuotaCheck::Ok
        }
    }

    async fn consume(
        &self,
        actor_user_id: i64,
        actor_token_id: &str,
        kind: UsageKind,
        tokens_used: u32,
        model: &str,
    ) {
        let result = sqlx::query(
            "INSERT INTO llm_usage \
                (actor_user_id, actor_token_id, kind, tokens_used, model) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(actor_user_id)
        .bind(actor_token_id)
        .bind(kind.as_str())
        // Clamp into the i32 column range so an implausibly large count can
        // never wrap to a negative value that would corrupt the window sum.
        .bind(tokens_used.min(i32::MAX as u32) as i32)
        .bind(model)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => {
                tracing::debug!(
                    event = "mcp_quota_consumed",
                    actor_user_id,
                    kind = kind.as_str(),
                    tokens_used,
                    model,
                );
            }
            Err(e) => {
                tracing::error!(
                    event = "mcp_quota_consume_failed",
                    error = %e,
                    actor_user_id,
                    kind = kind.as_str(),
                );
            }
        }
    }
}

// ── PgDeviceFlowRepo ──────────────────────────────────────────────────────────

/// PostgreSQL adapter implementing [`DeviceFlowRepo`].
///
/// `poll_device_token` opens its own transaction, executes `SELECT … FOR UPDATE`
/// and the `UPDATE` inside it, commits, then returns the pre-update state.
/// This guarantees the Tx is never split across two repo calls.
#[derive(Clone)]
pub struct PgDeviceFlowRepo {
    pool: PgPool,
}

impl PgDeviceFlowRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DeviceFlowRepo for PgDeviceFlowRepo {
    async fn insert_pending(
        &self,
        device_code: &str,
        user_code: &str,
        client_id: &str,
    ) -> Result<()> {
        // SQL moved verbatim from akashic-server::auth::oauth_device::device_authorization.
        sqlx::query(
            "INSERT INTO device_flow_pending \
                (device_code, user_code, client_id, expires_at, status) \
             VALUES ($1, $2, $3, now() + INTERVAL '600 seconds', 'pending')",
        )
        .bind(device_code)
        .bind(user_code)
        .bind(client_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn poll_device_token(&self, device_code: &str) -> Result<Option<DevicePollResult>> {
        // Tx-scoped: SELECT FOR UPDATE + UPDATE in one atomic block.
        // SQL moved verbatim from akashic-server::auth::oauth_device::device_token.
        let mut tx = self.pool.begin().await?;

        let row: Option<(
            String,                                // status
            chrono::DateTime<chrono::Utc>,         // expires_at
            Option<i64>,                           // granted_user_id
            Option<String>,                        // granted_user_login
            i32,                                   // interval_secs
            Option<chrono::DateTime<chrono::Utc>>, // last_polled_at
        )> = sqlx::query_as(
            "SELECT status, expires_at, granted_user_id, granted_user_login, \
                    interval_secs, last_polled_at \
             FROM device_flow_pending \
             WHERE device_code = $1 \
             FOR UPDATE",
        )
        .bind(device_code)
        .fetch_optional(&mut *tx)
        .await?;

        let Some((status, expires_at, granted_uid, granted_login, interval, prev_polled)) = row
        else {
            let _ = tx.rollback().await;
            return Ok(None);
        };

        let now = chrono::Utc::now();
        let too_fast =
            prev_polled.is_some_and(|t| now - t < chrono::Duration::seconds(interval as i64));

        let new_interval = if too_fast { interval + 5 } else { interval };
        let _ = sqlx::query(
            "UPDATE device_flow_pending \
             SET last_polled_at = now(), interval_secs = $2 \
             WHERE device_code = $1",
        )
        .bind(device_code)
        .bind(new_interval)
        .execute(&mut *tx)
        .await;

        if let Err(e) = tx.commit().await {
            tracing::warn!(event = "device_flow_poll_tx_commit_failed", error = %e);
            // Non-fatal: the SELECT data is still usable for the response.
        }

        Ok(Some(DevicePollResult {
            status,
            expires_at,
            granted_user_id: granted_uid,
            granted_user_login: granted_login,
            slow_down: too_fast,
        }))
    }

    async fn set_pre_approved(
        &self,
        user_code: &str,
        user_id: i64,
        user_login: &str,
    ) -> Result<bool> {
        // SQL moved verbatim from akashic-server::auth::oauth_device::device_verify.
        let r = sqlx::query(
            "UPDATE device_flow_pending \
             SET status = 'pre_approved', \
                 granted_user_id = $1, granted_user_login = $2 \
             WHERE user_code = $3 \
               AND status = 'pending' \
               AND expires_at > now()",
        )
        .bind(user_id)
        .bind(user_login)
        .bind(user_code)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    async fn set_approved(&self, user_code: &str, user_id: i64) -> Result<bool> {
        // SQL moved verbatim from akashic-server::auth::oauth_device::device_approve.
        let r = sqlx::query(
            "UPDATE device_flow_pending \
             SET status = 'approved' \
             WHERE user_code = $1 \
               AND status = 'pre_approved' \
               AND granted_user_id = $2 \
               AND expires_at > now()",
        )
        .bind(user_code)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    async fn mark_exchanged(&self, device_code: &str, token_id: Uuid) -> Result<bool> {
        // SQL moved verbatim from the approved-branch of device_token.
        // WHERE status = 'approved' is the exactly-once exchange guard.
        let r = sqlx::query(
            "UPDATE device_flow_pending \
             SET status = 'exchanged', granted_token_id = $1 \
             WHERE device_code = $2 AND status = 'approved'",
        )
        .bind(token_id)
        .bind(device_code)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }
}

// ── PgAccountAuditRepo ────────────────────────────────────────────────────────

/// PostgreSQL adapter implementing [`AccountAuditRepo`].
///
/// SQL moved verbatim from `akashic-server::auth::account` HTTP handlers.
#[derive(Clone)]
pub struct PgAccountAuditRepo {
    pool: PgPool,
}

impl PgAccountAuditRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AccountAuditRepo for PgAccountAuditRepo {
    async fn list_my_tokens(&self, user_id: i64) -> Result<Vec<McpTokenSummaryRow>> {
        // SQL moved verbatim from akashic-server::auth::account::list_my_tokens.
        let rows: Vec<(
            Uuid,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
            Option<chrono::DateTime<chrono::Utc>>,
            chrono::DateTime<chrono::Utc>,
            Option<chrono::DateTime<chrono::Utc>>,
        )> = sqlx::query_as(
            "SELECT id, label, issued_at, last_used_at, expires_at, revoked_at \
             FROM mcp_tokens \
             WHERE user_id = $1 \
             ORDER BY issued_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, label, issued_at, last_used_at, expires_at, revoked_at)| McpTokenSummaryRow {
                    id,
                    label,
                    issued_at,
                    last_used_at,
                    expires_at,
                    revoked_at,
                },
            )
            .collect())
    }

    async fn check_token_ownership(&self, token_id: Uuid) -> Result<Option<i64>> {
        // SQL moved verbatim from akashic-server::auth::account::revoke_my_token
        // (the ownership-check SELECT).
        let owner: Option<i64> = sqlx::query_scalar("SELECT user_id FROM mcp_tokens WHERE id = $1")
            .bind(token_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(owner)
    }

    async fn list_my_audit(&self, user_id: i64, limit: i64) -> Result<Vec<AuditEntryRow>> {
        // SQL moved verbatim from akashic-server::auth::account::list_my_audit.
        // Use host(ip) to convert INET → text for clean serialization, avoiding
        // the sqlx::types::ipnetwork dep gymnastics.
        let rows: Vec<(
            chrono::DateTime<chrono::Utc>,
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT ts, action, target_id, actor_token_id, host(ip) AS ip, response_summary \
             FROM audit_log \
             WHERE actor_user_id = $1 \
             ORDER BY ts DESC \
             LIMIT $2",
        )
        .bind(user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(ts, action, target_id, actor_token_id, ip, response_summary)| AuditEntryRow {
                    ts,
                    action,
                    target_id,
                    actor_token_id,
                    ip,
                    response_summary,
                },
            )
            .collect())
    }
}
