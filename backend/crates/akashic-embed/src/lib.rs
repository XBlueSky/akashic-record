pub mod candle;
pub mod openai;

use anyhow::Result;
use secrecy::ExposeSecret;

pub use candle::CandleEmbedding;
pub use openai::OpenAiEmbedding;

// The trait + contract types now live in akashic-domain; re-export here so
// existing `akashic_embed::EmbeddingProvider` / `akashic_embed::EmbeddingResponse`
// paths keep resolving without churn in consumers.
pub use akashic_domain::ports::{EmbeddingProvider, EmbeddingResponse};

use akashic_config as config;

/// Build the appropriate provider based on application config.
///
/// `cancel` is the top-level shutdown token; provider impls store a clone
/// and consult it to cooperatively cancel in-flight work on SIGTERM.
pub async fn build_provider(
    cfg: &config::Config,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<Box<dyn EmbeddingProvider>> {
    match cfg.embedding.provider {
        config::AiProvider::Local => {
            let provider = self::candle::CandleEmbedding::load(cancel).await?;
            Ok(Box::new(provider))
        }
        config::AiProvider::OpenAi | config::AiProvider::Ollama | config::AiProvider::Custom => {
            let api_key = cfg
                .embedding
                .api_key
                .as_ref()
                .map(|s| s.expose_secret().to_string())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "EMBEDDING_API_KEY required when provider={:?}",
                        cfg.embedding.provider
                    )
                })?;
            let provider = self::openai::OpenAiEmbedding::new(
                api_key,
                cfg.embedding.model.clone(),
                cfg.embedding.base_url.clone(),
                cancel,
            );
            Ok(Box::new(provider))
        }
        config::AiProvider::Anthropic => {
            anyhow::bail!(
                "Anthropic does not provide an embedding API. Use OpenAI or Local for embeddings."
            )
        }
    }
}
