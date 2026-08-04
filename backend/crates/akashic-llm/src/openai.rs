use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::{LlmProvider, LlmResponse, LlmUsage};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// OpenAI-compatible LLM provider.
///
/// Works with OpenAI, Ollama, vLLM, LM Studio, OpenRouter — any service
/// that implements the `/v1/chat/completions` endpoint.
pub struct OpenAiLlmProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    cancel: CancellationToken,
}

impl OpenAiLlmProvider {
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

    /// Reasoning models (gpt-5-nano, gpt-5-mini, o1*, o3*) reject temperature
    /// and use `max_completion_tokens` instead of `max_tokens`.
    fn is_reasoning_model(&self) -> bool {
        self.model.starts_with("gpt-5-nano")
            || self.model.starts_with("gpt-5-mini")
            || self.model.starts_with("o1")
            || self.model.starts_with("o3")
    }

    /// Standard chat models that still use the new `max_completion_tokens`
    /// param but DO support temperature (e.g. gpt-4o, gpt-5.4).
    fn uses_max_completion_tokens(&self) -> bool {
        self.is_reasoning_model()
            || self.model.starts_with("gpt-5")
            || self.model.starts_with("gpt-4o")
    }
}

/// Chat completion request body.
/// Fields wrapped in `Option` are skipped when `None`, so reasoning models
/// never send `temperature` and always use `max_completion_tokens`.
#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    response_format: ResponseFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ResponseFormat {
    r#type: &'static str,
}

/// Deserialization structs for OpenAI chat completion response.
#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: ChatUsage,
    model: String,
}

#[derive(serde::Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(serde::Deserialize)]
struct ChoiceMessage {
    content: String,
}

#[derive(serde::Deserialize)]
struct ChatUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

#[async_trait]
impl LlmProvider for OpenAiLlmProvider {
    fn provider_label(&self) -> &'static str {
        "openai"
    }

    async fn generate_json(&self, prompt: &str) -> Result<LlmResponse> {
        let url = format!("{}/chat/completions", self.base_url);

        let reasoning = self.is_reasoning_model();
        let new_tokens_param = self.uses_max_completion_tokens();

        let body = ChatRequest {
            model: &self.model,
            messages: vec![Message {
                role: "user",
                content: prompt,
            }],
            response_format: ResponseFormat {
                r#type: "json_object",
            },
            temperature: if reasoning { None } else { Some(0.0) },
            max_tokens: if new_tokens_param { None } else { Some(4096) },
            max_completion_tokens: if new_tokens_param { Some(16000) } else { None },
        };

        let send_fut = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .timeout(std::time::Duration::from_secs(if reasoning {
                120
            } else {
                30
            }))
            .send();
        let http_resp = tokio::select! {
            () = self.cancel.cancelled() => {
                return Err(anyhow::anyhow!("shutdown: LLM call cancelled"));
            }
            r = send_fut => r.context("OpenAI-compatible LLM request failed")?,
        };

        let status = http_resp.status();
        if !status.is_success() {
            let err_body = http_resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM API error {status}: {err_body}");
        }

        let resp: ChatResponse = http_resp
            .json()
            .await
            .context("OpenAI chat response parse")?;

        let text = resp
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("OpenAI returned no choices"))?
            .message
            .content;

        Ok(LlmResponse {
            text,
            usage: LlmUsage {
                input_tokens: resp.usage.prompt_tokens,
                output_tokens: resp.usage.completion_tokens,
            },
            model: resp.model,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_from_chat_response() {
        let body = r#"{
            "choices": [{"message": {"content": "ok"}}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 22, "total_tokens": 33},
            "model": "gpt-4o-mini"
        }"#;
        let resp: ChatResponse = serde_json::from_str(body).unwrap();
        assert_eq!(resp.usage.prompt_tokens, 11);
        assert_eq!(resp.usage.completion_tokens, 22);
        assert_eq!(resp.model, "gpt-4o-mini");
    }

    #[tokio::test]
    async fn generate_json_returns_err_when_token_cancelled_before_call() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        // Use 127.0.0.1:1 (no listener) so a real network call would also
        // fail — we want the cancel branch to win the select.
        let provider = OpenAiLlmProvider::new(
            "test-key".to_string(),
            "gpt-4o".to_string(),
            Some("http://127.0.0.1:1".to_string()),
            cancel,
        );
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            provider.generate_json("hello"),
        )
        .await
        .expect("must not hang waiting on network");
        assert!(r.is_err(), "expected Err on cancelled token, got {r:?}");
        let msg = format!("{:?}", r.unwrap_err());
        assert!(
            msg.to_lowercase().contains("shutdown"),
            "expected 'shutdown' in error, got {msg}"
        );
    }
}
