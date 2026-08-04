//! Top-level cooperative shutdown primitive for the akashic-record backend.
//!
//! `Shutdown` wraps a `tokio_util::sync::CancellationToken` and an optional
//! signal-listener task. Subsystems (axum server, MCP proxy, provider impls,
//! background tasks) consult the token via `cancelled().await` or
//! `is_cancelled()` and respond cooperatively.
//!
//! `drain_with_cap` is the bounded-join helper used in `main()` to await
//! subsystem shutdown with a hard wall-clock cap.

use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Top-level shutdown handle. Holds the cancellation token; signal handling
/// is installed via `install_signal_handler`.
#[derive(Clone, Debug)]
pub struct Shutdown {
    token: CancellationToken,
}

impl Shutdown {
    /// Construct a `Shutdown` from an already-existing token. Used in tests
    /// and when the caller wants explicit control over cancellation source.
    pub fn from_token(token: CancellationToken) -> Self {
        Self { token }
    }

    /// Borrow the underlying token (for cloning, passing to subsystems, or
    /// awaiting `cancelled()`).
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Future that resolves when shutdown has been requested.
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    /// Cancel immediately. Tests use this; production triggers cancellation
    /// via the signal handler.
    pub fn cancel(&self) {
        self.token.cancel();
    }
}

/// Outcome of a bounded drain.
#[derive(Debug, PartialEq, Eq)]
pub enum DrainOutcome {
    /// All work in the drain future completed inside the cap.
    Complete,
    /// Cap fired before the drain future resolved.
    TimedOut,
}

/// Run `work` to completion or abandon it after `cap`. Emits a structured
/// log line for both outcomes.
///
/// Logs `event="shutdown_drain_started"` before awaiting and one of
/// `event="drain_complete"` / `event="drain_timeout_force_exit"` after.
/// Also records `akashic_shutdown_drain_seconds{outcome}` histogram (C5).
pub async fn drain_with_cap<F: std::future::Future<Output = ()>>(
    cap: Duration,
    work: F,
) -> DrainOutcome {
    info!(event = "shutdown_drain_started", cap_secs = cap.as_secs());
    let start = std::time::Instant::now();
    match tokio::time::timeout(cap, work).await {
        Ok(()) => {
            let elapsed = start.elapsed().as_secs_f64();
            metrics::histogram!("akashic_shutdown_drain_seconds", "outcome" => "complete")
                .record(elapsed);
            info!(event = "drain_complete");
            DrainOutcome::Complete
        }
        Err(_) => {
            let elapsed = start.elapsed().as_secs_f64();
            metrics::histogram!("akashic_shutdown_drain_seconds", "outcome" => "timed_out")
                .record(elapsed);
            warn!(
                event = "drain_timeout_force_exit",
                cap_secs = cap.as_secs(),
                "in-flight work did not complete inside cap; aborting",
            );
            DrainOutcome::TimedOut
        }
    }
}

/// Install SIGTERM and SIGINT listeners on a spawned task. Returns a
/// `Shutdown` whose token will be cancelled when the first signal arrives.
///
/// Production-only: relies on `tokio::signal::unix`, which is not available
/// on Windows. The backend is Linux-only (containerized deployment).
#[cfg(unix)]
pub fn install_signal_handler() -> Shutdown {
    use tokio::signal::unix::{SignalKind, signal};

    let token = CancellationToken::new();
    let child = token.clone();
    tokio::spawn(async move {
        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
        let signal_name = tokio::select! {
            _ = sigterm.recv() => "SIGTERM",
            _ = sigint.recv() => "SIGINT",
        };
        info!(event = "shutdown_signal_received", signal = signal_name);
        metrics::counter!("akashic_shutdown_signal_total", "signal" => signal_name).increment(1);
        child.cancel();
    });
    Shutdown { token }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drain_with_cap_returns_complete_when_work_finishes_in_time() {
        let outcome = drain_with_cap(Duration::from_millis(100), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
        })
        .await;
        assert_eq!(outcome, DrainOutcome::Complete);
    }

    #[tokio::test]
    async fn drain_with_cap_returns_timed_out_when_work_exceeds_cap() {
        let outcome = drain_with_cap(Duration::from_millis(20), async {
            tokio::time::sleep(Duration::from_millis(200)).await;
        })
        .await;
        assert_eq!(outcome, DrainOutcome::TimedOut);
    }

    #[tokio::test]
    async fn shutdown_cancel_propagates_to_cloned_token() {
        let s = Shutdown::from_token(CancellationToken::new());
        let child = s.token().clone();
        assert!(!child.is_cancelled());
        s.cancel();
        assert!(child.is_cancelled());
    }

    #[tokio::test]
    async fn shutdown_cancelled_future_resolves_after_cancel() {
        let s = Shutdown::from_token(CancellationToken::new());
        let s2 = s.clone();
        let h = tokio::spawn(async move { s2.cancelled().await });
        // Give the spawned task a moment to start awaiting.
        tokio::time::sleep(Duration::from_millis(5)).await;
        s.cancel();
        h.await.unwrap();
    }
}
