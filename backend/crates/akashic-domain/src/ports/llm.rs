use anyhow::Result;
use async_trait::async_trait;

/// Token usage from a single LLM call (parsed from API response).
#[derive(Debug, Clone, Copy)]
pub struct LlmUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl LlmUsage {
    pub fn total(&self) -> u32 {
        // saturating: a misbehaving upstream reporting near-u32::MAX counts
        // must not panic (debug) or wrap (release) the quota math.
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

/// Wraps the LLM provider's text response with the usage metrics that B3
/// quota math needs. The model name comes from either the API response
/// or provider config.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
    pub usage: LlmUsage,
    pub model: String,
}

/// Trait abstracting over LLM providers for structured JSON generation.
///
/// Implementations must be `Send + Sync` so they can be shared via `Arc<dyn LlmProvider>`.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a prompt and receive an `LlmResponse` containing the raw JSON text,
    /// token usage, and the model name used.
    /// Implementations MUST force JSON output mode where the API supports it.
    async fn generate_json(&self, prompt: &str) -> Result<LlmResponse>;

    /// C5: provider kind label for metrics. Default `"unknown"` so legacy
    /// impls compile without changes; concrete providers override.
    fn provider_label(&self) -> &'static str {
        "unknown"
    }
}
