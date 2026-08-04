//! Dead-code ranking — pure, DB-free.
//!
//! Given the zero-inbound-CALLS `function` chunks of a repo (each flagged with
//! whether it is a persisted entry point), drop the entry points, assign a
//! visibility-based confidence tier + reason, and order deterministically.
//! Mirrors `super::code_community` (pure algo + DTOs + formatter).

/// A zero-caller function candidate, as fetched from the graph (pre-tiering).
#[derive(Debug, Clone)]
pub struct DeadCodeFn {
    pub name: String,
    pub module_path: String,
    pub fqn: Option<String>,
    pub visibility: String,
    /// True when a persisted `(:Chunk)-[:IS_ENTRY_POINT]->(:Flow)` edge exists.
    pub is_entry_point: bool,
}

/// Confidence tier. Declaration order is the sort order: High < Medium < Low,
/// so an ascending sort lists high-confidence candidates first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    /// Lowercase label for human-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }
}

/// A ranked dead-code candidate.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeadCodeCandidate {
    pub name: String,
    pub module_path: String,
    pub fqn: Option<String>,
    pub visibility: String,
    pub confidence: Confidence,
    pub reason: String,
}

/// Summary + ranked candidates for a repo.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeadCodeReport {
    pub total_functions: usize,
    pub zero_caller: usize,
    pub after_entry_point_exclusion: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub candidates: Vec<DeadCodeCandidate>,
}

/// Fixed caveat appended to every rendered report (verbatim from the spec).
pub const CAVEAT: &str = "Candidates are zero-inbound-CALLS functions; call-graph recall is imperfect, so review before removing — a `low`-confidence public function may be external API or reached via dynamic dispatch.";

/// Map a visibility string to a (tier, reason). `private` is the strong signal;
/// missing/unknown visibility stays `medium` — do not over-assert death.
fn classify(visibility: &str) -> (Confidence, &'static str) {
    match visibility {
        "private" => (
            Confidence::High,
            "private + no caller found in the extracted call graph.",
        ),
        "crate" => (
            Confidence::Medium,
            "crate-visible + no caller; dead unless used by an unindexed sibling crate.",
        ),
        "public" => (
            Confidence::Low,
            "public + no caller in this repo; may be external API, a trait impl reached via dynamic dispatch, or called only from a macro.",
        ),
        _ => (
            Confidence::Medium,
            "visibility undetermined; no caller — not asserting death on a missing signal.",
        ),
    }
}

