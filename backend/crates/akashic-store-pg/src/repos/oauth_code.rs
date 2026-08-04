//! PostgreSQL adapter for `OauthCodeRepo` — MCP OAuth authorization-code
//! persistence (RFC 6749 §4.1, Task 4 — "MCP OAuth (二)").
//!
//! Mirrors [`super::identity::PgMcpTokenRepo`]'s hashing discipline: only
//! `sha256(code)` is ever persisted, the plaintext is returned once by
//! `issue_code` and never stored. `consume_code`'s claim is a single atomic
//! `UPDATE ... WHERE used_at IS NULL AND expires_at > now() RETURNING ...` so
//! two concurrent redemption attempts against the same code can never both
//! observe a hit — exactly one caller gets the row, everyone else gets
//! `None` (the CAS itself, not a separate SELECT-then-UPDATE, closes the race).

use anyhow::{Context, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::OauthCodeRepo;
use akashic_domain::types::ConsumedOauthCode;

/// PostgreSQL adapter implementing [`OauthCodeRepo`].
#[derive(Clone)]
pub struct PgOauthCodeRepo {
    pool: PgPool,
}

impl PgOauthCodeRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl OauthCodeRepo for PgOauthCodeRepo {
    async fn issue_code(
        &self,
        client_id: Uuid,
        user_id: i64,
        user_login: &str,
        code_challenge: &str,
        redirect_uri: &str,
    ) -> Result<String> {
        // 32 random bytes via two concatenated UUIDv4s: akashic-store-pg has
        // no direct `rand` dependency, but `uuid` already is one. Two
        // independent UUIDv4s give well over 200 bits of randomness — ample
        // for a code with a fixed 10-minute TTL, the same security margin
        // `PgMcpTokenRepo::issue_mcp_token` already accepts for a 90-day
        // token minted from a single UUIDv4.
        let mut secret = [0u8; 32];
        secret[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        secret[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        let plaintext: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        let hash = Sha256::digest(plaintext.as_bytes());

        sqlx::query(
            "INSERT INTO mcp_oauth_codes \
                (code_hash, client_id, user_id, user_login, code_challenge, redirect_uri, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, now() + INTERVAL '10 minutes')",
        )
        .bind(hash.as_slice())
        .bind(client_id)
        .bind(user_id)
        .bind(user_login)
        .bind(code_challenge)
        .bind(redirect_uri)
        .execute(&self.pool)
        .await
        .context("Failed to insert mcp_oauth_codes row")?;

        Ok(plaintext)
    }

    async fn consume_code(&self, presented: &str) -> Result<Option<ConsumedOauthCode>> {
        let hash = Sha256::digest(presented.as_bytes());

        // Single atomic statement: the WHERE clause is the entire "is this
        // code still good" check, and the UPDATE is the claim — there is no
        // window between checking and claiming for a second caller to slip
        // through. `RETURNING` reads the row's bound data from the exact
        // same statement that claimed it.
        let row: Option<(Uuid, i64, String, String, String)> = sqlx::query_as(
            "UPDATE mcp_oauth_codes \
             SET used_at = now() \
             WHERE code_hash = $1 AND used_at IS NULL AND expires_at > now() \
             RETURNING client_id, user_id, user_login, code_challenge, redirect_uri",
        )
        .bind(hash.as_slice())
        .fetch_optional(&self.pool)
        .await
        .context("Failed to consume mcp_oauth_codes row")?;

        Ok(row.map(
            |(client_id, user_id, user_login, code_challenge, redirect_uri)| ConsumedOauthCode {
                client_id,
                user_id,
                user_login,
                code_challenge,
                redirect_uri,
            },
        ))
    }
}
