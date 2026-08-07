//! PostgreSQL adapter for `OauthConsentRepo` — MCP OAuth pending-consent
//! persistence (spec §4, MCP refactor 2026-08-07 — CIMD validation + consent
//! screen replace DCR).
//!
//! SQL pattern mirrors [`super::oauth_code::PgOauthCodeRepo`] exactly:
//! `issue_pending` inserts a row with a fixed 10-minute TTL and returns the
//! generated id; `redeem_pending`'s claim is a single atomic `UPDATE ... SET
//! used_at = now() WHERE ... AND used_at IS NULL AND expires_at > now()
//! RETURNING ...` so two concurrent redemption attempts against the same
//! `consent_id` can never both observe a hit. The extra `AND user_id = $2`
//! bind (absent from `consume_code`) is the CSRF defense described on
//! `OauthConsentRepo`'s trait doc comment — see that comment for the full
//! rationale.

use anyhow::{Context, Result};
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::OauthConsentRepo;
use akashic_domain::types::{PendingConsent, PendingConsentInput};

/// PostgreSQL adapter implementing [`OauthConsentRepo`].
#[derive(Clone)]
pub struct PgOauthConsentRepo {
    pool: PgPool,
}

impl PgOauthConsentRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl OauthConsentRepo for PgOauthConsentRepo {
    async fn issue_pending(
        &self,
        user_id: i64,
        user_login: &str,
        meta: &PendingConsentInput,
    ) -> Result<Uuid> {
        let consent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO mcp_oauth_pending_consents \
                (user_id, user_login, client_id, client_name, redirect_uri, \
                 oauth_state, code_challenge, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, now() + INTERVAL '10 minutes') \
             RETURNING consent_id",
        )
        .bind(user_id)
        .bind(user_login)
        .bind(&meta.client_id)
        .bind(&meta.client_name)
        .bind(&meta.redirect_uri)
        .bind(&meta.oauth_state)
        .bind(&meta.code_challenge)
        .fetch_one(&self.pool)
        .await
        .context("Failed to insert mcp_oauth_pending_consents row")?;

        Ok(consent_id)
    }

    async fn redeem_pending(
        &self,
        consent_id: Uuid,
        user_id: i64,
    ) -> Result<Option<PendingConsent>> {
        // Single atomic statement: the WHERE clause (id + owning user + not
        // yet used + not expired) is the entire validity check, and the
        // UPDATE is the claim — no window for a second caller (or a
        // different user's session) to slip through between checking and
        // claiming.
        let row: Option<(String, Option<String>, String, String, String, i64, String)> =
            sqlx::query_as(
                "UPDATE mcp_oauth_pending_consents \
                 SET used_at = now() \
                 WHERE consent_id = $1 AND user_id = $2 \
                   AND used_at IS NULL AND expires_at > now() \
                 RETURNING client_id, client_name, redirect_uri, oauth_state, code_challenge, \
                           user_id, user_login",
            )
            .bind(consent_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .context("Failed to redeem mcp_oauth_pending_consents row")?;

        Ok(row.map(
            |(
                client_id,
                client_name,
                redirect_uri,
                oauth_state,
                code_challenge,
                user_id,
                user_login,
            )| {
                PendingConsent {
                    client_id,
                    client_name,
                    redirect_uri,
                    oauth_state,
                    code_challenge,
                    user_id,
                    user_login,
                }
            },
        ))
    }
}
