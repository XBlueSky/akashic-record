//! C6: cooperative readiness state shared between the background poller
//! and the `/ready` handler.
//!
//! - `Probe` is the abstraction each dependency implements.
//! - `ReadinessState` is an `Arc<RwLock<HashMap>>` keyed by probe name.
//! - The poller updates `last_ok_at` only on success; the staleness rule
//!   in `check_ready` (30 seconds) provides debounce against transient
//!   failure.
//! - The `/ready` handler reads the state plus `shutdown.is_cancelled()`
//!   to produce a `ReadinessSummary` with HTTP 200 / 503 mapping.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

// `ProbeOutcome` and `ReadinessState` now live in `akashic-context` so that
// `AppState` (also in akashic-context) can carry a `ReadinessState` without
// pulling all of akashic-server into its dependency tree (which would create
// a cycle). Re-export them here so all internal callers (`check_ready`,
// `spawn_poller`, probe impls, tests) continue to use the unqualified names.
pub use akashic_context::{ProbeOutcome, ReadinessState};

/// Initialize a `ReadinessState` populated with `never_checked` entries
/// for each provided probe name. Pre-populating means `/ready` returns
/// `not_ready` (with explicit `last_ok_secs_ago=null`) immediately on
/// boot, before the first poll cycle completes.
pub fn new_state(probe_names: &[&'static str]) -> ReadinessState {
    akashic_context::new_readiness_state(probe_names)
}

#[async_trait]
pub trait Probe: Send + Sync {
    /// Run the probe. Return Ok(()) if dependency is healthy, Err with a
    /// human-readable reason otherwise.
    async fn probe(&self) -> Result<()>;
    /// Stable label used in metrics + JSON output. Closed set of values
    /// (e.g. "postgres", "neo4j", "embedding" — Task 2: no standalone "mcp"
    /// probe anymore, MCP rides the single-port REST app's own probes).
    fn name(&self) -> &'static str;
}

#[derive(Serialize, Debug, Clone)]
pub struct ProbeOutcomeSummary {
    pub ok: bool,
    pub last_ok_secs_ago: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct ReadinessSummary {
    pub status: &'static str, // "ready" | "not_ready" | "shutting_down"
    pub all_ok: bool,
    pub probes: BTreeMap<&'static str, ProbeOutcomeSummary>,
}

/// Read the readiness state and produce a JSON-serializable summary.
/// `stale_after` is the debounce threshold: a probe is considered ok
/// only if it has succeeded inside that window.
pub async fn check_ready(
    state: &ReadinessState,
    shutdown: &CancellationToken,
    stale_after: Duration,
) -> ReadinessSummary {
    if shutdown.is_cancelled() {
        return ReadinessSummary {
            status: "shutting_down",
            all_ok: false,
            probes: BTreeMap::new(),
        };
    }
    let now = Instant::now();
    let s = state.read().await;
    let mut probes = BTreeMap::new();
    let mut all_ok = !s.is_empty();
    for (&name, outcome) in s.iter() {
        let last_ok_secs = outcome.last_ok_at.map(|t| now.duration_since(t).as_secs());
        let ok = match last_ok_secs {
            Some(secs) => secs <= stale_after.as_secs(),
            None => false,
        };
        if !ok {
            all_ok = false;
        }
        probes.insert(
            name,
            ProbeOutcomeSummary {
                ok,
                last_ok_secs_ago: last_ok_secs,
                last_error: outcome.last_error.clone(),
            },
        );
    }
    ReadinessSummary {
        status: if all_ok { "ready" } else { "not_ready" },
        all_ok,
        probes,
    }
}

/// Spawn the readiness poller. Runs every `interval` and updates `state`
/// with the per-probe outcome. Each probe gets `per_probe_timeout` on top
/// of any internal timeout it has. Exits cleanly when `shutdown` is
/// cancelled.
///
/// Emits two C5 metrics per probe per cycle:
///   - `akashic_readiness_probe_status{probe}` gauge (0=ok, 1=fail)
///   - `akashic_readiness_probe_duration_seconds{probe}` histogram
pub fn spawn_poller(
    state: ReadinessState,
    probes: Vec<Arc<dyn Probe>>,
    interval: Duration,
    per_probe_timeout: Duration,
    shutdown: CancellationToken,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => {
                    tracing::info!(event = "readiness_poller_shutdown");
                    break;
                }
                _ = ticker.tick() => {
                    let probe_futs = probes.iter().map(|p| {
                        let p = p.clone();
                        let to = per_probe_timeout;
                        async move {
                            let start = Instant::now();
                            let outcome = match tokio::time::timeout(to, p.probe()).await {
                                Ok(Ok(())) => Ok(()),
                                Ok(Err(e)) => Err(format!("{e:#}")),
                                Err(_) => Err(format!("probe timed out after {to:?}")),
                            };
                            let elapsed = start.elapsed().as_secs_f64();
                            metrics::histogram!(
                                "akashic_readiness_probe_duration_seconds",
                                "probe" => p.name(),
                            )
                            .record(elapsed);
                            metrics::gauge!(
                                "akashic_readiness_probe_status",
                                "probe" => p.name(),
                            )
                            .set(if outcome.is_ok() { 0.0 } else { 1.0 });
                            (p.name(), outcome)
                        }
                    });
                    let outcomes = futures::future::join_all(probe_futs).await;
                    let now = Instant::now();
                    let mut s = state.write().await;
                    for (name, result) in outcomes {
                        let entry = s.entry(name).or_insert_with(ProbeOutcome::never_checked);
                        entry.last_check_at = now;
                        match result {
                            Ok(()) => {
                                entry.last_ok_at = Some(now);
                                entry.last_error = None;
                            }
                            Err(e) => {
                                entry.last_error = Some(e);
                                // last_ok_at NOT touched: that's the
                                // 30-second debounce mechanism.
                            }
                        }
                    }
                }
            }
        }
    });
}

