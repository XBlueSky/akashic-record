//! PostgreSQL adapter for `PublishTokenRepo` — repo-scoped docs-publish
//! bearer tokens (`akp_<32hex>`), Task 6 (B1, docs corpus).
//!
//! Mirrors `PgMcpTokenRepo` (`repos/identity.rs`) exactly in hashing
//! discipline: the token secret is 32 hex chars; the DB stores only
//! `sha256(secret_bytes)`, never the plaintext. The prefix differs (`akp_`
//! vs. `ak_`) so the two token families are visually and structurally
//! distinct. A publish token is scoped to a `repo_name` (not a `user_id`).
//!
//! Publish tokens carry the same 90-day sliding TTL as MCP tokens: issued
//! with `expires_at = now() + 90 days`, and each successful validation
//! slides `expires_at` forward another 90 days (debounced to at most once
//! per 60 s of activity, same as `PgMcpTokenRepo::validate_mcp_token`). A
//! token unused for 90 days simply stops validating.

use anyhow::Result;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::PublishTokenRepo;
use akashic_domain::types::{PublishTokenSummaryRow, ValidatedPublishToken};

/// PostgreSQL adapter implementing [`PublishTokenRepo`].
#[derive(Clone)]
pub struct PgPublishTokenRepo {
    pool: PgPool,
}

impl PgPublishTokenRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PublishTokenRepo for PgPublishTokenRepo {
    async fn issue_publish_token(
        &self,
        repo_name: &str,
        created_by: &str,
    ) -> Result<(Uuid, String)> {
        // Token format: "akp_" + 32 hex chars (UUIDv4 with hyphens removed).
        // Mirrors PgMcpTokenRepo::issue_mcp_token — the hash stored is sha256
        // of the secret PORTION only (bytes after "akp_").
        let token_secret = Uuid::new_v4().to_string().replace('-', "");
        let plaintext = format!("akp_{token_secret}");
        let hash = Sha256::digest(token_secret.as_bytes());
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO publish_tokens (token_hash, repo_name, created_by, expires_at) \
             VALUES ($1, $2, $3, now() + INTERVAL '90 days') \
             RETURNING id",
        )
        .bind(hash.as_slice())
        .bind(repo_name)
        .bind(created_by)
        .fetch_one(&self.pool)
        .await?;
        Ok((id, plaintext))
    }

    async fn validate_publish_token(
        &self,
        presented: &str,
    ) -> Result<Option<ValidatedPublishToken>> {
        let token_secret = match presented.strip_prefix("akp_") {
            Some(s) => s,
            None => return Ok(None),
        };
        if token_secret.len() != 32 || !token_secret.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(None);
        }
        let hash = Sha256::digest(token_secret.as_bytes());

        // Every token now carries a concrete expires_at (90-day sliding TTL,
        // set on issue and slid forward on each validation below), so a NULL
        // expires_at row is treated as expired here rather than exempted —
        // no row can validate forever, bounded or not.
        let row: Option<(Uuid, String, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
            "SELECT id, repo_name, last_used_at \
             FROM publish_tokens \
             WHERE token_hash = $1 \
               AND revoked_at IS NULL \
               AND expires_at > now()",
        )
        .bind(hash.as_slice())
        .fetch_optional(&self.pool)
        .await?;

        let Some((id, repo_name, last_used)) = row else {
            return Ok(None);
        };

        // Best-effort last_used_at bump + 90-day sliding TTL, mirroring
        // PgMcpTokenRepo::validate_mcp_token exactly: same 60 s debounce so
        // high-frequency publish traffic doesn't saturate the pool with
        // UPDATEs, same slide-forward-on-use semantics (a token used within
        // any 90-day window keeps sliding; unused for >90 days, it expires).
        let should_update =
            last_used.is_none_or(|t| chrono::Utc::now() - t > chrono::Duration::seconds(60));
        if should_update {
            let pool = self.pool.clone();
            tokio::spawn(async move {
                let _ = sqlx::query(
                    "UPDATE publish_tokens \
                     SET last_used_at = now(), \
                         expires_at = now() + INTERVAL '90 days' \
                     WHERE id = $1",
                )
                .bind(id)
                .execute(&pool)
                .await;
            });
        }

        Ok(Some(ValidatedPublishToken {
            token_id: id,
            repo_name,
        }))
    }

    async fn revoke_publish_token(&self, token_id: Uuid) -> Result<()> {
        // WHERE revoked_at IS NULL avoids writing to already-revoked rows —
        // 0 rows affected = idempotent success (mirrors revoke_mcp_token).
        sqlx::query(
            "UPDATE publish_tokens SET revoked_at = now() \
             WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(token_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_publish_tokens(&self, created_by: &str) -> Result<Vec<PublishTokenSummaryRow>> {
        let rows: Vec<(
            Uuid,
            String,
            chrono::DateTime<chrono::Utc>,
            Option<chrono::DateTime<chrono::Utc>>,
            Option<chrono::DateTime<chrono::Utc>>,
            Option<chrono::DateTime<chrono::Utc>>,
        )> = sqlx::query_as(
            "SELECT id, repo_name, created_at, last_used_at, expires_at, revoked_at \
             FROM publish_tokens \
             WHERE created_by = $1 \
             ORDER BY created_at DESC",
        )
        .bind(created_by)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, repo_name, created_at, last_used_at, expires_at, revoked_at)| {
                    PublishTokenSummaryRow {
                        id,
                        repo_name,
                        created_at,
                        last_used_at,
                        expires_at,
                        revoked_at,
                    }
                },
            )
            .collect())
    }

    async fn check_publish_token_ownership(&self, token_id: Uuid) -> Result<Option<String>> {
        let owner: Option<String> =
            sqlx::query_scalar("SELECT created_by FROM publish_tokens WHERE id = $1")
                .bind(token_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(owner)
    }

    async fn record_publish_token_audit(
        &self,
        actor_user_id: i64,
        actor_token_id: &str,
        auth_method: &str,
        action: &str,
        target_id: &str,
    ) -> Result<()> {
        let after_hash = Sha256::digest(target_id.as_bytes());
        sqlx::query(
            "INSERT INTO audit_log \
                (actor_user_id, actor_token_id, auth_method, action, target_id, after_hash) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(actor_user_id)
        .bind(actor_token_id)
        .bind(auth_method)
        .bind(action)
        .bind(target_id)
        .bind(after_hash.as_slice())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
