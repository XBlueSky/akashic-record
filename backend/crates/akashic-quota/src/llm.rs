//! B3 quota-aware LLM provider decorator.
//!
//! Wraps an inner `Arc<dyn LlmProvider>`. On each `generate_json`:
//! 1. Reads `CURRENT_ACTOR` task local.
//! 2. If actor present and quota.check returns Exceeded, writes a
//!    `quota_exceeded` row to B2 audit_log and returns
//!    `Err(QuotaExceededError)`.
//! 3. Otherwise forwards to inner. On success, fire-and-forget
//!    `tokio::spawn` of `quota.consume`.
//!
//! When CURRENT_ACTOR is None (e.g., MCP-originated calls in v1 since
//! rmcp 0.1 cannot install the task local, or background ingestion),
//! the wrapper bypasses both check and consume.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::{
    ActorIdentity, ActorIdentityAsAuthenticated, CURRENT_ACTOR, Quota, QuotaCheck,
    QuotaExceededError, UsageKind,
};
use akashic_domain::ports::{LlmProvider, LlmResponse};
use akashic_kernel::{AuditPort, Authenticated};

pub struct QuotaLlm {
    pub inner: Arc<dyn LlmProvider>,
    pub quota: Arc<Quota>,
    pub audit: Arc<dyn AuditPort>,
}

#[async_trait]
impl LlmProvider for QuotaLlm {
    async fn generate_json(&self, prompt: &str) -> Result<LlmResponse> {
        let actor: Option<ActorIdentity> = CURRENT_ACTOR
            .try_with(std::clone::Clone::clone)
            .ok()
            .flatten();

        // Pre-check: refuse if at-or-above cap.
        if let Some(a) = &actor {
            match self.quota.check(a).await {
                QuotaCheck::Exceeded {
                    used,
                    cap,
                    window_secs,
                } => {
                    // C5: emit before the audit_log spawn so the metric is
                    // synchronous with the rejection.
                    metrics::counter!(
                        "akashic_quota_exceeded_total",
                        "bucket" => "llm",
                    )
                    .increment(1);
                    audit_quota_exceeded(
                        self.audit.clone(),
                        a.clone(),
                        UsageKind::Llm,
                        used,
                        cap,
                        window_secs,
                    );
                    return Err(anyhow::Error::new(QuotaExceededError {
                        used,
                        cap,
                        window_secs,
                    }));
                }
                QuotaCheck::Ok => {}
            }
        }

        let resp = self.inner.generate_json(prompt).await?;

        // C5: emit token usage to Prometheus alongside the SQL row.
        let provider_label = self.inner.provider_label();
        metrics::counter!(
            "akashic_llm_tokens_total",
            "provider" => provider_label,
            "kind" => "input",
        )
        .increment(resp.usage.input_tokens as u64);
        metrics::counter!(
            "akashic_llm_tokens_total",
            "provider" => provider_label,
            "kind" => "output",
        )
        .increment(resp.usage.output_tokens as u64);

        // Fire-and-forget consume.
        if let Some(a) = actor {
            let q = self.quota.clone();
            let total = resp.usage.total();
            let model = resp.model.clone();
            tokio::spawn(async move {
                q.consume(&a, UsageKind::Llm, total, &model).await;
            });
        }

        Ok(resp)
    }
}

