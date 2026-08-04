//! Evaluator state — per-rule last-fired tracking + previous-tick
//! counter snapshots (for rate calculations).
//!
//! Cooldown semantics: a rule's `should_fire(now)` returns true IFF
//! the rule has never fired OR `now - last_fired >= cooldown`. After
//! a fire, the caller MUST update `last_fired` via `mark_fired(now)`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::rules::RuleId;

/// Safety cap on the number of distinct counter keys tracked for delta
/// computation. Cardinality is bounded by design (the C5 spec forbids
/// per-actor labels), so this should never trip in practice; it exists so a
/// future high-cardinality label can't grow the map without bound over a
/// long-lived process.
const MAX_TRACKED_COUNTER_KEYS: usize = 4096;

#[derive(Debug)]
pub struct EvaluatorState {
    last_fired: HashMap<RuleId, Instant>,
    cooldown: Duration,
    /// Previous-tick counter snapshots, keyed by `(metric, label_fingerprint)`.
    /// Used by rules that need per-tick deltas (R1, R4, R5).
    prev_counters: HashMap<String, f64>,
    /// Per-probe consecutive-fail count for R3.
    probe_consecutive_fails: HashMap<String, u32>,
}

impl EvaluatorState {
    pub fn new(cooldown: Duration) -> Self {
        Self {
            last_fired: HashMap::new(),
            cooldown,
            prev_counters: HashMap::new(),
            probe_consecutive_fails: HashMap::new(),
        }
    }

    pub fn should_fire(&self, rule: RuleId, now: Instant) -> bool {
        match self.last_fired.get(&rule) {
            None => true,
            Some(&t) => now.duration_since(t) >= self.cooldown,
        }
    }

    pub fn mark_fired(&mut self, rule: RuleId, now: Instant) {
        self.last_fired.insert(rule, now);
    }

    /// Read+update a per-metric counter snapshot. Returns
    /// `(previous_value, current_value)`. First call for a given key
    /// returns `(current, current)` — delta is zero, no spurious fire.
    pub fn observe_counter(&mut self, key: &str, current: f64) -> (f64, f64) {
        if let Some(&prev) = self.prev_counters.get(key) {
            self.prev_counters.insert(key.to_string(), current);
            return (prev, current);
        }
        // New key. Guard against unbounded growth: if we're at the cap, reset
        // delta tracking (one tick of zero-delta) rather than leaking.
        if self.prev_counters.len() >= MAX_TRACKED_COUNTER_KEYS {
            tracing::warn!(
                event = "alerts_counter_state_overflow",
                cap = MAX_TRACKED_COUNTER_KEYS,
                "evaluator counter-state cap reached; resetting delta tracking"
            );
            self.prev_counters.clear();
        }
        self.prev_counters.insert(key.to_string(), current);
        (current, current)
    }

    pub fn probe_fail_observed(&mut self, probe: &str) -> u32 {
        let n = self
            .probe_consecutive_fails
            .entry(probe.to_string())
            .or_insert(0);
        *n += 1;
        *n
    }

    pub fn probe_recovered(&mut self, probe: &str) {
        self.probe_consecutive_fails.remove(probe);
    }

    /// Drop the consecutive-fail streak for every tracked probe whose sample
    /// was ABSENT from the current tick (i.e. not in `seen`). An absent
    /// readiness sample — the poller hasn't completed its first cycle, or a
    /// probe was reconfigured and its series dropped — is not evidence of
    /// failure, so a previously-accumulated streak must reset rather than
    /// freeze across the gap and later resume past the alert threshold.
    pub fn reset_probes_absent_from(&mut self, seen: &HashSet<&str>) {
        self.probe_consecutive_fails
            .retain(|probe, _| seen.contains(probe.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_state() -> EvaluatorState {
        EvaluatorState::new(Duration::from_mins(5))
    }

    #[test]
    fn first_check_should_fire() {
        let state = fresh_state();
        let now = Instant::now();
        assert!(state.should_fire(RuleId::R1_5xxRate, now));
    }

    #[test]
    fn within_cooldown_does_not_fire() {
        let mut state = fresh_state();
        let t0 = Instant::now();
        state.mark_fired(RuleId::R1_5xxRate, t0);
        let t1 = t0 + Duration::from_secs(100);
        assert!(!state.should_fire(RuleId::R1_5xxRate, t1));
    }

    #[test]
    fn after_cooldown_fires_again() {
        let mut state = fresh_state();
        let t0 = Instant::now();
        state.mark_fired(RuleId::R1_5xxRate, t0);
        let t1 = t0 + Duration::from_secs(301);
        assert!(state.should_fire(RuleId::R1_5xxRate, t1));
    }

    #[test]
    fn different_rules_have_independent_cooldown() {
        let mut state = fresh_state();
        let t0 = Instant::now();
        state.mark_fired(RuleId::R1_5xxRate, t0);
        assert!(state.should_fire(RuleId::R2_LlmQuotaLow, t0));
    }

    #[test]
    fn observe_counter_returns_zero_delta_first_time() {
        let mut state = fresh_state();
        let (prev, cur) = state.observe_counter("akashic_foo", 10.0);
        assert_eq!(prev, cur);
    }

    #[test]
    fn observe_counter_returns_prev_on_subsequent_calls() {
        let mut state = fresh_state();
        state.observe_counter("akashic_foo", 10.0);
        let (prev, cur) = state.observe_counter("akashic_foo", 23.0);
        assert_eq!(prev, 10.0);
        assert_eq!(cur, 23.0);
    }

    #[test]
    fn probe_consecutive_fails_accumulate() {
        let mut state = fresh_state();
        assert_eq!(state.probe_fail_observed("pg"), 1);
        assert_eq!(state.probe_fail_observed("pg"), 2);
        assert_eq!(state.probe_fail_observed("pg"), 3);
        state.probe_recovered("pg");
        assert_eq!(state.probe_fail_observed("pg"), 1);
    }
}
