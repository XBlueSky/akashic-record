//! akashic-bench CLI — run the codebase-QA retrieval-cost benchmark: for each
//! question in the fixture, execute the baseline (grep/read over --source-dir)
//! and akashic (MCP-tool services over the live graph) plans, score both, and
//! print the comparison matrix.

use std::path::PathBuf;

use akashic_bench::akashic::{self, ServiceDispatcher};
use akashic_bench::baseline::BaselineRunner;
use akashic_bench::metrics::TokenCounter;
use akashic_bench::questions::load_questions;
use akashic_bench::report::{BenchReport, format_table};
use akashic_bench::runner::score_strategy;
use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser)]
#[command(name = "akashic-bench", about = "Codebase-QA retrieval-cost benchmark")]
struct Args {
    /// Ingested repo name (selects the graph the akashic strategy queries).
    #[arg(long)]
    repo: String,
    /// Path to that repo's source tree on disk (the baseline greps/reads here).
    #[arg(long)]
    source_dir: PathBuf,
    /// Question fixture (defaults to the crate's bundled benchmark/questions.json).
    #[arg(long)]
    questions: Option<PathBuf>,
    /// Context lines each baseline grep match carries.
    #[arg(long, default_value_t = 2)]
    context_lines: usize,
    /// Optional path to also write the rendered table.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Overrides for the live PG/Neo4j the akashic strategy reads (else dev defaults).
    #[arg(long)]
    database_url: Option<String>,
    #[arg(long)]
    neo4j_uri: Option<String>,
    #[arg(long)]
    neo4j_user: Option<String>,
    #[arg(long)]
    neo4j_password: Option<String>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

    // build_app_state (via ServiceDispatcher::connect) reads these from the
    // environment; forward the CLI overrides. set_var is unsafe in edition
    // 2024 (process-global) — sound here because this runs BEFORE the async
    // runtime is constructed below, so no worker threads (and no reader of
    // these vars) exist yet.
    unsafe {
        if let Some(v) = &args.database_url {
            std::env::set_var("DATABASE_URL", v);
        }
        if let Some(v) = &args.neo4j_uri {
            std::env::set_var("NEO4J_URI", v);
        }
        if let Some(v) = &args.neo4j_user {
            std::env::set_var("NEO4J_USER", v);
        }
        if let Some(v) = &args.neo4j_password {
            std::env::set_var("NEO4J_PASSWORD", v);
        }
    }

    tokio::runtime::Runtime::new()?.block_on(run(args))
}

async fn run(args: Args) -> Result<()> {
    let questions_path = args.questions.unwrap_or_else(|| {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/benchmark/questions.json"
        ))
    });
    let questions = load_questions(&questions_path)?;
    tracing::info!(repo = %args.repo, questions = questions.len(), "loaded fixture");

    let counter = TokenCounter::new();
    let baseline = BaselineRunner::new(args.source_dir.clone(), args.context_lines);
    let dispatcher = ServiceDispatcher::connect()
        .await
        .context("connecting akashic services")?;

    let mut baseline_items = Vec::with_capacity(questions.len());
    let mut akashic_items = Vec::with_capacity(questions.len());
    for q in &questions {
        let b_blobs = baseline.run(&q.baseline_plan);
        baseline_items.push((q, q.baseline_plan.len(), b_blobs));

        let a_blobs = akashic::run(&dispatcher, &q.akashic_plan)
            .await
            .with_context(|| format!("akashic plan for question '{}'", q.id))?;
        akashic_items.push((q, q.akashic_plan.len(), a_blobs));
    }

    let report = BenchReport {
        baseline: score_strategy(&counter, "baseline", &baseline_items),
        akashic: score_strategy(&counter, "akashic", &akashic_items),
    };
    let table = format_table(&report);
    println!("{table}");

    if let Some(out) = &args.out {
        std::fs::write(out, &table).with_context(|| format!("writing {}", out.display()))?;
        tracing::info!(out = %out.display(), "wrote results");
    }
    Ok(())
}
