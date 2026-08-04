//! B3 LLM/embedding quota: per-actor token cap on a rolling window.
//!
//! The `Quota` engine holds an injected `Arc<dyn QuotaRepo>`. The provider
//! decorators (`embedding::QuotaEmbedding`, `llm::QuotaLlm`) read the actor from
//! the `CURRENT_ACTOR` task-local set by REST `require_auth`. MCP-originated
//! calls do not set the task-local (rmcp 0.1 cannot mount middleware), so they
//! bypass quota.

use std::sync::Arc;

use akashic_domain::ports::QuotaRepo;
use akashic_kernel::AuthMethod;

pub mod embedding;
pub mod llm;
pub use embedding::QuotaEmbedding;
pub use llm::{QuotaLlm, audit_quota_exceeded};

tokio::task_local! {
    /// Per-task actor identity for quota attribution. Set by REST
    /// `require_auth`; readers in provider decorators use
    /// `CURRENT_ACTOR.try_with(|a| a.clone())` and treat absence as
    /// "anonymous → bypass quota".
    pub static CURRENT_ACTOR: Option<ActorIdentity>;
}

/// Reserved user_id for the synthetic system ingestion actor.
/// Real GitLab user ids are positive integers, so -1 is guaranteed
/// not to collide. Recognizable in `llm_usage.actor_user_id`.
pub const SYSTEM_INGESTION_USER_ID: i64 = -1;

/// Build the synthetic [`ActorIdentity`] used to attribute ingestion
/// embedding spend. Scope this around the ingestion run body so every
/// `ingest_embedder.embed(...)` call records to `llm_usage` with
/// `actor_user_id = -1` (visibility) and, when `ingest_quota_enabled`,
/// enforces the ingestion cap.
pub fn system_ingestion_actor() -> ActorIdentity {
    ActorIdentity {
        user_id: SYSTEM_INGESTION_USER_ID,
        token_id: "system:ingestion".to_string(),
        // DeviceFlow is the closest existing variant; the auth_method
        // column is informational here (the actor has no real credential).
        auth_method: AuthMethod::DeviceFlow,
    }
}

#[derive(Clone, Debug)]
pub struct ActorIdentity {
    pub user_id: i64,
    pub token_id: String,
    pub auth_method: AuthMethod,
}

// Re-export domain types so the decorators + `akashic_quota` consumers can name
// them via the crate root.
pub use akashic_domain::types::QuotaCheck;
pub use akashic_domain::types::UsageKind;

/// Returned (via `anyhow::Error::new(...)`) from provider decorators when
/// the rolling-window sum is at or above the cap. REST handlers downcast
/// to map to HTTP 429.
#[derive(Debug, thiserror::Error)]
#[error("quota exceeded: used {used}/{cap} tokens in last {window_secs}s")]
pub struct QuotaExceededError {
    pub used: u32,
    pub cap: u32,
    pub window_secs: i64,
}

/// Adapter so `ActorIdentity` can be passed to B2's
/// `audit::record_write(auth: Arc<dyn Authenticated>, ...)` when writing
/// the `quota_exceeded` audit row.
pub struct ActorIdentityAsAuthenticated {
    inner: ActorIdentity,
    cached_actor_id: String,
}

impl ActorIdentityAsAuthenticated {
    pub fn new(inner: ActorIdentity) -> Self {
        let cached_actor_id = format!("gitlab:user:{}", inner.user_id);
        Self {
            inner,
            cached_actor_id,
        }
    }
}

impl std::fmt::Debug for ActorIdentityAsAuthenticated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActorIdentityAsAuthenticated")
            .field("inner", &self.inner)
            .finish()
    }
}

impl akashic_kernel::Authenticated for ActorIdentityAsAuthenticated {
    fn actor_id(&self) -> &str {
        &self.cached_actor_id
    }
    fn actor_token_id(&self) -> &str {
        &self.inner.token_id
    }
    fn auth_method(&self) -> AuthMethod {
        self.inner.auth_method
    }
}

