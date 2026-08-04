//! Harness end-to-end tests.
//!
//! - `deterministic_end_to_end`: hermetic (no DB). A temp source tree + a fake
//!   akashic dispatcher + a 2-question fixture exercise the full baseline-grep
//!   / akashic-dispatch / score / report path with exact assertions.
//! - `live_smoke_akashic_beats_baseline` (`#[ignore]`): requires live PG+Neo4j
//!   and an ingested repo (added in Task 8).

use std::fs;

use akashic_bench::akashic::{self, AkashicDispatcher, ServiceDispatcher};
use akashic_bench::baseline::BaselineRunner;
use akashic_bench::metrics::{Blob, TokenCounter};
use akashic_bench::questions::{AkashicStep, BaselineStep, Question, load_questions};
use akashic_bench::report::{BenchReport, format_table};
use akashic_bench::runner::score_strategy;
use async_trait::async_trait;
use std::path::PathBuf;

/// A canned dispatcher: returns a small answer-bearing blob per tool, so the
/// akashic strategy is deterministic without a DB.
struct FakeDispatcher;

#[async_trait]
impl AkashicDispatcher for FakeDispatcher {
    async fn dispatch(&self, step: &AkashicStep) -> anyhow::Result<Blob> {
        let s = match step.tool.as_str() {
            "traverse_code_calls" => "CALLERS of target_sym: caller_a",
            "detect_dead_code" => "dead-code candidates: none; nonexistent_xyz is live",
            other => anyhow::bail!("fake dispatcher: unexpected tool {other}"),
        };
        Ok(Blob(s.to_string()))
    }
}

fn q(id: &str, cat: &str, gt: &[&str], baseline: Vec<BaselineStep>, tool: &str) -> Question {
    Question {
        id: id.into(),
        category: cat.into(),
        question: "?".into(),
        ground_truth: gt.iter().map(|s| s.to_string()).collect(),
        baseline_plan: baseline,
        akashic_plan: vec![AkashicStep {
            tool: tool.into(),
            args: serde_json::json!({}),
        }],
    }
}

#[tokio::test]
async fn deterministic_end_to_end() {
    // Temp source tree: q1's symbol appears (baseline finds it), q2's does not.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/a.rs"),
        "fn target_sym() {}\nfn caller_a() { target_sym(); }\nfn other() {}\n",
    )
    .unwrap();

    let q1 = q(
        "wc1",
        "who-calls",
        &["target_sym"],
        vec![BaselineStep::Grep {
            pattern: "target_sym".into(),
            glob: Some("**/*.rs".into()),
        }],
        "traverse_code_calls",
    );
    // q2's ground-truth string is absent from the source tree, so the baseline
    // grep returns nothing (recall 0); the fake akashic answer contains it
    // (recall 1) — demonstrating the recall-guarded comparison.
    let q2 = q(
        "dc1",
        "dead-code",
        &["nonexistent_xyz"],
        vec![BaselineStep::Grep {
            pattern: "nonexistent_xyz".into(),
            glob: Some("**/*.rs".into()),
        }],
        "detect_dead_code",
    );
    let questions = [q1, q2];

    let counter = TokenCounter::new();
    let baseline = BaselineRunner::new(dir.path().to_path_buf(), 0);
    let fake = FakeDispatcher;

    let mut baseline_items = Vec::new();
    let mut akashic_items = Vec::new();
    for q in &questions {
        let b = baseline.run(&q.baseline_plan);
        baseline_items.push((q, q.baseline_plan.len(), b));
        let a = akashic::run(&fake, &q.akashic_plan).await.unwrap();
        akashic_items.push((q, q.akashic_plan.len(), a));
    }

    let report = BenchReport {
        baseline: score_strategy(&counter, "baseline", &baseline_items),
        akashic: score_strategy(&counter, "akashic", &akashic_items),
    };

    // Recall: baseline finds q1 not q2 -> 0.5; akashic (fake) answers both -> 1.0.
    assert_eq!(report.baseline.overall.n, 2);
    assert_eq!(report.akashic.overall.n, 2);
    assert!((report.baseline.overall.mean_recall - 0.5).abs() < 1e-9);
    assert!((report.akashic.overall.mean_recall - 1.0).abs() < 1e-9);

    // Per-category recall.
    assert!(
        (report.baseline.per_category["who-calls"].mean_recall - 1.0).abs() < 1e-9,
        "baseline finds the who-calls symbol"
    );
    assert!(
        (report.baseline.per_category["dead-code"].mean_recall - 0.0).abs() < 1e-9,
        "baseline misses the absent dead-code fact"
    );

    // Both strategies pulled non-zero tokens; the ratio is defined.
    assert!(report.baseline.overall.total_tokens > 0);
    assert!(report.akashic.overall.total_tokens > 0);
    assert!(report.token_ratio_pct().is_some());

    // The rendered table carries the headline with both mean recalls shown
    // (akashic 1.000 / baseline 0.500 here — a token win counts only where
    // recall holds).
    let table = format_table(&report);
    assert!(table.contains("Headline:"));
    assert!(table.contains("mean recall akashic 1.000 / baseline 0.500"));
    assert!(table.contains("Token ratio per category"));
}

