//! Embeds queries + targets with one provider and ranks each query's true
//! target against the whole corpus (full distractor pool) by cosine similarity.

use crate::corpus::EvalItem;
use crate::metrics::{EvalMetrics, metrics};
use akashic_embed::EmbeddingProvider;

fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// A zero-norm (or empty) vector is genuinely broken provider output — e.g. an
/// OpenAI-compat/Ollama endpoint that 200-OKs with `embedding: []` or an
/// all-zero vector.
fn is_zero_or_empty(v: &[f32]) -> bool {
    v.is_empty() || v.iter().map(|x| x * x).sum::<f32>() <= f32::EPSILON
}

/// Run the doc→code eval for one provider over `items`. Returns the aggregate
/// metrics. Errors if the provider fails to embed (caller decides whether to
/// skip that column).
pub async fn run_provider(
    provider: &dyn EmbeddingProvider,
    items: &[EvalItem],
) -> anyhow::Result<EvalMetrics> {
    if items.is_empty() {
        return Ok(metrics(&[]));
    }
    let targets: Vec<String> = items.iter().map(|i| i.content.clone()).collect();
    let queries: Vec<String> = items.iter().map(|i| i.doc.clone()).collect();

    let mut tvecs = provider.embed_batch(&targets).await?;
    let mut qvecs = provider.embed_batch(&queries).await?;
    if tvecs.len() != items.len() || qvecs.len() != items.len() {
        anyhow::bail!(
            "provider returned {} target and {} query vectors for {} items",
            tvecs.len(),
            qvecs.len(),
            items.len()
        );
    }

    // Degeneracy guard, on the RAW (pre-normalization) vectors. A broken or
    // degenerate provider would otherwise be scored as PERFECT below: the
    // strict `>` comparison in the ranking loop never fires when every
    // similarity ties (all-zero → every cosine 0; constant output → every
    // cosine equal), so every rank comes out 0 and MRR/recall/NDCG all hit
    // 1.0. Reject it here so `run_matrix` records an honest skipped column
    // instead of a fake #1.
    //
    // Zero/empty check applies to BOTH sides: a zero-norm or empty vector on
    // either the query or target side is broken output regardless of source.
    if tvecs.iter().any(|v| is_zero_or_empty(v)) || qvecs.iter().any(|v| is_zero_or_empty(v)) {
        anyhow::bail!("provider returned degenerate embeddings (zero/empty/constant output)");
    }
    // All-identical check applies to TARGETS ONLY, not queries: if the
    // provider can't distinguish any two code bodies, retrieval is
    // meaningless. Deliberately NOT applied to queries — identical
    // (non-zero) queries are a legitimate input scenario (see
    // `misaligned_provider_scores_poorly`, which uses all-identical docs on
    // purpose), not evidence the provider itself is broken.
    if tvecs.len() >= 2 && tvecs[1..].iter().all(|v| v == &tvecs[0]) {
        anyhow::bail!("provider returned degenerate embeddings (zero/empty/constant output)");
    }

    for v in tvecs.iter_mut() {
        l2_normalize(v);
    }
    for v in qvecs.iter_mut() {
        l2_normalize(v);
    }

    // For query i, its rank = number of targets STRICTLY more similar than the
    // true target i (ties do not count as better → optimistic tie handling).
    let mut ranks = Vec::with_capacity(items.len());
    for (i, q) in qvecs.iter().enumerate() {
        let self_sim = dot(q, &tvecs[i]);
        let mut better = 0usize;
        for (j, t) in tvecs.iter().enumerate() {
            if j != i && dot(q, t) > self_sim {
                better += 1;
            }
        }
        ranks.push(better);
    }
    Ok(metrics(&ranks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::EvalItem;
    use akashic_domain::ports::EmbeddingResponse;
    use akashic_embed::EmbeddingProvider;

    fn item(doc: &str, content: &str) -> EvalItem {
        EvalItem {
            fqn: doc.into(),
            doc: doc.into(),
            content: content.into(),
            language: None,
        }
    }

    // Fake provider: embeds text as a one-hot vector at the index encoded in the
    // text's first char ('0'->0, '1'->1, ...). So query "0..." and target "0..."
    // map to the same basis vector → perfect self-retrieval.
    struct OneHotFake {
        dim: usize,
    }
    #[async_trait::async_trait]
    impl EmbeddingProvider for OneHotFake {
        async fn embed(&self, text: &str) -> anyhow::Result<EmbeddingResponse> {
            let idx = text
                .chars()
                .next()
                .and_then(|c| c.to_digit(10))
                .unwrap_or(0) as usize
                % self.dim;
            let mut v = vec![0.0f32; self.dim];
            v[idx] = 1.0;
            Ok(EmbeddingResponse {
                vector: v,
                tokens_used: 0,
                model: "fake".into(),
            })
        }
        fn dimensions(&self) -> usize {
            self.dim
        }
    }

    #[tokio::test]
    async fn perfect_provider_scores_perfect() {
        // doc i and content i share the leading digit i → one-hot aligned.
        let items = vec![
            item("0 alpha", "0 body"),
            item("1 beta", "1 body"),
            item("2 gamma", "2 body"),
        ];
        let fake = OneHotFake { dim: 8 };
        let m = run_provider(&fake, &items).await.unwrap();
        assert_eq!(m.queries, 3);
        assert!(
            (m.mrr - 1.0).abs() < 1e-9,
            "perfect provider must score MRR 1.0, got {}",
            m.mrr
        );
        assert!((m.recall_at_1 - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn misaligned_provider_scores_poorly() {
        // Every doc leads with '0' but each content leads with its own distinct
        // digit → query 0 aligns with target 0 only; queries 1,2 have no aligned
        // target (their true target's digit != 0) → they rank below target 0.
        let items = vec![item("0 a", "0 a"), item("0 b", "1 b"), item("0 c", "2 c")];
        let fake = OneHotFake { dim: 8 };
        let m = run_provider(&fake, &items).await.unwrap();
        assert_eq!(m.queries, 3);
        assert!(
            m.recall_at_1 < 1.0,
            "misaligned provider must not be perfect@1, got {}",
            m.recall_at_1
        );
    }

    // Fake provider: always returns an all-zero vector, regardless of input.
    // This is a real failure mode for an OpenAI-compat/Ollama endpoint that
    // 200-OKs with `embedding: []` or a zero vector.
    struct AllZeroFake {
        dim: usize,
    }
    #[async_trait::async_trait]
    impl EmbeddingProvider for AllZeroFake {
        async fn embed(&self, _text: &str) -> anyhow::Result<EmbeddingResponse> {
            Ok(EmbeddingResponse {
                vector: vec![0.0f32; self.dim],
                tokens_used: 0,
                model: "fake-zero".into(),
            })
        }
        fn dimensions(&self) -> usize {
            self.dim
        }
    }

    #[tokio::test]
    async fn degenerate_all_zero_provider_is_rejected() {
        // Pre-guard, this would score MRR 1.0 (every cosine is 0, strict `>`
        // never fires, every rank comes out 0) — a broken provider reported as
        // the best column instead of skipped.
        let items = vec![
            item("0 alpha", "0 body"),
            item("1 beta", "1 body"),
            item("2 gamma", "2 body"),
        ];
        let fake = AllZeroFake { dim: 8 };
        let result = run_provider(&fake, &items).await;
        assert!(
            result.is_err(),
            "all-zero provider output must be rejected as degenerate, not scored"
        );
    }

    // Fake provider: always returns the same non-zero vector, regardless of
    // input. A provider that can't distinguish any code body is degenerate
    // even though no individual vector is zero-norm.
    struct ConstantFake {
        dim: usize,
    }
    #[async_trait::async_trait]
    impl EmbeddingProvider for ConstantFake {
        async fn embed(&self, _text: &str) -> anyhow::Result<EmbeddingResponse> {
            let mut v = vec![0.0f32; self.dim];
            v[0] = 1.0;
            Ok(EmbeddingResponse {
                vector: v,
                tokens_used: 0,
                model: "fake-constant".into(),
            })
        }
        fn dimensions(&self) -> usize {
            self.dim
        }
    }

    #[tokio::test]
    async fn degenerate_constant_target_provider_is_rejected() {
        let items = vec![
            item("0 alpha", "0 body"),
            item("1 beta", "1 body"),
            item("2 gamma", "2 body"),
        ];
        let fake = ConstantFake { dim: 8 };
        let result = run_provider(&fake, &items).await;
        assert!(
            result.is_err(),
            "constant-output provider must be rejected as degenerate, not scored"
        );
    }
}
