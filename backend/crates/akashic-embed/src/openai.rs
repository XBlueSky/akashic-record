use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{EmbeddingProvider, EmbeddingResponse};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Remote embedding provider using the OpenAI embeddings API.
pub struct OpenAiEmbedding {
    client: reqwest::Client,
    api_key: String,
    model: String,
    dimensions: usize,
    base_url: String,
    cancel: CancellationToken,
}

/// Derive the vector dimensionality from the OpenAI model name.
fn dims_for_model(model: &str) -> usize {
    match model {
        "text-embedding-3-large" => 3072,
        "text-embedding-3-small" | "text-embedding-ada-002" => 1536,
        _ => 1536,
    }
}

impl OpenAiEmbedding {
    pub fn new(
        api_key: String,
        model: String,
        base_url: Option<String>,
        cancel: CancellationToken,
    ) -> Self {
        let dimensions = dims_for_model(&model);
        Self {
            client: reqwest::Client::new(),
            api_key,
            model,
            dimensions,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            cancel,
        }
    }
}

// ── Request / Response DTOs ──────────────────────────────────────────

#[derive(Serialize)]
struct EmbedRequest {
    model: String,
    input: String,
}

#[derive(Serialize)]
struct EmbedBatchRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
    usage: EmbedUsage,
    model: String,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
    /// Position in the request's `input` array — used to restore order.
    #[serde(default)]
    index: usize,
}

#[derive(Deserialize)]
struct EmbedUsage {
    total_tokens: u32,
}

// ── Trait impl ───────────────────────────────────────────────────────

#[async_trait]
impl EmbeddingProvider for OpenAiEmbedding {
    fn provider_label(&self) -> &'static str {
        "openai"
    }

    async fn embed(&self, text: &str) -> Result<EmbeddingResponse> {
        let body = EmbedRequest {
            model: self.model.clone(),
            input: text.to_string(),
        };

        let url = format!("{}/embeddings", self.base_url);
        let send_fut = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send();
        let resp = tokio::select! {
            () = self.cancel.cancelled() => {
                return Err(anyhow::anyhow!("shutdown: embedding call cancelled"));
            }
            r = send_fut => r.context("OpenAI embeddings request failed")?,
        };

        let status = resp.status();
        if !status.is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenAI API error {status}: {err_body}");
        }

        let parsed: EmbedResponse = resp.json().await.context("OpenAI embed response parse")?;
        let vector = parsed
            .data
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("OpenAI returned no embedding"))?
            .embedding;
        Ok(EmbeddingResponse {
            vector,
            tokens_used: parsed.usage.total_tokens,
            model: parsed.model,
        })
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // OpenAI's embeddings endpoint accepts an array `input` and returns one
        // `data` entry per input. Sub-batch to stay well under request limits.
        const MAX_BATCH: usize = 128;
        let url = format!("{}/embeddings", self.base_url);
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for group in texts.chunks(MAX_BATCH) {
            let body = EmbedBatchRequest {
                model: self.model.clone(),
                input: group.to_vec(),
            };
            // Audit #47: a stalled connection must not hang ingestion forever
            // nor ignore shutdown. Mirror embed()/llm::openai by setting a
            // per-request .timeout() AND racing self.cancel via tokio::select!,
            // so SIGTERM cancels an in-flight batch and a half-open socket
            // eventually errors out instead of blocking the ingestion future.
            let send_fut = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .timeout(std::time::Duration::from_mins(1))
                .send();
            let resp = tokio::select! {
                () = self.cancel.cancelled() => {
                    return Err(anyhow::anyhow!("shutdown: batch embedding call cancelled"));
                }
                r = send_fut => r.context("OpenAI batch embeddings request failed")?,
            };

            let status = resp.status();
            if !status.is_success() {
                let err_body = resp.text().await.unwrap_or_default();
                anyhow::bail!("OpenAI API error {status}: {err_body}");
            }

            let parsed: EmbedResponse = resp.json().await.context("parse OpenAI response")?;
            if parsed.data.len() != group.len() {
                anyhow::bail!(
                    "OpenAI returned {} embeddings for {} inputs",
                    parsed.data.len(),
                    group.len()
                );
            }
            // Restore request order by `index` (the API documents in-order data,
            // but sorting makes us robust to any reordering).
            let mut data = parsed.data;
            data.sort_by_key(|d| d.index);
            out.extend(data.into_iter().map(|d| d.embedding));
        }
        Ok(out)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_from_embed_response() {
        let body = r#"{
            "data": [{"embedding": [0.1, 0.2, 0.3]}],
            "usage": {"total_tokens": 7},
            "model": "text-embedding-3-large"
        }"#;
        let resp: EmbedResponse = serde_json::from_str(body).unwrap();
        assert_eq!(resp.usage.total_tokens, 7);
        assert_eq!(resp.model, "text-embedding-3-large");
        assert_eq!(resp.data[0].embedding, vec![0.1_f32, 0.2_f32, 0.3_f32]);
    }

    #[tokio::test]
    async fn embed_returns_err_when_token_cancelled_before_call() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let provider = OpenAiEmbedding::new(
            "test-key".to_string(),
            "text-embedding-3-small".to_string(),
            Some("http://127.0.0.1:1".to_string()),
            cancel,
        );
        let r = tokio::time::timeout(std::time::Duration::from_secs(2), provider.embed("hello"))
            .await
            .expect("must not hang");
        assert!(r.is_err(), "expected Err on cancelled token, got {r:?}");
        let msg = format!("{:?}", r.unwrap_err());
        assert!(
            msg.to_lowercase().contains("shutdown"),
            "expected 'shutdown' in error, got {msg}"
        );
    }

    // Audit #47: a pre-cancelled token must make embed_batch return promptly
    // (via the tokio::select! cancel branch) instead of blocking on a stalled
    // socket. base_url points at a closed port so the only non-hanging exit is
    // the cancel branch winning the select.
    #[tokio::test]
    async fn embed_batch_returns_err_when_token_cancelled_before_call() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let provider = OpenAiEmbedding::new(
            "test-key".to_string(),
            "text-embedding-3-small".to_string(),
            Some("http://127.0.0.1:1".to_string()),
            cancel,
        );
        let texts = vec!["alpha".to_string(), "bravo".to_string()];
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            provider.embed_batch(&texts),
        )
        .await
        .expect("must not hang");
        assert!(r.is_err(), "expected Err on cancelled token, got {r:?}");
        let msg = format!("{:?}", r.unwrap_err());
        assert!(
            msg.to_lowercase().contains("shutdown"),
            "expected 'shutdown' in error, got {msg}"
        );
    }

    // Empty input short-circuits without any HTTP (and so never touches the
    // cancel/timeout path). Cheap pure guard, kept here for the batch contract.
    #[tokio::test]
    async fn embed_batch_empty_returns_empty() {
        let cancel = CancellationToken::new();
        let provider = OpenAiEmbedding::new(
            "test-key".to_string(),
            "text-embedding-3-small".to_string(),
            Some("http://127.0.0.1:1".to_string()),
            cancel,
        );
        let r = provider.embed_batch(&[]).await.unwrap();
        assert!(r.is_empty());
    }
}
