//! Benchmark report: per-strategy, per-category aggregates + a comparison
//! table.
//!
//! Honesty note: the token-weighted OVERALL ratio (Σakashic / Σbaseline) is a
//! single number that is sensitive to the highest-cost category — one question
//! whose baseline pulls a huge blob (e.g. "grep every function signature" for
//! dead-code) can dominate the denominator and make the overall look far
//! cheaper than the typical per-question case. So the report leads with the
//! PER-CATEGORY ratios and their macro-average (each category weighted
//! equally), and reports the token-weighted overall only as a clearly-labelled
//! secondary number. Every ratio is shown beside the recall it was achieved at
//! — a cheaper-but-lower-recall strategy is not a win.

use crate::metrics::StrategyAgg;
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub struct StrategyReport {
    pub label: String,
    pub per_category: BTreeMap<String, StrategyAgg>,
    pub overall: StrategyAgg,
}

pub struct BenchReport {
    pub baseline: StrategyReport,
    pub akashic: StrategyReport,
}

/// One category's akashic-vs-baseline comparison.
pub struct CategoryRatio {
    pub category: String,
    /// akashic tokens as a percentage of baseline tokens (lower = cheaper).
    pub pct: f64,
    pub akashic_recall: f64,
    pub baseline_recall: f64,
}

impl BenchReport {
    /// akashic tokens as a percentage of baseline tokens, summed across ALL
    /// questions (token-weighted). `None` when the baseline pulled zero tokens.
    /// Sensitive to the highest-cost category — prefer [`Self::category_ratios`].
    pub fn token_ratio_pct(&self) -> Option<f64> {
        let base = self.baseline.overall.total_tokens;
        if base == 0 {
            return None;
        }
        Some(self.akashic.overall.total_tokens as f64 / base as f64 * 100.0)
    }

    /// Per-category token ratios (each category weighted equally), for every
    /// category present in both strategies with a non-zero baseline.
    pub fn category_ratios(&self) -> Vec<CategoryRatio> {
        let mut out = Vec::new();
        for (cat, b) in &self.baseline.per_category {
            let Some(a) = self.akashic.per_category.get(cat) else {
                continue;
            };
            if b.total_tokens == 0 {
                continue;
            }
            out.push(CategoryRatio {
                category: cat.clone(),
                pct: a.total_tokens as f64 / b.total_tokens as f64 * 100.0,
                akashic_recall: a.mean_recall,
                baseline_recall: b.mean_recall,
            });
        }
        out
    }

    /// Mean of the per-category ratios (macro-average — robust to one
    /// dominating category). `None` when no category qualifies.
    pub fn macro_avg_ratio_pct(&self) -> Option<f64> {
        let rs = self.category_ratios();
        if rs.is_empty() {
            return None;
        }
        Some(rs.iter().map(|r| r.pct).sum::<f64>() / rs.len() as f64)
    }
}

fn row(out: &mut String, category: &str, s: &StrategyAgg, label: &str) {
    let _ = writeln!(
        out,
        "{category:<16} {label:<9} {:>9} {:>10} {:>11.1} {:>11.3}",
        s.n, s.total_calls, s.mean_tokens, s.mean_recall
    );
}

