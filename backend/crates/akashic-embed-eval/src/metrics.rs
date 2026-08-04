//! Retrieval metrics for the eval harness. Single relevant target per query,
//! binary relevance, so DCG reduces to a per-rank gain and ideal DCG = 1.

/// Aggregate retrieval metrics over a set of queries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvalMetrics {
    pub mrr: f64,
    pub recall_at_1: f64,
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub ndcg_at_10: f64,
    pub queries: usize,
}

/// Compute metrics from per-query ranks. `ranks[i]` is the 0-based rank of
/// query i's single true target in its similarity-sorted candidate list
/// (0 = top hit). An empty input yields all-zero metrics.
pub fn metrics(ranks: &[usize]) -> EvalMetrics {
    let n = ranks.len();
    if n == 0 {
        return EvalMetrics {
            mrr: 0.0,
            recall_at_1: 0.0,
            recall_at_5: 0.0,
            recall_at_10: 0.0,
            ndcg_at_10: 0.0,
            queries: 0,
        };
    }
    let nf = n as f64;
    let mut mrr = 0.0;
    let (mut r1, mut r5, mut r10) = (0usize, 0usize, 0usize);
    let mut ndcg = 0.0;
    for &rank in ranks {
        mrr += 1.0 / (rank as f64 + 1.0);
        if rank < 1 {
            r1 += 1;
        }
        if rank < 5 {
            r5 += 1;
        }
        if rank < 10 {
            r10 += 1;
            ndcg += 1.0 / (rank as f64 + 2.0).log2();
        }
    }
    EvalMetrics {
        mrr: mrr / nf,
        recall_at_1: r1 as f64 / nf,
        recall_at_5: r5 as f64 / nf,
        recall_at_10: r10 as f64 / nf,
        ndcg_at_10: ndcg / nf,
        queries: n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
    }

    #[test]
    fn all_top_hits_are_perfect() {
        let m = metrics(&[0, 0, 0]);
        approx(m.mrr, 1.0);
        approx(m.recall_at_1, 1.0);
        approx(m.recall_at_5, 1.0);
        approx(m.recall_at_10, 1.0);
        approx(m.ndcg_at_10, 1.0);
        assert_eq!(m.queries, 3);
    }

    #[test]
    fn mixed_ranks_compute_correctly() {
        // ranks: 1, 9, 10 (0-based). rank 10 is outside top-10.
        let m = metrics(&[1, 9, 10]);
        approx(m.recall_at_1, 0.0);
        approx(m.recall_at_5, 1.0 / 3.0); // only rank 1 < 5
        approx(m.recall_at_10, 2.0 / 3.0); // ranks 1 and 9 < 10; 10 is not
        approx(m.mrr, (1.0 / 2.0 + 1.0 / 10.0 + 1.0 / 11.0) / 3.0);
        // NDCG@10: 1/log2(1+2) + 1/log2(9+2) for the two in-top-10; rank 10 → 0.
        let ndcg = (1.0 / 3.0_f64.log2() + 1.0 / 11.0_f64.log2()) / 3.0;
        approx(m.ndcg_at_10, ndcg);
        assert_eq!(m.queries, 3);
    }

    #[test]
    fn empty_is_all_zero() {
        let m = metrics(&[]);
        approx(m.mrr, 0.0);
        assert_eq!(m.queries, 0);
    }
}
