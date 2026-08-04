//! C5: tracing JSON subscriber + metrics-exporter-prometheus install.
//!
//! Both functions are called once at startup from `main()`. The recorder
//! handle returned by `install_metrics_recorder` is what the
//! `/api/v1/metrics` route renders.

use anyhow::{Context, Result};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use tracing_subscriber::EnvFilter;

/// Install the global tracing subscriber. Output is JSON, fields flattened
/// to top level. RUST_LOG controls level (default `info`).
///
/// MUST be called before any tracing emit. Replaces the previous
/// `tracing_subscriber::fmt()...init()` call in main().
pub fn install_subscriber() {
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(false)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
}

/// Install the global Prometheus recorder. Returns the handle used by the
/// `/api/v1/metrics` route to render the exposition.
///
/// Custom buckets for any metric ending in `_seconds` cover sub-millisecond
/// DB pings up to 30-second LLM tail latency.
pub fn install_metrics_recorder() -> Result<PrometheusHandle> {
    let handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Suffix("_seconds".to_string()),
            &[0.001, 0.01, 0.05, 0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0],
        )
        .context("set _seconds histogram buckets")?
        .install_recorder()
        .context("install Prometheus recorder")?;
    Ok(handle)
}
