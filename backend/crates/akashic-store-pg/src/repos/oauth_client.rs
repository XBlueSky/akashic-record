//! PostgreSQL adapter for `OauthClientRepo` — MCP OAuth dynamic client
//! registration (RFC 7591, Task 3 — "MCP OAuth (一)").
//!
//! Mirrors the shape of [`super::identity::PgMcpTokenRepo`]: a thin adapter
//! over a `PgPool`, no business logic beyond the SQL. Redirect-URI format
//! validation lives at the HTTP handler layer (`akashic-http::auth::mcp_oauth`),
//! not here — this repo persists whatever `redirect_uris` it is given.

use anyhow::{Context, Result};
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::OauthClientRepo;
use akashic_domain::types::ClientRegistration;

/// PostgreSQL adapter implementing [`OauthClientRepo`].
#[derive(Clone)]
pub struct PgOauthClientRepo {
    pool: PgPool,
}

impl PgOauthClientRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl OauthClientRepo for PgOauthClientRepo {
    async fn register_client(
        &self,
        redirect_uris: Vec<String>,
        client_name: Option<String>,
    ) -> Result<ClientRegistration> {
        // redirect_uris is persisted as JSONB (a plain string array) — mirrors
        // the `mcp_oauth_clients.redirect_uris JSONB NOT NULL` column.
        let uris_json = serde_json::to_value(&redirect_uris)
            .context("Failed to serialize redirect_uris to JSON")?;

        let client_id: Uuid = sqlx::query_scalar(
            "INSERT INTO mcp_oauth_clients (client_name, redirect_uris) \
             VALUES ($1, $2) \
             RETURNING client_id",
        )
        .bind(&client_name)
        .bind(&uris_json)
        .fetch_one(&self.pool)
        .await
        .context("Failed to insert mcp_oauth_clients row")?;

        Ok(ClientRegistration {
            client_id,
            client_name,
            redirect_uris,
        })
    }

    async fn get_client(&self, client_id: Uuid) -> Result<Option<ClientRegistration>> {
        let row: Option<(Option<String>, serde_json::Value)> = sqlx::query_as(
            "SELECT client_name, redirect_uris FROM mcp_oauth_clients WHERE client_id = $1",
        )
        .bind(client_id)
        .fetch_optional(&self.pool)
        .await
        .context("Failed to select mcp_oauth_clients row")?;

        let Some((client_name, uris_json)) = row else {
            return Ok(None);
        };
        let redirect_uris: Vec<String> = serde_json::from_value(uris_json)
            .context("Failed to deserialize redirect_uris from JSON")?;

        Ok(Some(ClientRegistration {
            client_id,
            client_name,
            redirect_uris,
        }))
    }
}
