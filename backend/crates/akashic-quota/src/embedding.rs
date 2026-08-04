//! B3 quota-aware embedding provider decorator. Same shape as
//! `crate::llm::QuotaLlm`. See that module's docstring for design.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use akashic_domain::ports::{EmbeddingProvider, EmbeddingResponse};

use crate::llm::audit_quota_exceeded;
use crate::{ActorIdentity, CURRENT_ACTOR, Quota, QuotaCheck, QuotaExceededError, UsageKind};
use akashic_kernel::AuditPort;

pub struct QuotaEmbedding {
    pub inner: Arc<dyn EmbeddingProvider>,
    pub quota: Arc<Quota>,
    pub audit: Arc<dyn AuditPort>,
}

#[async_trait]
impl EmbeddingProvider for QuotaEmbedding {
    async fn embed(&self, text: &str) -> Result<EmbeddingResponse> {
        let actor: Option<ActorIdentity> = CURRENT_ACTOR
            .try_with(std::clone::Clone::clone)
            .ok()
            .flatten();

        if let Some(a) = &actor {
            match self.quota.check(a).await {
                QuotaCheck::Exceeded {
                    used,
                    cap,
                    window_secs,
                } => {
                    metrics::counter!(
                        "akashic_quota_exceeded_total",
                        "bucket" => "embedding",
                    )
                    .increment(1);
                    audit_quota_exceeded(
                        self.audit.clone(),
                        a.clone(),
                        UsageKind::Embedding,
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

        let provider_label = self.inner.provider_label();
        let result = self.inner.embed(text).await;
        let result_label = if result.is_ok() { "ok" } else { "err" };
        // C5: emit on every call regardless of outcome.
        metrics::counter!(
            "akashic_embedding_calls_total",
            "provider" => provider_label,
            "result" => result_label,
        )
        .increment(1);
        let resp = result?;

        if let Some(a) = actor {
            let q = self.quota.clone();
            let tokens = resp.tokens_used;
            let model = resp.model.clone();
            tokio::spawn(async move {
                q.consume(&a, UsageKind::Embedding, tokens, &model).await;
            });
        }

        Ok(resp)
    }

    /// Batch path. Without this override, `QuotaEmbedding` would inherit the
    /// trait's default `embed_batch`, which loops `self.embed()` — one quota
    /// check + one API round-trip per text — silently defeating
    /// `OpenAiEmbedding::embed_batch`'s single-round-trip optimization on the
    /// ingestion hot path (B3 quota wrapper bug). We instead account quota once
    /// for the whole batch (mirroring `embed()`) and delegate to the INNER
    /// provider's `embed_batch`, preserving its one-call-per-sub-batch behavior.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let actor: Option<ActorIdentity> = CURRENT_ACTOR
            .try_with(std::clone::Clone::clone)
            .ok()
            .flatten();

        if let Some(a) = &actor {
            match self.quota.check(a).await {
                QuotaCheck::Exceeded {
                    used,
                    cap,
                    window_secs,
                } => {
                    metrics::counter!(
                        "akashic_quota_exceeded_total",
                        "bucket" => "embedding",
                    )
                    .increment(1);
                    audit_quota_exceeded(
                        self.audit.clone(),
                        a.clone(),
                        UsageKind::Embedding,
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

        let provider_label = self.inner.provider_label();
        // Delegate to the inner provider's batch impl (single round-trip per
        // sub-batch), NOT the default loop-over-self.embed().
        let result = self.inner.embed_batch(texts).await;
        let result_label = if result.is_ok() { "ok" } else { "err" };
        // C5: emit on every call regardless of outcome.
        metrics::counter!(
            "akashic_embedding_calls_total",
            "provider" => provider_label,
            "result" => result_label,
        )
        .increment(1);
        let vectors = result?;

        if let Some(a) = actor {
            // The inner batch API returns only vectors (no per-call token/model
            // metadata), so estimate token consumption from the input text size
            // (~4 chars/token, the common OpenAI heuristic). This keeps the
            // quota window fed for batch ingestion instead of accounting zero.
            let est_tokens: u32 = texts
                .iter()
                .map(|t| (t.len() / 4) as u64 + 1)
                .sum::<u64>()
                .min(u32::MAX as u64) as u32;
            let q = self.quota.clone();
            let model = provider_label.to_string();
            tokio::spawn(async move {
                q.consume(&a, UsageKind::Embedding, est_tokens, &model)
                    .await;
            });
        }

        Ok(vectors)
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActorIdentity;
    use akashic_kernel::AuthMethod;
    use akashic_kernel::audit::NoopAudit;
    use akashic_store_pg::repos::PgQuotaRepo;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StubEmbedder {
        dim: usize,
        tokens: u32,
        model: String,
        /// Counts how many times the (overridden) batch path was entered, so a
        /// test can prove `QuotaEmbedding` delegates to the inner provider's
        /// `embed_batch` rather than looping `self.embed`.
        batch_calls: AtomicUsize,
    }

    impl StubEmbedder {
        fn new(dim: usize, tokens: u32, model: &str) -> Self {
            Self {
                dim,
                tokens,
                model: model.into(),
                batch_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl EmbeddingProvider for StubEmbedder {
        async fn embed(&self, _text: &str) -> Result<EmbeddingResponse> {
            Ok(EmbeddingResponse {
                vector: vec![0.0; self.dim],
                tokens_used: self.tokens,
                model: self.model.clone(),
            })
        }
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            // A single batch call returns one vector per input — emphatically
            // NOT a loop of one-round-trip-per-text.
            self.batch_calls.fetch_add(1, Ordering::SeqCst);
            Ok(texts.iter().map(|_| vec![0.0; self.dim]).collect())
        }
        fn dimensions(&self) -> usize {
            self.dim
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
    async fn quota_embedding_bypasses_when_no_actor() {
        let pool = test_pool().await;
        let inner: Arc<dyn EmbeddingProvider> = Arc::new(StubEmbedder::new(8, 5, "stub-emb"));
        let quota = Arc::new(Quota::new(
            Arc::new(PgQuotaRepo::new(pool)),
            100,
            3600,
            true,
        ));
        let wrapped = QuotaEmbedding {
            inner,
            quota,
            audit: Arc::new(NoopAudit),
        };

        let r = wrapped.embed("text").await.unwrap();
        assert_eq!(r.vector.len(), 8);
    }

    #[tokio::test]
    async fn quota_embedding_returns_quota_exceeded_when_over_cap() {
        let pool = test_pool().await;
        let user = 2_100_001;
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
             VALUES ($1, 'mcp_token:t', 'embedding', 200, 'stub-emb')",
        )
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();

        let inner: Arc<dyn EmbeddingProvider> = Arc::new(StubEmbedder::new(8, 1, "stub-emb"));
        let quota = Arc::new(Quota::new(
            Arc::new(PgQuotaRepo::new(pool.clone())),
            100,
            3600,
            true,
        ));
        let wrapped = QuotaEmbedding {
            inner,
            quota,
            audit: Arc::new(NoopAudit),
        };

        let actor = ActorIdentity {
            user_id: user,
            token_id: "mcp_token:t".into(),
            auth_method: AuthMethod::DeviceFlow,
        };
        let result = CURRENT_ACTOR
            .scope(Some(actor), wrapped.embed("text"))
            .await;
        let err = result.unwrap_err();
        assert!(err.downcast_ref::<QuotaExceededError>().is_some());

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

    // B3 quota-wrapper batch-path regression: `embed_batch` must bypass quota
    // when there is no actor and must delegate to the INNER provider's
    // `embed_batch` (single round-trip) rather than the default loop.
    #[tokio::test]
    async fn quota_embedding_batch_bypasses_when_no_actor() {
        let pool = test_pool().await;
        // Keep a concrete handle so we can read the batch-call counter.
        let stub = Arc::new(StubEmbedder::new(8, 5, "stub-emb"));
        let inner: Arc<dyn EmbeddingProvider> = stub.clone();
        let quota = Arc::new(Quota::new(
            Arc::new(PgQuotaRepo::new(pool)),
            100,
            3600,
            true,
        ));
        let wrapped = QuotaEmbedding {
            inner,
            quota,
            audit: Arc::new(NoopAudit),
        };

        let texts = vec![
            "alpha".to_string(),
            "bravo".to_string(),
            "delta".to_string(),
        ];
        // No CURRENT_ACTOR scope — wrapper bypasses quota check + consume.
        let vectors = wrapped.embed_batch(&texts).await.unwrap();

        assert_eq!(vectors.len(), 3, "one vector per input");
        assert!(vectors.iter().all(|v| v.len() == 8));
        // Delegated to inner.embed_batch exactly once (not a per-text loop).
        assert_eq!(stub.batch_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn quota_embedding_batch_delegates_to_inner_batch() {
        let pool = test_pool().await;

        let user = 2_100_002;
        sqlx::query("DELETE FROM llm_usage WHERE actor_user_id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .ok();

        let stub = Arc::new(StubEmbedder::new(4, 9, "stub-emb"));
        let inner: Arc<dyn EmbeddingProvider> = stub.clone();
        // Generous cap so the call succeeds and the consume row is written.
        let quota = Arc::new(Quota::new(
            Arc::new(PgQuotaRepo::new(pool.clone())),
            1_000_000,
            3600,
            true,
        ));
        let wrapped = QuotaEmbedding {
            inner,
            quota,
            audit: Arc::new(NoopAudit),
        };

        let actor = ActorIdentity {
            user_id: user,
            token_id: "mcp_token:t".into(),
            auth_method: AuthMethod::DeviceFlow,
        };
        let texts = vec!["one".to_string(), "two".to_string()];
        let vectors = CURRENT_ACTOR
            .scope(Some(actor), wrapped.embed_batch(&texts))
            .await
            .unwrap();

        assert_eq!(vectors.len(), 2);
        // The inner batch impl ran once for the whole slice — proving we did
        // NOT fall back to the default loop-over-self.embed().
        assert_eq!(stub.batch_calls.load(Ordering::SeqCst), 1);

        // consume is fire-and-forget; wait briefly then assert a single batch
        // usage row was appended (estimated tokens, provider label as model).
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let row: (i32, String) = sqlx::query_as(
            "SELECT tokens_used, model FROM llm_usage \
             WHERE actor_user_id = $1 ORDER BY ts DESC LIMIT 1",
        )
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
        // "one"/"two" are 3 chars each → (3/4)+1 = 1 token apiece → 2 total.
        assert_eq!(row, (2, "unknown".to_string()));

        sqlx::query("DELETE FROM llm_usage WHERE actor_user_id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .ok();
    }
}
