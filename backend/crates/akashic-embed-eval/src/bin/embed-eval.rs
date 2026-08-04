//! `embed-eval` — offline embedding-retrieval eval matrix over a Postgres corpus.

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(about = "Evaluate code-retrieval quality (doc→code) across embedding providers")]
struct Args {
    /// repo_name whose doc-bearing chunks form the corpus
    #[arg(long)]
    repo: String,
    /// JSON file: array of ProviderSpec { label, provider, model, base_url?, api_key_env? }
    #[arg(long)]
    providers: String,
    /// Postgres URL (defaults to $DATABASE_URL then $TEST_DATABASE_URL)
    #[arg(long)]
    database_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let args = Args::parse();

    let db_url = args
        .database_url
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .or_else(|| std::env::var("TEST_DATABASE_URL").ok())
        .ok_or_else(|| {
            anyhow::anyhow!("no --database-url and neither DATABASE_URL nor TEST_DATABASE_URL set")
        })?;

    let specs: Vec<akashic_embed_eval::providers::ProviderSpec> =
        serde_json::from_str(&std::fs::read_to_string(&args.providers)?)?;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&db_url)
        .await?;

    let results = akashic_embed_eval::providers::run_matrix(&pool, &args.repo, &specs).await;

    println!("\nrepo: {}  (doc→code retrieval eval)\n", args.repo);
    println!(
        "{:<24} {:>7} {:>8} {:>8} {:>9} {:>9} {:>8}",
        "provider", "queries", "MRR", "R@1", "R@5", "R@10", "NDCG@10"
    );
    for (label, res) in &results {
        match res {
            Ok(m) => println!(
                "{:<24} {:>7} {:>8.4} {:>8.4} {:>9.4} {:>9.4} {:>8.4}",
                label, m.queries, m.mrr, m.recall_at_1, m.recall_at_5, m.recall_at_10, m.ndcg_at_10
            ),
            Err(e) => println!("{label:<24}  SKIPPED: {e}"),
        }
    }
    println!();
    Ok(())
}