#[allow(dead_code)]
pub struct Quota {
    repo: Arc<dyn QuotaRepo>,
    pub cap_per_window: u32,
    pub window_secs: i64,
    pub enabled: bool,
}

impl Quota {
    /// Construct over an injected `QuotaRepo` (the composition root builds the
    /// concrete `PgQuotaRepo`). This is the "QuotaPort" inversion — the engine
    /// no longer reaches into `akashic-store-pg`.
    pub fn new(
        repo: Arc<dyn QuotaRepo>,
        cap_per_window: u32,
        window_secs: i64,
        enabled: bool,
    ) -> Self {
        Self {
            repo,
            cap_per_window,
            window_secs,
            enabled,
        }
    }

    /// Sum tokens used in the rolling window for this actor and compare
    /// against the cap. Returns `QuotaCheck::Ok` if `enabled = false` or
    /// if the sum is strictly less than the cap. On DB error logs via
    /// `tracing::error!` and returns `Ok` (fail-open) — see spec §1
    /// "logging" rationale.
    pub async fn check(&self, actor: &ActorIdentity) -> QuotaCheck {
        if !self.enabled {
            return QuotaCheck::Ok;
        }
        self.repo
            .check(actor.user_id, self.cap_per_window, self.window_secs)
            .await
    }

    /// Append a usage row. Discards errors via `tracing::error!` and never
    /// panics. Caller may wrap in `tokio::spawn` for fire-and-forget.
    pub async fn consume(
        &self,
        actor: &ActorIdentity,
        kind: UsageKind,
        tokens_used: u32,
        model: &str,
    ) {
        self.repo
            .consume(actor.user_id, &actor.token_id, kind, tokens_used, model)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use akashic_store_pg::repos::PgQuotaRepo;
    use sqlx::PgPool;
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;

    async fn test_pool() -> PgPool {
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
        akashic_store_pg::init_auth_schema(&pool)
            .await
            .expect("init_auth_schema");
        pool
    }

    async fn cleanup_user(pool: &PgPool, user_id: i64) {
        sqlx::query("DELETE FROM llm_usage WHERE actor_user_id = $1")
            .bind(user_id)
            .execute(pool)
            .await
            .ok();
    }

    fn make_actor(user_id: i64) -> ActorIdentity {
        ActorIdentity {
            user_id,
            token_id: format!("mcp_token:test-{user_id}"),
            auth_method: AuthMethod::DeviceFlow,
        }
    }

    #[tokio::test]
    async fn check_returns_ok_for_user_under_cap() {
        let pool = test_pool().await;
        let user = 1_000_001;
        cleanup_user(&pool, user).await;

        let q = Quota::new(
            Arc::new(PgQuotaRepo::new(pool.clone())),
            100_000,
            3600,
            true,
        );
        let actor = make_actor(user);
        assert_eq!(q.check(&actor).await, QuotaCheck::Ok);

        cleanup_user(&pool, user).await;
    }

    #[tokio::test]
    async fn check_returns_exceeded_when_window_sum_at_or_above_cap() {
        let pool = test_pool().await;
        let user = 1_000_002;
        cleanup_user(&pool, user).await;

        sqlx::query(
            "INSERT INTO llm_usage (actor_user_id, actor_token_id, kind, tokens_used, model) \
             VALUES ($1, 'mcp_token:test', 'llm', 100000, 'gpt-4')",
        )
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();

        let q = Quota::new(
            Arc::new(PgQuotaRepo::new(pool.clone())),
            100_000,
            3600,
            true,
        );
        let actor = make_actor(user);
        match q.check(&actor).await {
            QuotaCheck::Exceeded {
                used,
                cap,
                window_secs,
            } => {
                assert_eq!(used, 100_000);
                assert_eq!(cap, 100_000);
                assert_eq!(window_secs, 3600);
            }
            QuotaCheck::Ok => panic!("expected Exceeded"),
        }

        cleanup_user(&pool, user).await;
    }

    #[tokio::test]
    async fn check_with_enabled_false_always_returns_ok() {
        let pool = test_pool().await;
        let user = 1_000_003;
        cleanup_user(&pool, user).await;

        sqlx::query(
            "INSERT INTO llm_usage (actor_user_id, actor_token_id, kind, tokens_used, model) \
             VALUES ($1, 'mcp_token:test', 'llm', 1000000, 'gpt-4')",
        )
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();

        let q = Quota::new(
            Arc::new(PgQuotaRepo::new(pool.clone())),
            100_000,
            3600,
            false,
        );
        let actor = make_actor(user);
        assert_eq!(q.check(&actor).await, QuotaCheck::Ok);

        cleanup_user(&pool, user).await;
    }

    #[tokio::test]
    async fn enabled_false_records_usage_but_skips_check() {
        let pool = test_pool().await;
        let user = 1_000_007_i64;
        cleanup_user(&pool, user).await;

        // Pre-seed past-cap usage.
        sqlx::query(
            "INSERT INTO llm_usage (actor_user_id, actor_token_id, kind, tokens_used, model) \
             VALUES ($1, 'mcp_token:t', 'llm', 1000000, 'gpt-4')",
        )
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();

        let q = Quota::new(
            Arc::new(PgQuotaRepo::new(pool.clone())),
            100_000,
            3600,
            false,
        ); // enabled=false
        let actor = make_actor(user);

        // Check: returns Ok despite cap.
        assert_eq!(q.check(&actor).await, QuotaCheck::Ok);

        // Consume: still appends (regardless of enabled flag).
        q.consume(&actor, UsageKind::Llm, 50, "gpt-4").await;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::BIGINT FROM llm_usage WHERE actor_user_id = $1 AND tokens_used = 50",
        )
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);

