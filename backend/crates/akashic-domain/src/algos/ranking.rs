//! Query intent / space analysis — pure, DB-free.
//!
//! Moved from `akashic-retrieval::graphrag::query_analyzer` (A1 Task 2).
//! The `init_archetypes` initializer (which calls an async embedding provider)
//! stays in the retrieval crate; only the pure analysis kernel lives here.

use regex::Regex;
use std::sync::{LazyLock, OnceLock};

use crate::types::QueryAnalysis;

static SYMBOL_ARCHETYPE: OnceLock<Vec<f32>> = OnceLock::new();
static CONCEPT_ARCHETYPE: OnceLock<Vec<f32>> = OnceLock::new();

static RE_PASCAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Z][a-z]+[A-Z]").unwrap());
static RE_SNAKE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z]+_[a-z]+").unwrap());
static RE_DOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\w+\.\w+").unwrap());
static RE_RUST_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\w+::\w+").unwrap());
static RE_BACKTICK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`[^`]+`").unwrap());
static RE_QUESTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(how|what|why|explain|overview|architecture|describe)\b").unwrap()
});

pub const SYMBOL_ARCHETYPE_TEXT: &str =
    "function name class struct method variable type interface enum trait impl";
pub const CONCEPT_ARCHETYPE_TEXT: &str =
    "how does it work explain architecture overview design pattern why purpose";

/// Register pre-computed archetype embedding vectors.
///
/// Call once at startup (via the retrieval crate's `init_archetypes`).
/// Silently no-ops if called more than once (OnceLock semantics).
pub fn set_archetypes(symbol: Vec<f32>, concept: Vec<f32>) {
    let _ = SYMBOL_ARCHETYPE.set(symbol);
    let _ = CONCEPT_ARCHETYPE.set(concept);
}

/// Analyze a query to determine low-level (BM25) vs high-level (vector) weight.
pub fn analyze(query: &str, query_embedding: &[f32]) -> QueryAnalysis {
    let (rule_low, rule_confidence) = rule_signal(query);
    let embed_low = archetype_signal(query_embedding);

    let rule_w = rule_confidence;
    let embed_w = 1.0 - rule_confidence;

    let low = rule_w * rule_low + embed_w * embed_low;
    let low_clamped = low.clamp(0.05, 0.95);

    QueryAnalysis {
        low_level_weight: low_clamped,
        high_level_weight: 1.0 - low_clamped,
    }
}

fn rule_signal(query: &str) -> (f64, f64) {
    let mut low_score = 0.0_f64;
    let mut confidence = 0.0_f64;

    if RE_PASCAL.is_match(query) || RE_SNAKE.is_match(query) {
        low_score += 0.6;
        confidence += 0.6;
    }
    if RE_DOT.is_match(query) || RE_RUST_PATH.is_match(query) {
        low_score += 0.3;
        confidence += 0.3;
    }
    if RE_BACKTICK.is_match(query) {
        low_score += 0.4;
        confidence += 0.5;
    }

    if RE_QUESTION.is_match(query) {
        low_score -= 0.3;
        confidence += 0.3;
    }

    if confidence == 0.0 {
        return (0.4, 0.1);
    }

    let confidence = confidence.min(1.0);
    let low_score = low_score.clamp(0.0, 1.0);
    (low_score, confidence)
}

fn archetype_signal(query_embedding: &[f32]) -> f64 {
    let (Some(sym_arch), Some(con_arch)) = (SYMBOL_ARCHETYPE.get(), CONCEPT_ARCHETYPE.get()) else {
        return 0.5;
    };

    let sim_symbol = cosine_similarity(query_embedding, sym_arch);
    let sim_concept = cosine_similarity(query_embedding, con_arch);

    let denom = sim_symbol + sim_concept;
    if denom <= 0.0 {
        return 0.5;
    }

    sim_symbol / denom
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    let mut norm_a = 0.0_f64;
    let mut norm_b = 0.0_f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let x = *x as f64;
        let y = *y as f64;
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        (dot / denom).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_case_query_is_low_level() {
        let q = "AppState";
        let fake_embedding = vec![0.0; 384];
        let analysis = analyze(q, &fake_embedding);
        assert!(
            analysis.low_level_weight > 0.5,
            "PascalCase should bias toward low-level, got {}",
            analysis.low_level_weight
        );
    }

    #[test]
    fn snake_case_query_is_low_level() {
        let q = "validate_jwt";
        let fake_embedding = vec![0.0; 384];
        let analysis = analyze(q, &fake_embedding);
        assert!(
            analysis.low_level_weight > 0.5,
            "snake_case should bias toward low-level, got {}",
            analysis.low_level_weight
        );
    }

    #[test]
    fn question_query_is_high_level() {
        let q = "how does authentication work";
        let fake_embedding = vec![0.0; 384];
        let analysis = analyze(q, &fake_embedding);
        assert!(
            analysis.high_level_weight > analysis.low_level_weight,
            "Question query should bias toward high-level, got low={} high={}",
            analysis.low_level_weight,
            analysis.high_level_weight
        );
    }

    #[test]
    fn backtick_query_strongly_low_level() {
        let q = "`validate_jwt` function";
        let fake_embedding = vec![0.0; 384];
        let analysis = analyze(q, &fake_embedding);
        assert!(
            analysis.low_level_weight > 0.6,
            "Backtick query should strongly bias low-level, got {}",
            analysis.low_level_weight
        );
    }

    #[test]
    fn ambiguous_query_stays_balanced() {
        let q = "error handling";
        let fake_embedding = vec![0.0; 384];
        let analysis = analyze(q, &fake_embedding);
        assert!(
            analysis.low_level_weight > 0.2 && analysis.low_level_weight < 0.8,
            "Ambiguous query should be balanced, got low={}",
            analysis.low_level_weight
        );
    }

    #[test]
    fn weights_sum_to_one() {
        let cases = vec![
            "AppState",
            "how does auth work",
            "`parse_config`",
            "error handling",
            "some random words",
        ];
        let fake_embedding = vec![0.0; 384];
        for q in cases {
            let analysis = analyze(q, &fake_embedding);
            let sum = analysis.low_level_weight + analysis.high_level_weight;
            assert!(
                (sum - 1.0).abs() < 1e-10,
                "Weights for '{q}' don't sum to 1.0: {sum}"
            );
        }
    }

    #[test]
    fn cosine_similarity_identical_vectors() {
        let v = vec![1.0_f32, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }
}
