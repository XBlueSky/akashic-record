//! Pure strategy scorer: given already-executed retrieval blobs per question,
//! compute per-category and overall metrics into a `StrategyReport`. Executing
//! the strategies (baseline grep / akashic services) to PRODUCE the blobs lives
//! in the CLI — keeping this module DB/FS-free and unit-testable.

use crate::metrics::{Blob, ItemMetrics, TokenCounter, aggregate, item_metrics};
use crate::questions::Question;
use crate::report::StrategyReport;
use std::collections::BTreeMap;

/// Score one strategy over a set of questions. `items` pairs each question with
/// the strategy's `plan_len` (tool-call count) and the blobs its plan produced.
pub fn score_strategy(
    counter: &TokenCounter,
    label: &str,
    items: &[(&Question, usize, Vec<Blob>)],
) -> StrategyReport {
    let mut by_cat: BTreeMap<String, Vec<ItemMetrics>> = BTreeMap::new();
    let mut all: Vec<ItemMetrics> = Vec::with_capacity(items.len());

    for (q, plan_len, blobs) in items {
        let m = item_metrics(counter, *plan_len, blobs, &q.ground_truth);
        by_cat
            .entry(q.category.clone())
            .or_default()
            .push(m.clone());
        all.push(m);
    }

    let per_category = by_cat
        .into_iter()
        .map(|(cat, ms)| (cat, aggregate(&ms)))
        .collect::<BTreeMap<_, _>>();
    let overall = aggregate(&all);

    StrategyReport {
        label: label.to_string(),
        per_category,
        overall,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(id: &str, cat: &str, gt: &[&str]) -> Question {
        Question {
            id: id.into(),
            category: cat.into(),
            question: "?".into(),
            ground_truth: gt.iter().map(|s| s.to_string()).collect(),
            baseline_plan: vec![],
            akashic_plan: vec![],
        }
    }

    #[test]
    fn scores_per_category_and_overall() {
        let counter = TokenCounter::new();
        let q1 = q("a", "who-calls", &["foo"]);
        let q2 = q("b", "dead-code", &["bar", "missing"]);
        // q1: 1 call, one blob containing "foo" -> recall 1.0
        // q2: 2 calls, one blob containing "bar" only -> recall 0.5
        let items: Vec<(&Question, usize, Vec<Blob>)> = vec![
            (&q1, 1, vec![Blob("the foo symbol".into())]),
            (&q2, 2, vec![Blob("only bar here".into())]),
        ];
        let r = score_strategy(&counter, "akashic", &items);

        assert_eq!(r.label, "akashic");
        assert_eq!(r.overall.n, 2);
        assert_eq!(r.overall.total_calls, 3);
        assert!((r.overall.mean_recall - 0.75).abs() < 1e-9);

        let wc = r.per_category.get("who-calls").unwrap();
        assert_eq!(wc.n, 1);
        assert!((wc.mean_recall - 1.0).abs() < 1e-9);
        let dc = r.per_category.get("dead-code").unwrap();
        assert_eq!(dc.n, 1);
        assert!((dc.mean_recall - 0.5).abs() < 1e-9);
        // tokens are counted, not zero.
        assert!(wc.total_tokens > 0);
    }
}
