//! The 5 canonical alert rules.
//!
//! Each rule's `evaluate(samples, state, tick_secs, now)` returns
//! `Some(Alert)` when the condition is met AND cooldown allows. The
//! caller (evaluator) does NOT need to apply cooldown — rules consult
//! `state.should_fire` themselves before producing an Alert.

use std::time::Instant;

use super::parser::Sample;
use super::state::EvaluatorState;

/// Rule identifiers carry the documented rule number (R1..R5) in their name
/// so logs and on-call references stay one-to-one with `docs/operations/on-call-sop.md`.
/// The leading `R<n>_` prefix is non-UpperCamelCase by design.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleId {
    R1_5xxRate,
    R2_LlmQuotaLow,
    R3_ReadinessProbeFail,
    R4_AuditLogSpike,
    R5_ShutdownDrainTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Critical,
}

impl Severity {
    pub fn emoji(self) -> &'static str {
        match self {
            Severity::Info => "ℹ️",
            Severity::Warn => "⚠️",
            Severity::Critical => "🚨",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Alert {
    pub rule: RuleId,
    pub severity: Severity,
    pub text: String,
}

/// Evaluate all 5 rules against a metric snapshot. Returns the alerts
/// that should be emitted this tick (cooldown-respecting). Updates
/// `state` (counter snapshots, last_fired, probe streak counters)
/// as a side effect.
pub fn evaluate_all(
    samples: &[Sample<'_>],
    state: &mut EvaluatorState,
    tick_secs: u64,
    now: Instant,
) -> Vec<Alert> {
    let mut out = Vec::new();
    if let Some(a) = r1_5xx_rate(samples, state, tick_secs, now) {
        out.push(a);
    }
    if let Some(a) = r2_llm_quota_low(samples, state, now) {
        out.push(a);
    }
    if let Some(a) = r3_readiness_probe_fail(samples, state, now) {
        out.push(a);
    }
    if let Some(a) = r4_audit_log_spike(samples, state, tick_secs, now) {
        out.push(a);
    }
    if let Some(a) = r5_shutdown_drain_timeout(samples, state, now) {
        out.push(a);
    }
    out
}

// ─── R1: HTTP 5xx rate ────────────────────────────────────────────────

const R1_THRESHOLD_PER_MIN: f64 = 5.0;

fn r1_5xx_rate(
    samples: &[Sample<'_>],
    state: &mut EvaluatorState,
    tick_secs: u64,
    now: Instant,
) -> Option<Alert> {
    let total: f64 = samples
        .iter()
        .filter(|s| s.metric == "akashic_http_requests_total")
        .filter(|s| s.labels.get("status").is_some_and(|s| s.starts_with('5')))
        .map(|s| s.value)
        .sum();
    // BUGFIX: snapshot the counter every tick (before the cooldown
    // early-return), otherwise the snapshot freezes during the cooldown
    // window and the next post-cooldown tick divides a multi-tick delta by
    // a single tick duration, inflating per_min into false alerts.
    let (prev, cur) = state.observe_counter("akashic_http_5xx_total", total);
    let delta = cur - prev;
    if !state.should_fire(RuleId::R1_5xxRate, now) {
        return None;
    }
    let per_min = delta * (60.0 / tick_secs as f64);
    if per_min > R1_THRESHOLD_PER_MIN {
        state.mark_fired(RuleId::R1_5xxRate, now);
        Some(Alert {
            rule: RuleId::R1_5xxRate,
            severity: Severity::Warn,
            text: format!(
                "{} [akashic-record] 5xx rate {:.1}/min (threshold {:.0}/min)",
                Severity::Warn.emoji(),
                per_min,
                R1_THRESHOLD_PER_MIN,
            ),
        })
    } else {
        None
    }
}

// ─── R2: LLM quota low ────────────────────────────────────────────────

const R2_QUOTA_FLOOR: f64 = 1000.0;

fn r2_llm_quota_low(
    samples: &[Sample<'_>],
    state: &mut EvaluatorState,
    now: Instant,
) -> Option<Alert> {
    if !state.should_fire(RuleId::R2_LlmQuotaLow, now) {
        return None;
    }
    let low: Vec<&Sample> = samples
        .iter()
        .filter(|s| s.metric == "akashic_llm_quota_remaining" && s.value < R2_QUOTA_FLOOR)
        .collect();
    if low.is_empty() {
        return None;
    }
    state.mark_fired(RuleId::R2_LlmQuotaLow, now);
    let text = if low.len() == 1 {
        let actor = low[0]
            .labels
            .get("actor_token_id")
            .map_or_else(|| "unknown".to_string(), |s| short_id(s));
        format!(
            "{} [akashic-record] LLM quota low: actor={} remaining={} tokens",
            Severity::Info.emoji(),
            actor,
            low[0].value as u64,
        )
    } else {
        format!(
            "{} [akashic-record] {} actors below LLM quota floor ({} tokens)",
            Severity::Info.emoji(),
            low.len(),
            R2_QUOTA_FLOOR as u64,
        )
    };
    Some(Alert {
        rule: RuleId::R2_LlmQuotaLow,
        severity: Severity::Info,
        text,
    })
}

fn short_id(full: &str) -> String {
    full.chars().take(16).collect()
}

// ─── R3: Readiness probe failure ──────────────────────────────────────

const R3_CONSECUTIVE_FAILS_THRESHOLD: u32 = 2;

fn r3_readiness_probe_fail(
    samples: &[Sample<'_>],
    state: &mut EvaluatorState,
    now: Instant,
) -> Option<Alert> {
    // BUGFIX (audit rules.rs:184): collect EVERY probe that crosses the
    // consecutive-fail threshold this tick, not just the first one. The
    // R3 cooldown is keyed by RuleId (a single fire suppresses the rule for
    // the whole window), so reporting only `fired.is_none()`'s first probe
    // meant a second probe failing in the same tick (or while the first's
    // alert is on cooldown) was silently dropped. We now fold all currently
    // failing probes into one Critical alert so none is lost.
    let mut failing: Vec<(String, u32)> = Vec::new();
    // Track every probe whose sample is PRESENT this tick, so a probe tracked
    // from a previous tick but missing now can have its streak reset below.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for s in samples
        .iter()
        .filter(|s| s.metric == "akashic_readiness_probe_status")
    {
        let probe = s.labels.get("probe").copied().unwrap_or("unknown");
        seen.insert(probe);
        // The `akashic_readiness_probe_status` gauge is emitted as 0=ok, 1=fail
        // (see readiness::spawn_poller). A value at/above 0.5 therefore means
        // the probe FAILED; below 0.5 means it is healthy.
        if s.value >= 0.5 {
            let streak = state.probe_fail_observed(probe);
            if streak >= R3_CONSECUTIVE_FAILS_THRESHOLD {
                failing.push((probe.to_string(), streak));
            }
        } else {
            state.probe_recovered(probe);
        }
    }
    // Absent-sample handling (audit rules.rs:184): a probe missing from this
    // scrape is not evidence of failure — reset its streak so a frozen count
    // can't later resume past the threshold.
    state.reset_probes_absent_from(&seen);
    if failing.is_empty() {
        return None;
    }
    if !state.should_fire(RuleId::R3_ReadinessProbeFail, now) {
        return None;
    }
    state.mark_fired(RuleId::R3_ReadinessProbeFail, now);
    // Deterministic ordering so the rendered text (and tests) don't depend on
    // sample iteration order, which is positional in the parsed scrape.
    failing.sort();
    let detail = failing
        .iter()
        .map(|(probe, streak)| format!("{probe} ({streak} consecutive ticks)"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(Alert {
        rule: RuleId::R3_ReadinessProbeFail,
        severity: Severity::Critical,
        text: format!(
            "{} [akashic-record] Readiness probe FAIL: {}",
            Severity::Critical.emoji(),
            detail,
        ),
    })
}

// ─── R4: Audit log write spike ────────────────────────────────────────

const R4_THRESHOLD_PER_MIN: f64 = 30.0;

fn r4_audit_log_spike(
    samples: &[Sample<'_>],
    state: &mut EvaluatorState,
    tick_secs: u64,
    now: Instant,
) -> Option<Alert> {
    let total: f64 = samples
        .iter()
        .filter(|s| s.metric == "akashic_audit_log_writes_total")
        .map(|s| s.value)
        .sum();
    // BUGFIX: snapshot the counter every tick (before the cooldown
    // early-return), otherwise the snapshot freezes during the cooldown
    // window and the next post-cooldown tick divides a multi-tick delta by
    // a single tick duration, inflating per_min into false alerts.
    let (prev, cur) = state.observe_counter("akashic_audit_log_writes_total", total);
    let delta = cur - prev;
    if !state.should_fire(RuleId::R4_AuditLogSpike, now) {
        return None;
    }
    let per_min = delta * (60.0 / tick_secs as f64);
    if per_min > R4_THRESHOLD_PER_MIN {
        state.mark_fired(RuleId::R4_AuditLogSpike, now);
        Some(Alert {
            rule: RuleId::R4_AuditLogSpike,
            severity: Severity::Warn,
            text: format!(
                "{} [akashic-record] Audit log spike: {:.0} writes/min (threshold {:.0}/min)",
                Severity::Warn.emoji(),
                per_min,
                R4_THRESHOLD_PER_MIN,
            ),
        })
    } else {
        None
    }
}

// ─── R5: Shutdown drain timeout ───────────────────────────────────────
// C5 emits `akashic_shutdown_drain_seconds` as a histogram with the
// `outcome` label; Prometheus renders the count series as
// `akashic_shutdown_drain_seconds_count{outcome="..."}`. We match the
// `_count` suffix and filter for `outcome="timed_out"`.

fn r5_shutdown_drain_timeout(
    samples: &[Sample<'_>],
    state: &mut EvaluatorState,
    now: Instant,
) -> Option<Alert> {
    let total: f64 = samples
        .iter()
        .filter(|s| s.metric == "akashic_shutdown_drain_seconds_count")
        .filter(|s| s.labels.get("outcome") == Some(&"timed_out"))
        .map(|s| s.value)
        .sum();
    // BUGFIX: snapshot the counter every tick (before the cooldown
    // early-return), otherwise the snapshot freezes during the cooldown
    // window and the next post-cooldown tick reports a delta spanning many
    // ticks instead of just the current tick.
    let (prev, cur) = state.observe_counter("akashic_shutdown_drain_timed_out_total", total);
    let delta = cur - prev;
    if !state.should_fire(RuleId::R5_ShutdownDrainTimeout, now) {
        return None;
    }
    if delta > 0.0 {
        state.mark_fired(RuleId::R5_ShutdownDrainTimeout, now);
        Some(Alert {
            rule: RuleId::R5_ShutdownDrainTimeout,
            severity: Severity::Critical,
            text: format!(
                "{} [akashic-record] Shutdown drain TIMEOUT: {:.0} new timed-out drains; recent restart had in-flight work that didn't drain cleanly",
                Severity::Critical.emoji(),
                delta,
            ),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::parser::parse;
    use std::time::{Duration, Instant};

    fn fresh_state() -> EvaluatorState {
        EvaluatorState::new(Duration::from_mins(5))
    }

    // ─── R1 ────────────────────────────────────────────────────────

    #[test]
    fn r1_fires_when_5xx_rate_exceeds_threshold() {
        let mut state = fresh_state();
        let now = Instant::now();
        let samples = parse(r#"akashic_http_requests_total{status="500"} 0"#);
        let _ = evaluate_all(&samples, &mut state, 60, now);
        let samples = parse(r#"akashic_http_requests_total{status="500"} 6"#);
        let alerts = evaluate_all(&samples, &mut state, 60, now + Duration::from_mins(1));
        assert!(alerts.iter().any(|a| a.rule == RuleId::R1_5xxRate));
    }

    #[test]
    fn r1_does_not_fire_within_cooldown() {
        let mut state = fresh_state();
        let now = Instant::now();
        let s0 = parse(r#"akashic_http_requests_total{status="500"} 0"#);
        let _ = evaluate_all(&s0, &mut state, 60, now);
        let s1 = parse(r#"akashic_http_requests_total{status="500"} 6"#);
        let _ = evaluate_all(&s1, &mut state, 60, now + Duration::from_mins(1));
        let s2 = parse(r#"akashic_http_requests_total{status="500"} 12"#);
        let alerts = evaluate_all(&s2, &mut state, 60, now + Duration::from_mins(2));
        assert!(!alerts.iter().any(|a| a.rule == RuleId::R1_5xxRate));
    }

    #[test]
    fn r1_does_not_fire_below_threshold() {
        let mut state = fresh_state();
        let now = Instant::now();
        let s0 = parse(r#"akashic_http_requests_total{status="500"} 0"#);
        let _ = evaluate_all(&s0, &mut state, 60, now);
        let s1 = parse(r#"akashic_http_requests_total{status="500"} 3"#);
        let alerts = evaluate_all(&s1, &mut state, 60, now + Duration::from_mins(1));
        assert!(!alerts.iter().any(|a| a.rule == RuleId::R1_5xxRate));
    }

    #[test]
    fn r1_fires_again_after_cooldown_elapses() {
        let mut state = fresh_state();
        let now = Instant::now();
        let s0 = parse(r#"akashic_http_requests_total{status="500"} 0"#);
        let _ = evaluate_all(&s0, &mut state, 60, now);
        let s1 = parse(r#"akashic_http_requests_total{status="500"} 6"#);
        let _ = evaluate_all(&s1, &mut state, 60, now + Duration::from_mins(1));
        let s2 = parse(r#"akashic_http_requests_total{status="500"} 50"#);
        let alerts = evaluate_all(&s2, &mut state, 60, now + Duration::from_secs(361));
        assert!(alerts.iter().any(|a| a.rule == RuleId::R1_5xxRate));
    }

    /// Regression for the cooldown-window snapshot-freeze bug: ticks during
    /// the cooldown window MUST keep advancing the counter snapshot, so the
    /// first post-cooldown tick computes a single-tick delta/rate — NOT a
    /// multi-tick delta divided by one tick (which inflated per_min 5x and
    /// produced false-magnitude alerts).
    #[test]
    fn r1_rate_is_single_tick_after_cooldown_window() {
        let mut state = fresh_state();
        let now = Instant::now();
        // t=0: seed snapshot at 0.
        let _ = evaluate_all(
            &parse(r#"akashic_http_requests_total{status="500"} 0"#),
            &mut state,
            60,
            now,
        );
        // t=60: +6 in one tick (6/min) → fires, starts the 300s cooldown.
        let fired = evaluate_all(
            &parse(r#"akashic_http_requests_total{status="500"} 6"#),
            &mut state,
            60,
            now + Duration::from_mins(1),
        );
        assert!(fired.iter().any(|a| a.rule == RuleId::R1_5xxRate));
        // t=120..300: steady +6 per tick, all inside cooldown → suppressed,
        // but each tick must still refresh the snapshot.
        for (i, total) in [(2u64, 12), (3, 18), (4, 24), (5, 30)] {
            // Bind the formatted String so the parsed `Sample`s (which borrow
            // it) outlive the `evaluate_all` call.
            let raw = format!(r#"akashic_http_requests_total{{status="500"}} {total}"#);
            let s = parse(&raw);
            let alerts = evaluate_all(&s, &mut state, 60, now + Duration::from_secs(60 * i));
            assert!(
                !alerts.iter().any(|a| a.rule == RuleId::R1_5xxRate),
                "tick {i} inside cooldown should be suppressed"
            );
        }
        // t=361: cooldown elapsed, +6 since the previous (t=300) tick.
        // Correct rate is 6/min. With the freeze bug the snapshot would still
        // be 6 (from the t=60 fire), yielding delta 30 → 30/min.
        let alerts = evaluate_all(
            &parse(r#"akashic_http_requests_total{status="500"} 36"#),
            &mut state,
            60,
            now + Duration::from_secs(361),
        );
        let a = alerts
            .iter()
            .find(|a| a.rule == RuleId::R1_5xxRate)
            .expect("r1 fires again after cooldown");
        // The rendered rate must reflect a single-tick delta (6.0/min), not
        // the inflated multi-tick delta (30.0/min).
        assert!(
            a.text.contains("rate 6.0/min"),
            "expected single-tick rate 6.0/min, got: {}",
            a.text
        );
    }

    // ─── R2 ────────────────────────────────────────────────────────

    #[test]
    fn r2_fires_when_quota_below_floor() {
        let mut state = fresh_state();
        let now = Instant::now();
        let samples =
            parse(r#"akashic_llm_quota_remaining{actor_token_id="mcp_token:abc123"} 500"#);
        let alerts = evaluate_all(&samples, &mut state, 60, now);
        assert!(alerts.iter().any(|a| a.rule == RuleId::R2_LlmQuotaLow));
    }

    #[test]
    fn r2_does_not_fire_when_quota_above_floor() {
        let mut state = fresh_state();
        let now = Instant::now();
        let samples = parse(r#"akashic_llm_quota_remaining{actor_token_id="mcp_token:abc"} 5000"#);
        let alerts = evaluate_all(&samples, &mut state, 60, now);
        assert!(!alerts.iter().any(|a| a.rule == RuleId::R2_LlmQuotaLow));
    }

    #[test]
    fn r2_aggregates_multiple_actors() {
        let mut state = fresh_state();
        let now = Instant::now();
        let samples = parse(
            "akashic_llm_quota_remaining{actor_token_id=\"a\"} 100\n\
             akashic_llm_quota_remaining{actor_token_id=\"b\"} 200\n\
             akashic_llm_quota_remaining{actor_token_id=\"c\"} 300\n",
        );
        let alerts = evaluate_all(&samples, &mut state, 60, now);
        let a = alerts
            .iter()
            .find(|a| a.rule == RuleId::R2_LlmQuotaLow)
            .expect("r2 fired");
        assert!(a.text.contains("3 actors"));
    }

    #[test]
    fn r2_does_not_fire_within_cooldown() {
        let mut state = fresh_state();
        let now = Instant::now();
        let samples = parse(r#"akashic_llm_quota_remaining{actor_token_id="a"} 100"#);
        let _ = evaluate_all(&samples, &mut state, 60, now);
        let alerts = evaluate_all(&samples, &mut state, 60, now + Duration::from_mins(1));
        assert!(!alerts.iter().any(|a| a.rule == RuleId::R2_LlmQuotaLow));
    }

    // ─── R3 ────────────────────────────────────────────────────────

    #[test]
    fn r3_fires_on_second_consecutive_fail() {
        let mut state = fresh_state();
        let now = Instant::now();
        // gauge convention: 1 = fail, 0 = ok.
        let s = parse(r#"akashic_readiness_probe_status{probe="postgres"} 1"#);
        let a1 = evaluate_all(&s, &mut state, 60, now);
        assert!(!a1.iter().any(|a| a.rule == RuleId::R3_ReadinessProbeFail));
        let a2 = evaluate_all(&s, &mut state, 60, now + Duration::from_mins(1));
        assert!(a2.iter().any(|a| a.rule == RuleId::R3_ReadinessProbeFail));
    }

    #[test]
    fn r3_recovery_resets_streak() {
        let mut state = fresh_state();
        let now = Instant::now();
        // gauge convention: 1 = fail, 0 = ok.
        let fail = parse(r#"akashic_readiness_probe_status{probe="postgres"} 1"#);
        let ok = parse(r#"akashic_readiness_probe_status{probe="postgres"} 0"#);
        let _ = evaluate_all(&fail, &mut state, 60, now);
        let _ = evaluate_all(&ok, &mut state, 60, now + Duration::from_mins(1));
        let alerts = evaluate_all(&fail, &mut state, 60, now + Duration::from_mins(2));
        assert!(
            !alerts
                .iter()
                .any(|a| a.rule == RuleId::R3_ReadinessProbeFail)
        );
    }

    // ─── R4 ────────────────────────────────────────────────────────

    #[test]
    fn r4_fires_on_spike() {
        let mut state = fresh_state();
        let now = Instant::now();
        let s0 = parse("akashic_audit_log_writes_total{action=\"save_note\"} 0");
        let _ = evaluate_all(&s0, &mut state, 60, now);
        let s1 = parse("akashic_audit_log_writes_total{action=\"save_note\"} 35");
        let alerts = evaluate_all(&s1, &mut state, 60, now + Duration::from_mins(1));
        assert!(alerts.iter().any(|a| a.rule == RuleId::R4_AuditLogSpike));
    }

    #[test]
    fn r4_does_not_fire_below_threshold() {
        let mut state = fresh_state();
        let now = Instant::now();
        let s0 = parse("akashic_audit_log_writes_total{action=\"save_note\"} 0");
        let _ = evaluate_all(&s0, &mut state, 60, now);
        let s1 = parse("akashic_audit_log_writes_total{action=\"save_note\"} 15");
        let alerts = evaluate_all(&s1, &mut state, 60, now + Duration::from_mins(1));
        assert!(!alerts.iter().any(|a| a.rule == RuleId::R4_AuditLogSpike));
    }

    // ─── R5 ────────────────────────────────────────────────────────

    #[test]
    fn r5_fires_on_first_timeout_increment() {
        let mut state = fresh_state();
        let now = Instant::now();
        let s0 = parse(r#"akashic_shutdown_drain_seconds_count{outcome="timed_out"} 0"#);
        let _ = evaluate_all(&s0, &mut state, 60, now);
        let s1 = parse(r#"akashic_shutdown_drain_seconds_count{outcome="timed_out"} 1"#);
        let alerts = evaluate_all(&s1, &mut state, 60, now + Duration::from_mins(1));
        assert!(
            alerts
                .iter()
                .any(|a| a.rule == RuleId::R5_ShutdownDrainTimeout)
        );
    }

    #[test]
    fn r5_does_not_fire_with_no_delta() {
        let mut state = fresh_state();
        let now = Instant::now();
        let s = parse(r#"akashic_shutdown_drain_seconds_count{outcome="timed_out"} 3"#);
        let _ = evaluate_all(&s, &mut state, 60, now);
        let alerts = evaluate_all(&s, &mut state, 60, now + Duration::from_mins(1));
        assert!(
            !alerts
                .iter()
                .any(|a| a.rule == RuleId::R5_ShutdownDrainTimeout)
        );
    }

    /// Regression for audit rules.rs:184 — when two probes both cross the
    /// consecutive-fail threshold in the SAME tick, the single RuleId-keyed
    /// R3 alert must name BOTH probes. The old `fired.is_none()` logic
    /// reported only the first failing probe and the per-RuleId cooldown then
    /// silently dropped the second probe's failure entirely.
    #[test]
    fn r3_reports_all_failing_probes_in_one_alert() {
        let mut state = fresh_state();
        let now = Instant::now();
        // gauge convention: 1 = fail, 0 = ok. Two probes failing together.
        let s = parse(
            "akashic_readiness_probe_status{probe=\"postgres\"} 1\n\
             akashic_readiness_probe_status{probe=\"neo4j\"} 1\n",
        );
        // Tick 1: streak reaches 1 for each — below threshold, no alert.
        let a1 = evaluate_all(&s, &mut state, 60, now);
        assert!(!a1.iter().any(|a| a.rule == RuleId::R3_ReadinessProbeFail));
        // Tick 2: streak reaches 2 for each — both cross the threshold and
        // must appear in the single Critical alert.
        let a2 = evaluate_all(&s, &mut state, 60, now + Duration::from_mins(1));
        let alert = a2
            .iter()
            .find(|a| a.rule == RuleId::R3_ReadinessProbeFail)
            .expect("r3 fires when probes cross the consecutive-fail threshold");
        assert_eq!(alert.severity, Severity::Critical);
        assert!(
            alert.text.contains("postgres") && alert.text.contains("neo4j"),
            "both failing probes must be named, got: {}",
            alert.text
        );
    }

    /// Regression for audit rules.rs:184 (absent-sample freeze). A probe whose
    /// readiness sample is MISSING from a scrape (poller not yet polled, or the
    /// series dropped) must have its consecutive-fail streak RESET — an absent
    /// sample is not evidence of failure, so a stale streak must not freeze
    /// across the gap and then resume past the threshold on the next fail.
    #[test]
    fn r3_absent_sample_resets_streak() {
        let mut state = fresh_state();
        let now = Instant::now();
        // gauge convention: 1 = fail, 0 = ok.
        let fail = parse(r#"akashic_readiness_probe_status{probe="postgres"} 1"#);
        // Tick 1: one fail → streak 1, below the 2-tick threshold, no alert.
        let a1 = evaluate_all(&fail, &mut state, 60, now);
        assert!(!a1.iter().any(|a| a.rule == RuleId::R3_ReadinessProbeFail));
        // Tick 2: the postgres readiness sample is ABSENT from this scrape
        // (only an unrelated metric is present). The streak must reset to 0.
        let other = parse(r#"akashic_http_requests_total{status="200"} 5"#);
        let _ = evaluate_all(&other, &mut state, 60, now + Duration::from_mins(1));
        // Tick 3: postgres fails again. With the reset this is only the FIRST
        // consecutive fail → must NOT fire. With the freeze bug it would be the
        // 2nd consecutive fail and would spuriously fire.
        let a3 = evaluate_all(&fail, &mut state, 60, now + Duration::from_mins(2));
        assert!(
            !a3.iter().any(|a| a.rule == RuleId::R3_ReadinessProbeFail),
            "absent sample must reset the streak; got a spurious fire: {a3:?}"
        );
    }
}
