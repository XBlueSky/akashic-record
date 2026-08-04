use tiktoken_rs::{CoreBPE, cl100k_base};

pub struct Blob(pub String);

pub struct TokenCounter {
    bpe: CoreBPE,
}

impl TokenCounter {
    pub fn new() -> Self {
        Self {
            bpe: cl100k_base().expect("cl100k_base BPE ships with tiktoken-rs"),
        }
    }
    pub fn count(&self, s: &str) -> usize {
        self.bpe.encode_ordinary(s).len()
    }
}

impl Default for TokenCounter {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn answer_recall(ground_truth: &[String], retrieved: &str) -> f64 {
    if ground_truth.is_empty() {
        return 1.0;
    }
    let hay = normalize(retrieved);
    let found = ground_truth
        .iter()
        .filter(|f| hay.contains(&normalize(f)))
        .count();
    found as f64 / ground_truth.len() as f64
}

#[derive(Debug, Clone)]
pub struct ItemMetrics {
    pub tool_calls: usize,
    pub tokens: usize,
    pub bytes: usize,
    pub recall: f64,
}

pub fn item_metrics(
    counter: &TokenCounter,
    plan_len: usize,
    blobs: &[Blob],
    ground_truth: &[String],
) -> ItemMetrics {
    let tokens = blobs.iter().map(|b| counter.count(&b.0)).sum();
    let bytes = blobs.iter().map(|b| b.0.len()).sum();
    let joined = blobs
        .iter()
        .map(|b| b.0.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    ItemMetrics {
        tool_calls: plan_len,
        tokens,
        bytes,
        recall: answer_recall(ground_truth, &joined),
    }
}

pub struct StrategyAgg {
    pub total_tokens: usize,
    pub total_calls: usize,
    pub mean_tokens: f64,
    pub mean_recall: f64,
    pub n: usize,
}

pub fn aggregate(items: &[ItemMetrics]) -> StrategyAgg {
    let n = items.len();
    let total_tokens: usize = items.iter().map(|i| i.tokens).sum();
    let total_calls: usize = items.iter().map(|i| i.tool_calls).sum();
    let mean_tokens = if n == 0 {
        0.0
    } else {
        total_tokens as f64 / n as f64
    };
    let mean_recall = if n == 0 {
        0.0
    } else {
        items.iter().map(|i| i.recall).sum::<f64>() / n as f64
    };
    StrategyAgg {
        total_tokens,
        total_calls,
        mean_tokens,
        mean_recall,
        n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_all_present() {
        let gt = vec![
            "analyze_impact".to_string(),
            "collect_impact_for_target".to_string(),
        ];
        let text = "fn analyze_impact() calls collect_impact_for_target here";
        assert_eq!(answer_recall(&gt, text), 1.0);
    }

    #[test]
    fn recall_partial_and_normalized() {
        let gt = vec!["Analyze_Impact".to_string(), "missing_fn".to_string()];
        // case-insensitive + whitespace-collapsed match finds the first, not the second
        let text = "the   analyze_impact   symbol";
        assert_eq!(answer_recall(&gt, text), 0.5);
    }

    #[test]
    fn aggregate_means() {
        let items = vec![
            ItemMetrics {
                tool_calls: 1,
                tokens: 100,
                bytes: 400,
                recall: 1.0,
            },
            ItemMetrics {
                tool_calls: 3,
                tokens: 300,
                bytes: 1200,
                recall: 0.5,
            },
        ];
        let agg = aggregate(&items);
        assert_eq!(agg.total_tokens, 400);
        assert_eq!(agg.total_calls, 4);
        assert_eq!(agg.n, 2);
        assert!((agg.mean_tokens - 200.0).abs() < 1e-9);
        assert!((agg.mean_recall - 0.75).abs() < 1e-9);
    }

    #[test]
    fn token_counter_counts_nonzero() {
        let c = TokenCounter::new();
        // "hello world" is a small, stable number of cl100k tokens (2).
        assert_eq!(c.count("hello world"), 2);
        assert_eq!(c.count(""), 0);
    }
}
