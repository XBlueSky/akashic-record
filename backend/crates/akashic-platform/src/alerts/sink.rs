//! Alert sinks. Two impls:
//!   - `WebhookSink`: POSTs `{"text": "..."}` to the configured
//!     incoming-webhook URL.
//!   - `LogSink`: emits the alert at `tracing::warn!` (fallback when
//!     no webhook URL is configured).

use async_trait::async_trait;
use reqwest::Client;
use std::time::Duration;

#[async_trait]
pub trait Sink: Send + Sync {
    /// Send one alert. Errors are NEVER propagated — the sink logs and
    /// continues so the evaluator loop doesn't abort on transient
    /// webhook failures.
    async fn send(&self, msg: &str);
}

/// Push alerts to a incoming webhook (Slack-compatible `{"text"}` JSON POST).
pub struct WebhookSink {
    client: Client,
    url: String,
}

impl WebhookSink {
    pub fn new(url: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client builder");
        Self { client, url }
    }
}

#[async_trait]
impl Sink for WebhookSink {
    async fn send(&self, msg: &str) {
        let body = serde_json::json!({ "text": msg });
        let res = self.client.post(&self.url).json(&body).send().await;
        match res {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(event = "alerts_sink_webhook_ok", url = %self.url);
            }
            Ok(resp) => {
                tracing::warn!(
                    event = "alerts_sink_webhook_non2xx",
                    status = %resp.status(),
                    msg = %msg,
                    "webhook returned non-success; alert lost"
                );
            }
            Err(e) => {
                tracing::warn!(
                    event = "alerts_sink_webhook_err",
                    error = %e,
                    msg = %msg,
                    "webhook POST failed; alert lost"
                );
            }
        }
    }
}

/// Fallback when no webhook URL is configured. Logs at WARN.
pub struct LogSink;

#[async_trait]
impl Sink for LogSink {
    async fn send(&self, msg: &str) {
        tracing::warn!(event = "alerts_sink_log", msg = %msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Test-only Sink that captures messages in memory.
    pub(crate) struct FakeSink {
        pub captured: Arc<Mutex<Vec<String>>>,
    }

    impl FakeSink {
        pub fn new() -> Self {
            Self {
                captured: Arc::new(Mutex::new(Vec::new())),
            }
        }
        pub fn messages(&self) -> Vec<String> {
            self.captured.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Sink for FakeSink {
        async fn send(&self, msg: &str) {
            self.captured.lock().unwrap().push(msg.to_string());
        }
    }

    #[tokio::test]
    async fn fake_sink_captures_messages() {
        let sink = FakeSink::new();
        sink.send("hello").await;
        sink.send("world").await;
        assert_eq!(
            sink.messages(),
            vec!["hello".to_string(), "world".to_string()]
        );
    }

    #[tokio::test]
    async fn log_sink_does_not_panic() {
        let sink = LogSink;
        sink.send("test alert").await;
        // Pass if no panic; tracing output is best-effort to assert on.
    }
}