/// Render the full comparison matrix: per-category + overall rows, the
/// per-category token ratios, and the honest headline.
pub fn format_table(report: &BenchReport) -> String {
    let mut out = String::new();
    out.push_str("=== Codebase-QA retrieval-cost benchmark ===\n\n");
    let _ = writeln!(
        out,
        "{:<16} {:<9} {:>9} {:>10} {:>11} {:>11}",
        "Category", "Strategy", "Questions", "ToolCalls", "MeanTokens", "MeanRecall"
    );

    // Union of categories, sorted (BTreeMap keys are already sorted).
    let mut cats: Vec<&String> = report
        .baseline
        .per_category
        .keys()
        .chain(report.akashic.per_category.keys())
        .collect();
    cats.sort();
    cats.dedup();

    for cat in cats {
        if let Some(s) = report.baseline.per_category.get(cat) {
            row(&mut out, cat, s, &report.baseline.label);
        }
        if let Some(s) = report.akashic.per_category.get(cat) {
            row(&mut out, cat, s, &report.akashic.label);
        }
    }

    out.push('\n');
    row(
        &mut out,
        "OVERALL",
        &report.baseline.overall,
        &report.baseline.label,
    );
    row(
        &mut out,
        "OVERALL",
        &report.akashic.overall,
        &report.akashic.label,
    );

    // Per-category token ratios (the honest primary view).
    out.push_str("\nToken ratio per category (akashic / baseline, lower = akashic cheaper):\n");
    let ratios = report.category_ratios();
    for r in &ratios {
        let _ = writeln!(
            out,
            "  {:<16} {:>7.1}%   (recall akashic {:.3} / baseline {:.3})",
            r.category, r.pct, r.akashic_recall, r.baseline_recall
        );
    }

    out.push('\n');
    match (
        report.macro_avg_ratio_pct(),
        ratios
            .iter()
            .map(|r| r.pct)
            .fold(None::<(f64, f64)>, |acc, p| {
                Some(acc.map_or((p, p), |(lo, hi)| (lo.min(p), hi.max(p))))
            }),
    ) {
        (Some(macro_avg), Some((lo, hi))) => {
            let _ = writeln!(
                out,
                "Headline: over {} questions, akashic's per-category token cost is {lo:.1}%-{hi:.1}% of \
baseline (macro-avg {macro_avg:.1}%); mean recall akashic {:.3} / baseline {:.3} (a token win counts only \
where recall holds).",
                report.akashic.overall.n,
                report.akashic.overall.mean_recall,
                report.baseline.overall.mean_recall,
            );
            if let Some(overall) = report.token_ratio_pct() {
                let _ = writeln!(
                    out,
                    "  (Token-weighted overall = {overall:.1}%, but that single number is sensitive to the \
highest-cost category — prefer the per-category ratios above.)",
                );
            }
        }
        _ => {
            let overall = report
                .token_ratio_pct()
                .map(|p| format!("{p:.1}% of baseline tokens"))
                .unwrap_or_else(|| "N/A (baseline pulled 0 tokens)".to_string());
            let _ = writeln!(
                out,
                "Headline: akashic used {overall} — akashic mean recall {:.3} vs baseline mean recall {:.3} (over {} questions).",
                report.akashic.overall.mean_recall,
                report.baseline.overall.mean_recall,
                report.akashic.overall.n,
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agg(tokens: usize, recall: f64, n: usize) -> StrategyAgg {
        StrategyAgg {
            total_tokens: tokens,
            total_calls: n,
            mean_tokens: tokens as f64 / n.max(1) as f64,
            mean_recall: recall,
            n,
        }
    }

    /// Two categories: "cheap" (akashic 50 / baseline 100 = 50%) and "huge"
    /// (akashic 100 / baseline 100000 = 0.1%). Token-weighted overall is
    /// dominated by "huge" (~0.15%); macro-avg is 25.05% — the divergence the
    /// honest headline exists to expose.
    fn sample() -> BenchReport {
        let mut base = BTreeMap::new();
        base.insert("cheap".to_string(), agg(100, 1.0, 1));
        base.insert("huge".to_string(), agg(100_000, 1.0, 1));
        let mut ak = BTreeMap::new();
        ak.insert("cheap".to_string(), agg(50, 1.0, 1));
        ak.insert("huge".to_string(), agg(100, 1.0, 1));
        BenchReport {
            baseline: StrategyReport {
                label: "baseline".into(),
                per_category: base,
                overall: agg(100_100, 1.0, 2),
            },
            akashic: StrategyReport {
                label: "akashic".into(),
                per_category: ak,
                overall: agg(150, 1.0, 2),
            },
        }
    }

    #[test]
    fn token_weighted_overall_is_akashic_over_baseline() {
        let r = sample();
        // 150 / 100100 * 100 ~= 0.1498
        assert!((r.token_ratio_pct().unwrap() - 0.149_850_1).abs() < 1e-4);
    }

    #[test]
    fn macro_avg_is_mean_of_per_category_ratios() {
        let r = sample();
        // (50.0 + 0.1) / 2 = 25.05
        assert!((r.macro_avg_ratio_pct().unwrap() - 25.05).abs() < 1e-6);
    }

    #[test]
    fn overall_ratio_none_when_baseline_zero() {
        let mut r = sample();
        r.baseline.overall = agg(0, 0.0, 2);
        assert_eq!(r.token_ratio_pct(), None);
    }

    #[test]
    fn table_leads_with_per_category_and_flags_the_weighted_overall() {
        let table = format_table(&sample());
        assert!(table.contains("Token ratio per category"));
        assert!(table.contains("macro-avg 25.")); // (50.0 + 0.1)/2 = 25.05 -> "25.1"
        assert!(table.contains("per-category token cost is 0.1%-50.0% of baseline"));
        assert!(table.contains("Token-weighted overall = 0.1%"));
        assert!(table.contains("sensitive to the"));
        // recall shown beside ratios.
        assert!(table.contains("recall akashic 1.000 / baseline 1.000"));
    }
}