        cleanup_user(&pool, user).await;
    }

    #[test]
    fn system_ingestion_actor_has_expected_fields() {
        let actor = super::system_ingestion_actor();
        assert_eq!(
            actor.user_id,
            super::SYSTEM_INGESTION_USER_ID,
            "user_id must equal SYSTEM_INGESTION_USER_ID (-1)"
        );
        assert_eq!(
            actor.token_id, "system:ingestion",
            "token_id must be 'system:ingestion'"
        );
    }

    #[test]
    fn actor_identity_adapter_pre_formats_id() {
        let identity = ActorIdentity {
            user_id: 12345,
            token_id: "mcp_token:abc".into(),
            auth_method: AuthMethod::DeviceFlow,
        };
        let auth = ActorIdentityAsAuthenticated::new(identity);
        use akashic_kernel::Authenticated;
        assert_eq!(auth.actor_id(), "gitlab:user:12345");
        assert_eq!(auth.actor_token_id(), "mcp_token:abc");
        assert_eq!(auth.auth_method(), AuthMethod::DeviceFlow);
    }

    #[tokio::test]
    async fn consume_appends_row_with_correct_fields() {
        let pool = test_pool().await;
        let user = 1_000_004;
        cleanup_user(&pool, user).await;

        let q = Quota::new(
            Arc::new(PgQuotaRepo::new(pool.clone())),
            100_000,
            3600,
            true,
        );
        let actor = make_actor(user);
        q.consume(&actor, UsageKind::Llm, 1234, "gpt-4o-mini").await;

        let row: (String, i32, String) = sqlx::query_as(
            "SELECT kind, tokens_used, model FROM llm_usage \
             WHERE actor_user_id = $1 ORDER BY ts DESC LIMIT 1",
        )
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, ("llm".to_string(), 1234, "gpt-4o-mini".to_string()));

        cleanup_user(&pool, user).await;
    }

    #[tokio::test]
    async fn consume_does_not_panic_on_db_error() {
        let pool = test_pool().await;
        pool.close().await;

        let q = Quota::new(Arc::new(PgQuotaRepo::new(pool)), 100_000, 3600, true);
        let actor = make_actor(1_000_005);
        // Should not panic; should not return Err (function returns ()).
        q.consume(&actor, UsageKind::Embedding, 100, "text-embedding-3-large")
            .await;
    }
}
