//! Provider matrix: build each column's `EmbeddingProvider` from a spec and
//! run the eval for each, skipping (not crashing on) any that fail to build/run.

use crate::corpus::load_corpus;
use crate::metrics::EvalMetrics;
use crate::runner::run_provider;
use akashic_config::AiProvider;
use akashic_embed::{CandleEmbedding, EmbeddingProvider, OpenAiEmbedding};
use tokio_util::sync::CancellationToken;

/// One column of the eval matrix. `api_key_env` names the env var holding the
/// key (kept out of the file so no secret is committed).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProviderSpec {
    pub label: String,
    pub provider: AiProvider,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
}

/// Build the `EmbeddingProvider` for one spec.
pub async fn build_provider(
    spec: &ProviderSpec,
    cancel: CancellationToken,
) -> anyhow::Result<Box<dyn EmbeddingProvider>> {
    match spec.provider {
        AiProvider::Local => Ok(Box::new(CandleEmbedding::load(cancel).await?)),
        AiProvider::Anthropic => {
            anyhow::bail!("Anthropic has no embedding API; use Local/OpenAi/Ollama/Custom")
        }
        AiProvider::OpenAi | AiProvider::Ollama | AiProvider::Custom => {
            let api_key = match &spec.api_key_env {
                Some(var) => std::env::var(var).map_err(|_| {
                    anyhow::anyhow!("env var {var} (api_key_env for '{}') not set", spec.label)
                })?,
                None => String::new(), // Ollama / local OpenAI-compat often need no key
            };
            Ok(Box::new(OpenAiEmbedding::new(
                api_key,
                spec.model.clone(),
                spec.base_url.clone(),
                cancel,
            )))
        }
    }
}

/// Run the eval for every spec over the corpus. Each column's result is
/// independent: a provider that fails to build or embed becomes an `Err` entry,
/// never aborting the others.
pub async fn run_matrix(
    pool: &sqlx::PgPool,
    repo: &str,
    specs: &[ProviderSpec],
) -> Vec<(String, anyhow::Result<EvalMetrics>)> {
    let items = match load_corpus(pool, repo).await {
        Ok(i) => i,
        Err(e) => {
            return specs
                .iter()
                .map(|s| (s.label.clone(), Err(anyhow::anyhow!("{e}"))))
                .collect();
        }
    };
    tracing::info!(repo, corpus = items.len(), "loaded eval corpus");

    let mut out = Vec::with_capacity(specs.len());
    for spec in specs {
        let res = async {
            let provider = build_provider(spec, CancellationToken::new()).await?;
            run_provider(provider.as_ref(), &items).await
        }
        .await;
        if let Err(e) = &res {
            tracing::warn!(label = %spec.label, error = %e, "provider column skipped");
        }
        out.push((spec.label.clone(), res));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db_url() -> String {
        std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://akashic:${PG_PASSWORD}@localhost:5432/akashic".into())
    }

    #[tokio::test]
    #[ignore = "requires live Postgres + local MiniLM model; run with --ignored"]
    async fn minilm_live_smoke_produces_plausible_metrics() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&test_db_url())
            .await
            .expect("connect pg");

        let repo = "e2e-embed-eval-smoke";
        // Idempotent: clean any residue from a previous run before seeding, and
        // again at the end, so this test never leaves data behind in a shared
        // dev DB and can be re-run freely.
        sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();

        // Discover the live embedding dim; the stored vector is irrelevant to
        // the eval (it re-embeds fresh in-memory — see corpus::load_corpus /
        // runner::run_provider) and only needs to satisfy the chunks table's
        // NOT NULL `embedding` column. Deliberately NOT calling init_schema
        // with a hardcoded dim here: a dim mismatch against the dev DB's
        // current dim would trigger migrate_vector_dims, which DELETES ALL
        // EMBEDDINGS — destructive and against this harness's read-only intent.
        let dim: i32 = sqlx::query_scalar::<_, Option<i32>>(
            "SELECT vector_dims(embedding) FROM chunks WHERE embedding IS NOT NULL LIMIT 1",
        )
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .flatten()
        .unwrap_or(384);
        let dummy = pgvector::Vector::from(vec![0.1f32; dim as usize]);

        let seeds = [
            (
                "add",
                "Adds two integers and returns the sum.",
                "fn add(a: i32, b: i32) -> i32 { a + b }",
            ),
            (
                "greet",
                "Returns a friendly greeting for the given name.",
                "fn greet(name: &str) -> String { format!(\"hi {name}\") }",
            ),
            (
                "is_even",
                "Reports whether the number is even.",
                "fn is_even(n: i64) -> bool { n % 2 == 0 }",
            ),
        ];
        for (name, doc, content) in seeds {
            // chunks NOT NULL columns: repo_name, module_path, chunk_type, name,
            // content, embedding (verified against akashic-store-pg/src/schema.rs).
            sqlx::query(
                "INSERT INTO chunks (id, repo_name, module_path, chunk_type, name, content, doc, embedding) \
                 VALUES (gen_random_uuid(), $1, 'src/lib.rs', 'function', $2, $3, $4, $5)",
            )
            .bind(repo)
            .bind(name)
            .bind(content)
            .bind(doc)
            .bind(&dummy)
            .execute(&pool)
            .await
            .unwrap();
        }

        let items = crate::corpus::load_corpus(&pool, repo)
            .await
            .expect("load corpus");
        assert_eq!(items.len(), 3);

        let spec = ProviderSpec {
            label: "minilm".into(),
            provider: akashic_config::AiProvider::Local,
            model: "all-MiniLM-L6-v2".into(),
            base_url: None,
            api_key_env: None,
        };
        let provider = build_provider(&spec, tokio_util::sync::CancellationToken::new())
            .await
            .expect("build MiniLM");
        let m = crate::runner::run_provider(provider.as_ref(), &items)
            .await
            .expect("run");

        println!(
            "minilm live smoke: queries={} mrr={:.4} recall@1={:.4} recall@5={:.4} recall@10={:.4} ndcg@10={:.4}",
            m.queries, m.mrr, m.recall_at_1, m.recall_at_5, m.recall_at_10, m.ndcg_at_10
        );
        assert_eq!(m.queries, 3);
        assert!(
            m.mrr > 0.0 && m.mrr <= 1.0,
            "MRR must be in (0,1], got {}",
            m.mrr
        );
        assert!(
            m.recall_at_10 >= m.recall_at_1,
            "recall@10 must be >= recall@1"
        );

        sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .ok();
    }
}