/// Drop entry points, tier the rest, and sort by (confidence high→low,
/// module_path, name) for stable output.
pub fn rank(fns: &[DeadCodeFn]) -> Vec<DeadCodeCandidate> {
    let mut out: Vec<DeadCodeCandidate> = fns
        .iter()
        .filter(|f| !f.is_entry_point)
        .map(|f| {
            let (confidence, reason) = classify(&f.visibility);
            DeadCodeCandidate {
                name: f.name.clone(),
                module_path: f.module_path.clone(),
                fqn: f.fqn.clone(),
                visibility: f.visibility.clone(),
                confidence,
                reason: reason.to_string(),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.confidence
            .cmp(&b.confidence)
            .then_with(|| a.module_path.cmp(&b.module_path))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Build the full report from all zero-caller functions + the repo's total
/// function count. `zero_caller` counts entry + non-entry; `rank` excludes
/// entry points; tier counts derive from the surviving candidates.
pub fn build_report(total_functions: usize, fns: &[DeadCodeFn]) -> DeadCodeReport {
    let zero_caller = fns.len();
    let candidates = rank(fns);
    let high = candidates
        .iter()
        .filter(|c| c.confidence == Confidence::High)
        .count();
    let medium = candidates
        .iter()
        .filter(|c| c.confidence == Confidence::Medium)
        .count();
    let low = candidates
        .iter()
        .filter(|c| c.confidence == Confidence::Low)
        .count();
    DeadCodeReport {
        total_functions,
        zero_caller,
        after_entry_point_exclusion: candidates.len(),
        high,
        medium,
        low,
        candidates,
    }
}

/// Render the report as an agent-facing string: summary + ranked candidates
/// (capped at `max_candidates`) + the fixed caveat.
pub fn format_dead_code(repo: &str, report: &DeadCodeReport, max_candidates: usize) -> String {
    let mut out = format!(
        "Dead-code candidates in '{repo}' (zero inbound CALLS, entry points excluded).\n\
         Functions: {} total · {} zero-caller · {} after entry-point exclusion.\n\
         Confidence: {} high · {} medium · {} low.\n\n",
        report.total_functions,
        report.zero_caller,
        report.after_entry_point_exclusion,
        report.high,
        report.medium,
        report.low,
    );

    if report.candidates.is_empty() {
        out.push_str("No dead-code candidates.\n\n");
    } else {
        for c in report.candidates.iter().take(max_candidates) {
            let fqn = c.fqn.as_deref().filter(|s| !s.is_empty());
            match fqn {
                Some(f) => out.push_str(&format!(
                    "[{}] {} · {} ({})\n   reason: {}\n",
                    c.confidence.label(),
                    c.name,
                    c.module_path,
                    f,
                    c.reason
                )),
                None => out.push_str(&format!(
                    "[{}] {} · {}\n   reason: {}\n",
                    c.confidence.label(),
                    c.name,
                    c.module_path,
                    c.reason
                )),
            }
        }
        if report.candidates.len() > max_candidates {
            out.push_str(&format!(
                "(+{} more, truncated — raise `max_candidates`)\n",
                report.candidates.len() - max_candidates
            ));
        }
        out.push('\n');
    }

    out.push_str(CAVEAT);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str, module: &str, vis: &str, entry: bool) -> DeadCodeFn {
        DeadCodeFn {
            name: name.into(),
            module_path: module.into(),
            fqn: None,
            visibility: vis.into(),
            is_entry_point: entry,
        }
    }

    #[test]
    fn tiers_by_visibility() {
        let fns = vec![
            f("a", "m", "private", false),
            f("b", "m", "crate", false),
            f("c", "m", "public", false),
            f("d", "m", "", false), // unknown/absent
        ];
        let out = rank(&fns);
        let by = |n: &str| out.iter().find(|c| c.name == n).unwrap().confidence;
        assert_eq!(by("a"), Confidence::High);
        assert_eq!(by("b"), Confidence::Medium);
        assert_eq!(by("c"), Confidence::Low);
        assert_eq!(by("d"), Confidence::Medium);
    }

    #[test]
    fn entry_points_are_excluded() {
        let fns = vec![
            f("keep", "m", "private", false),
            f("main", "m", "private", true),
        ];
        let out = rank(&fns);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "keep");
    }

    #[test]
    fn ordering_is_confidence_then_module_then_name() {
        let fns = vec![
            f("z_pub", "a", "public", false),   // Low
            f("a_priv", "b", "private", false), // High, module b
            f("m_priv", "a", "private", false), // High, module a
        ];
        let out = rank(&fns);
        let order: Vec<&str> = out.iter().map(|c| c.name.as_str()).collect();
        // High tier first, sorted by module then name; Low last.
        assert_eq!(order, vec!["m_priv", "a_priv", "z_pub"]);
    }

    #[test]
    fn ordering_is_deterministic() {
        let fns = vec![
            f("a", "m", "private", false),
            f("b", "m", "public", false),
            f("c", "m", "crate", false),
        ];
        assert_eq!(rank(&fns), rank(&fns));
    }

    #[test]
    fn rank_empty_is_empty() {
        assert!(rank(&[]).is_empty());
    }

    #[test]
    fn report_counts_are_correct() {
        let fns = vec![
            f("a", "m", "private", false),
            f("b", "m", "public", false),
            f("main", "m", "private", true),
        ];
        let r = build_report(10, &fns);
        assert_eq!(r.total_functions, 10);
        assert_eq!(r.zero_caller, 3);
        assert_eq!(r.after_entry_point_exclusion, 2);
        assert_eq!(r.high, 1);
        assert_eq!(r.medium, 0);
        assert_eq!(r.low, 1);
    }

    #[test]
    fn format_has_summary_and_caveat() {
        let r = build_report(5, &[f("orphan", "util", "private", false)]);
        let s = format_dead_code("acme", &r, 25);
        assert!(s.contains("acme"));
        assert!(s.contains("orphan"));
        assert!(s.contains("high"));
        assert!(s.contains("review before removing")); // caveat fragment
    }

    #[test]
    fn format_empty_still_has_caveat() {
        let r = build_report(3, &[]);
        let s = format_dead_code("acme", &r, 25);
        assert!(s.contains("No dead-code candidates"));
        assert!(s.contains("review before removing"));
    }
}
