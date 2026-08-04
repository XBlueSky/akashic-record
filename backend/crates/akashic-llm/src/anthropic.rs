use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{LlmProvider, LlmResponse, LlmUsage};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic LLM provider using the /v1/messages endpoint.
///
/// Key differences from OpenAI:
/// - Uses `x-api-key` header instead of Bearer auth
/// - Requires `anthropic-version` header
/// - `max_tokens` is mandatory (400 error without it)
/// - System prompt is a top-level field, NOT in messages array
/// - No native `response_format` — uses system prompt to force JSON
pub struct AnthropicLlmProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    cancel: CancellationToken,
}

impl AnthropicLlmProvider {
    pub fn new(
        api_key: String,
        model: String,
        base_url: Option<String>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            model,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            cancel,
        }
    }
}

/// Deserialization structs for Anthropic messages API response.
#[derive(serde::Deserialize)]
struct MsgResponse {
    content: Vec<ContentBlock>,
    usage: MsgUsage,
    model: String,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Other,
}

#[derive(serde::Deserialize)]
struct MsgUsage {
    input_tokens: u32,
    output_tokens: u32,
}

#[async_trait]
impl LlmProvider for AnthropicLlmProvider {
    fn provider_label(&self) -> &'static str {
        "anthropic"
    }

    async fn generate_json(&self, prompt: &str) -> Result<LlmResponse> {
        let url = format!("{}/messages", self.base_url);

        let send_fut = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&serde_json::json!({
                "model": self.model,
                "max_tokens": 4096,
                "system": "You must respond with valid JSON only. No markdown, no explanation, no code blocks. Output raw JSON.",
                "messages": [{"role": "user", "content": prompt}],
                "temperature": 0.0,
            }))
            .timeout(std::time::Duration::from_secs(30))
            .send();
        let http_resp = tokio::select! {
            () = self.cancel.cancelled() => {
                return Err(anyhow::anyhow!("shutdown: LLM call cancelled"));
            }
            r = send_fut => r.context("Anthropic LLM request failed")?,
        };

        let status = http_resp.status();
        if !status.is_success() {
            let err_body = http_resp.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic API error {status}: {err_body}");
        }

        let resp: MsgResponse = http_resp
            .json()
            .await
            .context("Anthropic message response parse")?;

        let text = resp
            .content
            .into_iter()
            .find_map(|b| match b {
                ContentBlock::Text { text } => Some(text),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("Anthropic returned no text content"))?;

        Ok(LlmResponse {
            text,
            usage: LlmUsage {
                input_tokens: resp.usage.input_tokens,
                output_tokens: resp.usage.output_tokens,
            },
            model: resp.model,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_from_message_response() {
        let body = r#"{
            "content": [{"type": "text", "text": "hello"}],
            "usage": {"input_tokens": 5, "output_tokens": 10},
            "model": "claude-3-5-sonnet-latest"
        }"#;
        let resp: MsgResponse = serde_json::from_str(body).unwrap();
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.output_tokens, 10);
        assert_eq!(resp.model, "claude-3-5-sonnet-latest");
    }

    #[tokio::test]
    async fn generate_json_returns_err_when_token_cancelled_before_call() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let provider = AnthropicLlmProvider::new(
            "test-key".to_string(),
            "claude-sonnet-4-6".to_string(),
            Some("http://127.0.0.1:1".to_string()),
            cancel,
        );
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            provider.generate_json("hello"),
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
}