// ── Probe implementations ─────────────────────────────────────────────────

pub mod probes {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result};
    use async_trait::async_trait;
    use sqlx::PgPool;

    use super::Probe;
    use akashic_embed::EmbeddingProvider;
    use akashic_store_neo4j::Neo4jPool;

    /// PostgreSQL readiness — `SELECT 1`.
    pub struct PgProbe {
        pub pool: PgPool,
    }

    #[async_trait]
    impl Probe for PgProbe {
        async fn probe(&self) -> Result<()> {
            sqlx::query("SELECT 1")
                .execute(&self.pool)
                .await
                .context("pg probe SELECT 1 failed")?;
            Ok(())
        }
        fn name(&self) -> &'static str {
            "postgres"
        }
    }

    /// Neo4j readiness — `RETURN 1`.
    pub struct Neo4jProbe {
        pub pool: Neo4jPool,
    }

    #[async_trait]
    impl Probe for Neo4jProbe {
        async fn probe(&self) -> Result<()> {
            self.pool
                .query(neo4rs::query("RETURN 1"))
                .await
                .context("neo4j probe RETURN 1 failed")?;
            Ok(())
        }
        fn name(&self) -> &'static str {
            "neo4j"
        }
    }

    /// Embedding provider readiness — active probe with a 1-token text.
    /// Uses the **raw** provider (bypassing QuotaEmbedding) so health
    /// probes never burn an actor's quota bucket.
    ///
    /// The real `embed()` call can be a billed upstream request (e.g. OpenAI),
    /// so it is throttled to at most once per `min_interval`: between calls the
    /// last outcome is replayed. Without this, the 5s readiness cadence would
    /// issue ~17k embedding requests/day against the provider.
    pub struct EmbeddingProbe {
        raw: Arc<dyn EmbeddingProvider>,
        min_interval: Duration,
        last: std::sync::Mutex<Option<(Instant, std::result::Result<(), String>)>>,
    }

    impl EmbeddingProbe {
        pub fn new(raw: Arc<dyn EmbeddingProvider>, min_interval: Duration) -> Self {
            Self {
                raw,
                min_interval,
                last: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl Probe for EmbeddingProbe {
        async fn probe(&self) -> Result<()> {
            // Replay a recent outcome rather than making a fresh (billed)
            // upstream call on every tick. The lock is never held across the
            // .await below.
            {
                let guard = self.last.lock().unwrap();
                if let Some((at, outcome)) = guard.as_ref()
                    && at.elapsed() < self.min_interval
                {
                    return match outcome {
                        Ok(()) => Ok(()),
                        Err(msg) => Err(anyhow::anyhow!("{msg} (cached)")),
                    };
                }
            }
            let result = self
                .raw
                .embed("readiness")
                .await
                .context("embedding probe embed('readiness') failed")
                .map(|_| ());
            let stored = match &result {
                Ok(()) => Ok(()),
                Err(e) => Err(format!("{e:#}")),
            };
            *self.last.lock().unwrap() = Some((Instant::now(), stored));
            result
        }
        fn name(&self) -> &'static str {
            "embedding"
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    use super::*;

    fn fresh_state(probes_with_ok_ages: &[(&'static str, Option<Duration>)]) -> ReadinessState {
        let mut map = HashMap::new();
        for &(name, age) in probes_with_ok_ages {
            let now = Instant::now();
            let last_ok_at = age.map(|a| now.checked_sub(a).unwrap_or(now));
            map.insert(
                name,
                ProbeOutcome {
                    last_ok_at,
                    last_check_at: now,
                    last_error: None,
                },
            );
        }
        Arc::new(RwLock::new(map))
    }

    #[tokio::test]
    async fn check_ready_returns_shutting_down_when_token_cancelled() {
        let state = fresh_state(&[("postgres", Some(Duration::from_secs(0)))]);
        let token = CancellationToken::new();
        token.cancel();
        let summary = check_ready(&state, &token, Duration::from_secs(30)).await;
        assert_eq!(summary.status, "shutting_down");
        assert!(!summary.all_ok);
    }

    #[tokio::test]
    async fn check_ready_returns_ready_when_all_probes_fresh() {
        let state = fresh_state(&[
            ("postgres", Some(Duration::from_secs(2))),
            ("neo4j", Some(Duration::from_secs(3))),
        ]);
        let token = CancellationToken::new();
        let summary = check_ready(&state, &token, Duration::from_secs(30)).await;
        assert_eq!(summary.status, "ready");
        assert!(summary.all_ok);
        assert_eq!(summary.probes.len(), 2);
        for s in summary.probes.values() {
            assert!(s.ok);
        }
    }

    #[tokio::test]
    async fn check_ready_returns_not_ready_when_one_probe_stale() {
        let state = fresh_state(&[
            ("postgres", Some(Duration::from_secs(2))),
            ("neo4j", Some(Duration::from_secs(45))),
        ]);
        let token = CancellationToken::new();
        let summary = check_ready(&state, &token, Duration::from_secs(30)).await;
        assert_eq!(summary.status, "not_ready");
        assert!(!summary.all_ok);
        assert!(summary.probes["postgres"].ok);
        assert!(!summary.probes["neo4j"].ok);
    }

    #[tokio::test]
    async fn check_ready_returns_not_ready_when_probe_never_succeeded() {
        let state = fresh_state(&[("embedding", None)]);
        let token = CancellationToken::new();
        let summary = check_ready(&state, &token, Duration::from_secs(30)).await;
        assert_eq!(summary.status, "not_ready");
        assert!(!summary.all_ok);
        assert_eq!(summary.probes["embedding"].last_ok_secs_ago, None);
        assert!(!summary.probes["embedding"].ok);
    }

    #[tokio::test]
    async fn check_ready_returns_not_ready_when_state_empty() {
        let state = fresh_state(&[]);
        let token = CancellationToken::new();
        let summary = check_ready(&state, &token, Duration::from_secs(30)).await;
        assert_eq!(summary.status, "not_ready");
        assert!(!summary.all_ok);
    }

    #[tokio::test]
    async fn probe_names_are_stable() {
        use crate::readiness::probes::*;
        let pg_pool =
            sqlx::PgPool::connect_lazy("postgres://x:y@127.0.0.1:1/z").expect("connect_lazy");
        let pg_probe = PgProbe { pool: pg_pool };
        assert_eq!(pg_probe.name(), "postgres");
    }
}
