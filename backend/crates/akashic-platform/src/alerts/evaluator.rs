//! The 60s-tick evaluator. Reads PrometheusHandle::render(), parses
//! it, runs the 5 rules, dispatches matched alerts. Cooperates with
//! the C2 shutdown token.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use akashic_config::AlertsConfig;
use akashic_context::AppState;

use super::parser::parse;
use super::rules::evaluate_all;
use super::sink::{LogSink, Sink, WebhookSink};
use super::state::EvaluatorState;

/// Spawn the evaluator. Returns the JoinHandle; caller typically
/// discards (the task runs until shutdown).
pub fn start_evaluator(
    state: AppState,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let cfg = state.config.alerts.clone();
    tokio::spawn(async move {
        run_evaluator(state, cfg, shutdown).await;
    })
}

async fn run_evaluator(state: AppState, cfg: AlertsConfig, shutdown: CancellationToken) {
    let sink: Arc<dyn Sink> = match cfg.webhook_url.as_ref() {
        Some(url) => Arc::new(WebhookSink::new(url.clone())),
        None => Arc::new(LogSink),
    };

    let eval_state = Arc::new(Mutex::new(EvaluatorState::new(Duration::from_secs(
        cfg.cooldown_secs,
    ))));

    tracing::info!(
        event = "alerts_evaluator_started",
        tick_secs = cfg.tick_secs,
        cooldown_secs = cfg.cooldown_secs,
        webhook_configured = cfg.webhook_url.is_some(),
    );

    let mut interval = tokio::time::interval(Duration::from_secs(cfg.tick_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Burn the immediate first tick — wait one full interval before the
    // first evaluation so backend boot doesn't generate spurious
    // counter-delta alerts.
    interval.tick().await;

    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                tracing::info!(event = "alerts_evaluator_shutdown");
                return;
            }
            _ = interval.tick() => {
                let rendered = state.metrics_handle.render();
                let samples = parse(&rendered);
                let mut s = eval_state.lock().await;
                let alerts = evaluate_all(&samples, &mut s, cfg.tick_secs, Instant::now());
                drop(s);
                for alert in alerts {
                    tracing::info!(
                        event = "alerts_fired",
                        rule = ?alert.rule,
                        severity = ?alert.severity,
                    );
                    sink.send(&alert.text).await;
                }
            }
        }
    }
}