/// Spawn a fire-and-forget B2 audit_log write recording the quota-exceeded
/// event. Public so `embedding::QuotaEmbedding` can reuse the helper.
///
/// Takes an `Arc<dyn AuditPort>` rather than `Arc<Quota>` so the caller
/// (and this module) need not import from `mcp` directly — the port is
/// injected at the composition root via `McpAudit`.
pub fn audit_quota_exceeded(
    audit: Arc<dyn AuditPort>,
    actor: ActorIdentity,
    kind: UsageKind,
    used: u32,
    cap: u32,
    window_secs: i64,
) {
    let action = format!("quota_exceeded:{}", kind.as_str());
    let summary = format!("used={used} cap={cap} window_secs={window_secs}");
    let auth: Arc<dyn Authenticated> = Arc::new(ActorIdentityAsAuthenticated::new(actor));
    let body = summary.into_bytes();
    let args = serde_json::json!({
        "used": used,
        "cap": cap,
        "window_secs": window_secs,
        "kind": kind.as_str(),
    });
    // Move `audit` into the async block so the spawned future owns the Arc
    // and satisfies `'static`. The write was denied by the quota gate, so
    // `success = false`.
    tokio::spawn(async move {
        audit
            .record_write(auth, action, args, body, false, None)
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActorIdentity;
    use akashic_domain::ports::LlmUsage;
    use akashic_kernel::AuthMethod;
    use akashic_kernel::audit::NoopAudit;
    use akashic_store_pg::repos::PgQuotaRepo;
    use std::net::IpAddr;
    use std::sync::Mutex;

    /// Captures the last `record_write` so the quota-exceeded audit test can
    /// assert the action + success flag without depending on `akashic-mcp`.
    #[derive(Default)]
    struct RecordingAudit {
        last: Mutex<Option<(String, bool)>>,
    }
    #[async_trait]
    impl AuditPort for RecordingAudit {
        async fn record_write(
            &self,
            _auth: Arc<dyn Authenticated>,
            tool_name: String,
            _args: serde_json::Value,
            _response_body: Vec<u8>,
            success: bool,
            _peer_ip: Option<IpAddr>,
        ) {
            *self.last.lock().unwrap() = Some((tool_name, success));
        }
    }

    struct StubLlm {
        text: String,
        usage: LlmUsage,
        model: String,
    }

    #[async_trait]
    impl LlmProvider for StubLlm {
        async fn generate_json(&self, _prompt: &str) -> Result<LlmResponse> {
            Ok(LlmResponse {
                text: self.text.clone(),
                usage: LlmUsage {
                    input_tokens: self.usage.input_tokens,
                    output_tokens: self.usage.output_tokens,
                },
                model: self.model.clone(),
            })
        }
    }

    async fn test_pool() -> sqlx::PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            format!(
                "postgres://{}:{}@localhost:5433/akashic",
                "akashic", "akashic_secret"
            )
        });
        let pool = sqlx::PgPool::connect(&url).await.unwrap();
        akashic_store_pg::init_auth_schema(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn quota_llm_bypasses_when_no_actor() {
        let pool = test_pool().await;
        let inner: Arc<dyn LlmProvider> = Arc::new(StubLlm {
            text: "ok".into(),
            usage: LlmUsage {
                input_tokens: 10,
                output_tokens: 20,
            },
            model: "stub".into(),
        });
        let quota = Arc::new(Quota::new(
            Arc::new(PgQuotaRepo::new(pool)),
            100,
            3600,
            true,
        ));
        let wrapped = QuotaLlm {
            inner,
            quota,
            audit: Arc::new(NoopAudit),
        };

        // No CURRENT_ACTOR scope — wrapper bypasses.
        let r = wrapped.generate_json("p").await.unwrap();
        assert_eq!(r.text, "ok");
    }

    #[tokio::test]
    async fn quota_llm_returns_quota_exceeded_when_over_cap() {
        let pool = test_pool().await;

        let user = 2_000_001;
        sqlx::query("DELETE FROM llm_usage WHERE actor_user_id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM audit_log WHERE actor_user_id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .ok();
        sqlx::query(
            "INSERT INTO llm_usage (actor_user_id, actor_token_id, kind, tokens_used, model) \
             VALUES ($1, 'mcp_token:test', 'llm', 200, 'gpt-4')",
        )
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();

        let inner: Arc<dyn LlmProvider> = Arc::new(StubLlm {
            text: "ok".into(),
            usage: LlmUsage {
                input_tokens: 1,
                output_tokens: 1,
            },
            model: "stub".into(),
        });
        let quota = Arc::new(Quota::new(
            Arc::new(PgQuotaRepo::new(pool.clone())),
            100,
            3600,
            true,
        ));
        let wrapped = QuotaLlm {
            inner,
            quota,
            audit: Arc::new(NoopAudit),
        };

        let actor = ActorIdentity {
            user_id: user,
            token_id: "mcp_token:test".into(),
            auth_method: AuthMethod::DeviceFlow,
        };
        let result = CURRENT_ACTOR
            .scope(Some(actor), wrapped.generate_json("p"))
            .await;
        let err = result.unwrap_err();
        assert!(
            err.downcast_ref::<QuotaExceededError>().is_some(),
            "got: {err}"
        );

        sqlx::query("DELETE FROM llm_usage WHERE actor_user_id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM audit_log WHERE actor_user_id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn quota_llm_appends_usage_row_on_success() {
        let pool = test_pool().await;

        let user = 2_000_002;
        sqlx::query("DELETE FROM llm_usage WHERE actor_user_id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .ok();

        let inner: Arc<dyn LlmProvider> = Arc::new(StubLlm {
            text: "ok".into(),
            usage: LlmUsage {
                input_tokens: 30,
                output_tokens: 70,
            },
            model: "stub-gpt".into(),
        });
        let quota = Arc::new(Quota::new(
            Arc::new(PgQuotaRepo::new(pool.clone())),
            1_000_000,
            3600,
            true,
        ));
        let wrapped = QuotaLlm {
            inner,
            quota,
            audit: Arc::new(NoopAudit),
        };

        let actor = ActorIdentity {
            user_id: user,
            token_id: "mcp_token:test".into(),
            auth_method: AuthMethod::DeviceFlow,
        };
        let _ = CURRENT_ACTOR
            .scope(Some(actor), wrapped.generate_json("p"))
            .await
            .unwrap();

        // Row insert is fire-and-forget; wait briefly.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let row: (i32, String) = sqlx::query_as(
            "SELECT tokens_used, model FROM llm_usage \
             WHERE actor_user_id = $1 ORDER BY ts DESC LIMIT 1",
        )
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, (100, "stub-gpt".to_string()));

        sqlx::query("DELETE FROM llm_usage WHERE actor_user_id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn quota_exceeded_writes_audit_record() {
        let pool = test_pool().await;

        let user = 2_000_010_i64;
        sqlx::query("DELETE FROM llm_usage WHERE actor_user_id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .ok();
        sqlx::query(
            "INSERT INTO llm_usage (actor_user_id, actor_token_id, kind, tokens_used, model) \
             VALUES ($1, 'mcp_token:t', 'llm', 200, 'gpt-4')",
        )
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();

        let inner: Arc<dyn LlmProvider> = Arc::new(StubLlm {
            text: "ok".into(),
            usage: LlmUsage {
                input_tokens: 1,
                output_tokens: 1,
            },
            model: "stub".into(),
        });
        let audit = Arc::new(RecordingAudit::default());
        let quota = Arc::new(Quota::new(
            Arc::new(PgQuotaRepo::new(pool.clone())),
            100,
            3600,
            true,
        ));
        let wrapped = QuotaLlm {
            inner,
            quota,
            audit: audit.clone(),
        };

        let actor = ActorIdentity {
            user_id: user,
            token_id: "mcp_token:t".into(),
            auth_method: AuthMethod::DeviceFlow,
        };
        let _ = CURRENT_ACTOR
            .scope(Some(actor), wrapped.generate_json("p"))
            .await;

        // audit write is fire-and-forget; wait then assert the captured call.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let last = audit.last.lock().unwrap().clone();
        assert_eq!(last, Some(("quota_exceeded:llm".to_string(), false)));

        sqlx::query("DELETE FROM llm_usage WHERE actor_user_id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .ok();
    }
}
