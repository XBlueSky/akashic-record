pub mod anthropic;
pub mod openai;

pub use anthropic::AnthropicLlmProvider;
pub use openai::OpenAiLlmProvider;

use anyhow::Result;
use async_trait::async_trait;
use secrecy::ExposeSecret;

use akashic_config::{self as config, AiProvider};

// The trait + contract types now live in akashic-domain; re-export here so
// existing `akashic_llm::LlmProvider` / `akashic_llm::LlmResponse` / `akashic_llm::LlmUsage`
// paths keep resolving without churn in consumers.
pub use akashic_domain::ports::{LlmProvider, LlmResponse, LlmUsage};

/// Strip markdown code block wrappers from LLM output.
///
/// LLMs frequently wrap JSON in ```json ... ``` blocks even when instructed not to.
pub fn extract_json(raw: &str) -> &str {
    let trimmed = raw.trim();

    // Try ```json ... ``` first
    if let Some(rest) = trimmed.strip_prefix("```json") {
        return rest.trim_start().trim_end_matches("```").trim();
    }
    // Try ``` ... ```
    if let Some(rest) = trimmed.strip_prefix("```") {
        return rest.trim_start().trim_end_matches("```").trim();
    }

    trimmed
}

/// No-op provider for when no LLM is configured.
/// Errors at call time so callers can fall back gracefully.
struct NoOpLlmProvider;

#[async_trait]
impl LlmProvider for NoOpLlmProvider {
    async fn generate_json(&self, _prompt: &str) -> Result<LlmResponse> {
        anyhow::bail!(
            "No LLM provider configured. Set LLM_PROVIDER and LLM_API_KEY to enable LLM features."
        )
    }
    fn provider_label(&self) -> &'static str {
        "local"
    }
}

/// Build the appropriate LLM provider based on application config.
///
/// `cancel` is the top-level shutdown token; provider impls store a clone
/// and consult it via `tokio::select!` in their async methods so SIGTERM
/// cooperatively cancels in-flight LLM API calls.
pub fn build_llm_provider(
    cfg: &config::LlmConfig,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<Box<dyn LlmProvider>> {
    match cfg.provider {
        AiProvider::Local => {
            tracing::warn!(
                "LLM_PROVIDER=local: no LLM configured. Virtual module grouping will use prefix fallback, EXPLAINS edges will be skipped."
            );
            Ok(Box::new(NoOpLlmProvider))
        }
        AiProvider::OpenAi | AiProvider::Ollama | AiProvider::Custom => {
            let api_key = cfg
                .api_key
                .as_ref()
                .map(|s| s.expose_secret().to_string())
                .ok_or_else(|| {
                    anyhow::anyhow!("LLM_API_KEY required when LLM_PROVIDER={:?}", cfg.provider)
                })?;
            Ok(Box::new(crate::openai::OpenAiLlmProvider::new(
                api_key,
                cfg.model.clone(),
                cfg.base_url.clone(),
                cancel,
            )))
        }
        AiProvider::Anthropic => {
            let api_key = cfg
                .api_key
                .as_ref()
                .map(|s| s.expose_secret().to_string())
                .ok_or_else(|| {
                    anyhow::anyhow!("LLM_API_KEY required when LLM_PROVIDER=anthropic")
                })?;
            Ok(Box::new(crate::anthropic::AnthropicLlmProvider::new(
                api_key,
                cfg.model.clone(),
                cfg.base_url.clone(),
                cancel,
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_plain() {
        assert_eq!(extract_json(r#"["foo","bar"]"#), r#"["foo","bar"]"#);
    }

    #[test]
    fn extract_json_with_json_block() {
        let input = "```json\n[\"foo\",\"bar\"]\n```";
        assert_eq!(extract_json(input), r#"["foo","bar"]"#);
    }

    #[test]
    fn extract_json_with_plain_block() {
        let input = "```\n{\"key\": \"value\"}\n```";
        assert_eq!(extract_json(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn extract_json_with_whitespace() {
        let input = "  ```json\n  [\"a\"]\n  ```  ";
        assert_eq!(extract_json(input), r#"["a"]"#);
    }
}