/// Live smoke: run the bundled fixture against a live PG+Neo4j with an
/// ingested repo, and assert the core claim on the two categories where it
/// should hold most clearly — akashic pulls fewer tokens than the baseline at
/// no worse recall. Absolute numbers are the deliverable, not a fixed
/// threshold, so they are not asserted.
///
/// Prerequisite: the repo named in the fixture's akashic_plan args
/// (`d1-measure`) must be ingested into the live PG+Neo4j, and `--source-dir`
/// (the backend tree) present on disk. `#[ignore]` — run explicitly:
///   RUSTFLAGS="-Clink-arg=-fuse-ld=lld -Cdebuginfo=0" \
///     cargo test -p akashic-bench --jobs 1 --test harness_e2e -- --ignored live_smoke
#[tokio::test]
#[ignore = "requires live PG+Neo4j + an ingested `d1-measure` repo"]
async fn live_smoke_akashic_beats_baseline() {
    // build_app_state defaults DATABASE_URL to :5433; the dev stack is :5432.
    // Single-threaded test start, before any task spawns — safe to set_var.
    unsafe {
        if std::env::var("DATABASE_URL").is_err() {
            std::env::set_var(
                "DATABASE_URL",
                "postgres://akashic:${PG_PASSWORD}@localhost:5432/akashic",
            );
        }
    }

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source_dir = manifest.join("..").join(".."); // backend/ (the d1-measure source tree)
    let questions = load_questions(&manifest.join("benchmark/questions.json")).unwrap();

    let counter = TokenCounter::new();
    let baseline = BaselineRunner::new(source_dir, 2);
    let dispatcher = ServiceDispatcher::connect()
        .await
        .expect("connect live services");

    let mut baseline_items = Vec::new();
    let mut akashic_items = Vec::new();
    for q in &questions {
        let b = baseline.run(&q.baseline_plan);
        baseline_items.push((q, q.baseline_plan.len(), b));
        let a = akashic::run(&dispatcher, &q.akashic_plan)
            .await
            .expect("akashic plan");
        akashic_items.push((q, q.akashic_plan.len(), a));
    }

    let report = BenchReport {
        baseline: score_strategy(&counter, "baseline", &baseline_items),
        akashic: score_strategy(&counter, "akashic", &akashic_items),
    };
    println!("{}", format_table(&report));

    for cat in ["who-calls", "blast-radius"] {
        let b = report
            .baseline
            .per_category
            .get(cat)
            .unwrap_or_else(|| panic!("no baseline {cat}"));
        let a = report
            .akashic
            .per_category
            .get(cat)
            .unwrap_or_else(|| panic!("no akashic {cat}"));
        assert!(
            a.mean_recall >= b.mean_recall,
            "{cat}: akashic recall {} must be >= baseline recall {}",
            a.mean_recall,
            b.mean_recall
        );
        assert!(
            a.total_tokens < b.total_tokens,
            "{cat}: akashic tokens {} must be < baseline tokens {}",
            a.total_tokens,
            b.total_tokens
        );
    }
}
