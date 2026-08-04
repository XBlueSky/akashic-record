use anyhow::Result;
use async_trait::async_trait;

/// Result of a single embedding call. Wraps the vector with the token
/// usage metric needed by B3 quota math.
#[derive(Debug, Clone)]
pub struct EmbeddingResponse {
    pub vector: Vec<f32>,
    pub tokens_used: u32,
    pub model: String,
}

/// Trait abstracting over embedding providers.
///
/// Implementations must be `Send + Sync` so they can be shared via `Arc<dyn EmbeddingProvider>`.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Generate an embedding vector for the given text.
    async fn embed(&self, text: &str) -> Result<EmbeddingResponse>;

    /// Generate embeddings for many texts. The default loops `embed` (correct but
    /// one round-trip per text); remote providers override it to embed a whole
    /// batch in a single API call. Returned vectors are 1:1 with `texts` by index.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            // `embed` returns EmbeddingResponse (B3 quota); take its vector.
            out.push(self.embed(t).await?.vector);
        }
        Ok(out)
    }

    /// The dimensionality of the vectors produced by this provider.
    fn dimensions(&self) -> usize;

    /// C5: provider kind label for metrics. Default `"unknown"` so legacy
    /// impls compile without changes; concrete providers override.
    fn provider_label(&self) -> &'static str {
        "unknown"
    }
}
