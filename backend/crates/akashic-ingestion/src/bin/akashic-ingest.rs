//! Standalone ingest CLI (Roadmap E1) + repo-scoped snapshot export/import
//! (Roadmap E2) — no HTTP server, no MCP layer, no auth.
//!
//! `ingest` triggers a real ingestion run directly against Postgres + Neo4j.
//! Closes the long-standing "no ingest CLI" gap: every prior D-mechanism
//! recall-measurement effort on this branch had to orchestrate a full HTTP
//! server + auth cycle (or thrash trying to do so autonomously) just to
//! trigger a re-ingest. `--until-stage 6` additionally skips the LLM-calling
//! Stage 9 (communities) and the flows/staleness bookkeeping (Stages 7/8),
//! which are irrelevant to measuring a resolver mechanism's CALLS.method
//! distribution.
//!
//! Re-running this CLI against the SAME repo_name + unchanged source tree is
//! cheap: Stage 4's content-match skip (see `stages::stage4_embed_store`)
//! reuses every already-embedded chunk whose content is byte-identical, so
//! only Stages 1/2/3/5/6 (all local, pure-Rust, no external service calls)
//! actually do meaningful work on a re-run — exactly the workflow for testing
//! a new resolver mechanism's recall gain without re-paying for embedding.
//!
//! `export`/`import` move one repo's ingestion output (Postgres rows + Neo4j
//! nodes/edges) between database pairs as a single `.tar.zst` file,
//! preserving original UUIDs so Postgres and Neo4j stay joined on `pg_id`
//! with no ID-remapping step. `export`/`import` never construct an
//! embedding/LLM provider — they move already-computed rows verbatim, they
//! don't re-embed or re-summarize.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use akashic_domain::ports::{
    EmbeddingProvider, LlmProvider, ModuleGraphRepo, ModuleRepo, SnapshotGraphRepo, SnapshotPgRepo,
};
use akashic_domain::types::GraphRepoSnapshot;
use akashic_ingestion::ingestion::pipeline::{IngestRequest, IngestionPipeline};
use akashic_store_neo4j::Neo4jPool;
use akashic_store_neo4j::repos::module_graph::Neo4jModuleGraphRepo;
use akashic_store_neo4j::repos::snapshot::Neo4jSnapshotRepo;
use akashic_store_pg::repos::module::PgModuleRepo;
use akashic_store_pg::repos::snapshot::PgSnapshotRepo;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotMeta {
    schema_version: u32,
    repo_name: String,
    exported_at: String,
}

#[derive(Debug, Parser)]
#[command(
    name = "akashic-ingest",
    about = "Standalone ingest CLI — no HTTP server, no auth. See module docs."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run a real ingestion pipeline (Roadmap E1).
    Ingest {
        /// Repository name to ingest under (scopes all chunks/notes/graph nodes).
        #[arg(long)]
        repo_name: String,

        /// Git ref/branch to record against the ingested chunks.
        #[arg(long, default_value = "main")]
        git_ref: String,

        /// Local filesystem path to ingest (v1: local paths only).
        #[arg(long, required = true)]
        local_path: Option<String>,

        /// Stop after this pipeline stage (1-9).
        #[arg(long, default_value_t = 9, value_parser = clap::value_parser!(u8).range(1..=9))]
        until_stage: u8,
    },
    /// Export one repo's ingestion output to a portable `.tar.zst` file (Roadmap E2).
    Export {
        #[arg(long)]
        repo_name: String,
        /// Output archive path, e.g. `snapshot.tar.zst`.
        #[arg(long)]
        out: String,
    },
    /// Import a previously-exported snapshot into the currently-configured
    /// database pair. Only supports a `repo_name` that does not yet exist there.
    Import {
        #[arg(long)]
        file: String,
        #[arg(long)]
        repo_name: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::Ingest {
            repo_name,
            git_ref,
            local_path,
            until_stage,
        } => run_ingest(repo_name, git_ref, local_path, until_stage).await,
        Commands::Export { repo_name, out } => run_export(repo_name, out).await,
        Commands::Import { file, repo_name } => run_import(file, repo_name).await,
    }
}

async fn run_ingest(
    repo_name: String,
    git_ref: String,
    local_path: Option<String>,
    until_stage: u8,
) -> anyhow::Result<()> {
    let cfg = akashic_config::Config::from_env()?;

    let neo4j = Neo4jPool::connect(&cfg).await?;
    let pg = akashic_store_pg::connect(&cfg.database_url).await?;

    // Raw providers, no quota wrapping — this is a manual dev-tooling CLI,
    // not the multi-tenant production HTTP server; there is no shared budget
    // to enforce or actor to attribute spend to.
    let shutdown = tokio_util::sync::CancellationToken::new();
    let embedder: Arc<dyn EmbeddingProvider> =
        Arc::from(akashic_embed::build_provider(&cfg, shutdown.clone()).await?);
    let llm: Arc<dyn LlmProvider> =
        Arc::from(akashic_llm::build_llm_provider(&cfg.llm, shutdown.clone())?);

    let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
    let (event_tx, _rx) = tokio::sync::broadcast::channel(16);
    let pipeline = IngestionPipeline::new(pg, neo4j, embedder, llm, cfg, semaphore, event_tx);

    let req = IngestRequest {
        repo_name: repo_name.clone(),
        git_ref,
        source: "local".to_string(),
        local_path,
        user_token: None,
        seed_url: None,
        crawl_depth: None,
        url_pattern: None,
    };

    println!("akashic-ingest: starting repo='{repo_name}' until_stage={until_stage}");
    let summary = pipeline.run_sync(req, until_stage).await?;
    println!(
        "akashic-ingest: done. processed_files={} total_chunks={}",
        summary.processed_files, summary.total_chunks
    );
    Ok(())
}

async fn run_export(repo_name: String, out: String) -> anyhow::Result<()> {
    let cfg = akashic_config::Config::from_env()?;
    let neo4j = Neo4jPool::connect(&cfg).await?;
    let pg = akashic_store_pg::connect(&cfg.database_url).await?;

    let pg_snapshot_repo = PgSnapshotRepo::new(pg.clone());
    let graph_snapshot_repo = Neo4jSnapshotRepo::new(neo4j.clone());

    let pg_snapshot = pg_snapshot_repo.export_repo_snapshot(&repo_name).await?;
    let graph_snapshot = graph_snapshot_repo.export_repo_snapshot(&repo_name).await?;

    let meta = SnapshotMeta {
        schema_version: SCHEMA_VERSION,
        repo_name: repo_name.clone(),
        exported_at: chrono::Utc::now().to_rfc3339(),
    };

    write_archive(Path::new(&out), &meta, &pg_snapshot, &graph_snapshot)?;

    println!(
        "akashic-ingest export: repo='{}' modules={} chunks={} large_chunks={} communities={} \
         chunk_nodes={} calls_edges={} flows={} -> {}",
        repo_name,
        pg_snapshot.modules.len(),
        pg_snapshot.chunks.len(),
        pg_snapshot.large_chunks.len(),
        pg_snapshot.communities.len(),
        graph_snapshot.chunk_nodes.len(),
        graph_snapshot.calls_edges.len(),
        graph_snapshot.flows.len(),
        out
    );
    Ok(())
}

async fn run_import(file: String, repo_name: String) -> anyhow::Result<()> {
    let (meta, pg_snapshot, graph_snapshot) = read_archive(Path::new(&file))?;

    if meta.schema_version != SCHEMA_VERSION {
        anyhow::bail!(
            "snapshot schema_version {} does not match this CLI's version {SCHEMA_VERSION}",
            meta.schema_version
        );
    }
    if meta.repo_name != repo_name {
        anyhow::bail!(
            "snapshot was exported for repo '{}', but --repo-name is '{repo_name}' — \
             refusing to import under a different name",
            meta.repo_name
        );
    }

    let cfg = akashic_config::Config::from_env()?;
    let neo4j = Neo4jPool::connect(&cfg).await?;
    let pg = akashic_store_pg::connect(&cfg.database_url).await?;

    let module_repo = PgModuleRepo::new(pg.clone());
    if module_repo.count_modules(&repo_name).await? > 0 {
        anyhow::bail!(
            "repo '{repo_name}' already has data in the target Postgres — import only supports \
             a repo_name that does not yet exist there; use a different --repo-name or clean the \
             target first"
        );
    }
    let module_graph_repo = Neo4jModuleGraphRepo::new(neo4j.clone());
    if !module_graph_repo
        .get_module_nodes(&repo_name)
        .await?
        .is_empty()
    {
        anyhow::bail!(
            "repo '{repo_name}' already has Module nodes in the target Neo4j — import only \
             supports a repo_name that does not yet exist there"
        );
    }

    let pg_snapshot_repo = PgSnapshotRepo::new(pg.clone());
    pg_snapshot_repo
        .import_repo_snapshot(&repo_name, &pg_snapshot)
        .await?;

    akashic_ingestion::ingestion::snapshot_commit::import_graph_snapshot(
        &neo4j,
        &repo_name,
        &graph_snapshot,
    )
    .await?;

    println!(
        "akashic-ingest import: repo='{}' modules={} chunks={} large_chunks={} communities={} <- {file}",
        repo_name,
        pg_snapshot.modules.len(),
        pg_snapshot.chunks.len(),
        pg_snapshot.large_chunks.len(),
        pg_snapshot.communities.len(),
    );
    Ok(())
}

fn write_archive(
    out_path: &Path,
    meta: &SnapshotMeta,
    pg_snapshot: &akashic_domain::types::PgRepoSnapshot,
    graph_snapshot: &GraphRepoSnapshot,
) -> anyhow::Result<()> {
    let meta_json = serde_json::to_vec_pretty(meta)?;
    let pg_json = serde_json::to_vec(pg_snapshot)?;
    let graph_json = serde_json::to_vec(graph_snapshot)?;

    let file = std::fs::File::create(out_path)?;
    let encoder = zstd::Encoder::new(file, 3)?.auto_finish();
    let mut builder = tar::Builder::new(encoder);
    append_bytes(&mut builder, "meta.json", &meta_json)?;
    append_bytes(&mut builder, "pg_snapshot.json", &pg_json)?;
    append_bytes(&mut builder, "graph_snapshot.json", &graph_json)?;
    builder.into_inner()?;
    Ok(())
}

fn append_bytes<W: Write>(
    builder: &mut tar::Builder<W>,
    name: &str,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, name, bytes)?;
    Ok(())
}

fn read_archive(
    path: &Path,
) -> anyhow::Result<(
    SnapshotMeta,
    akashic_domain::types::PgRepoSnapshot,
    GraphRepoSnapshot,
)> {
    let file = std::fs::File::open(path)?;
    let decoder = zstd::Decoder::new(file)?;
    let mut archive = tar::Archive::new(decoder);

    let mut meta = None;
    let mut pg_snapshot = None;
    let mut graph_snapshot = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.to_path_buf();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        match entry_path.to_str() {
            Some("meta.json") => meta = Some(serde_json::from_slice(&buf)?),
            Some("pg_snapshot.json") => pg_snapshot = Some(serde_json::from_slice(&buf)?),
            Some("graph_snapshot.json") => graph_snapshot = Some(serde_json::from_slice(&buf)?),
            _ => {}
        }
    }

    Ok((
        meta.ok_or_else(|| anyhow::anyhow!("archive missing meta.json"))?,
        pg_snapshot.ok_or_else(|| anyhow::anyhow!("archive missing pg_snapshot.json"))?,
        graph_snapshot.ok_or_else(|| anyhow::anyhow!("archive missing graph_snapshot.json"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_minimal_ingest_args() {
        let cli = Cli::parse_from([
            "akashic-ingest",
            "ingest",
            "--repo-name",
            "my-repo",
            "--local-path",
            "/tmp/some-repo",
        ]);
        match cli.command {
            Commands::Ingest {
                repo_name,
                git_ref,
                local_path,
                until_stage,
            } => {
                assert_eq!(repo_name, "my-repo");
                assert_eq!(local_path.as_deref(), Some("/tmp/some-repo"));
                assert_eq!(git_ref, "main");
                assert_eq!(until_stage, 9);
            }
            _ => panic!("expected Commands::Ingest"),
        }
    }

    #[test]
    fn parses_until_stage_override() {
        let cli = Cli::parse_from([
            "akashic-ingest",
            "ingest",
            "--repo-name",
            "my-repo",
            "--local-path",
            "/tmp/some-repo",
            "--until-stage",
            "6",
        ]);
        match cli.command {
            Commands::Ingest { until_stage, .. } => assert_eq!(until_stage, 6),
            _ => panic!("expected Commands::Ingest"),
        }
    }

    #[test]
    fn rejects_until_stage_out_of_range() {
        let result = Cli::try_parse_from([
            "akashic-ingest",
            "ingest",
            "--repo-name",
            "my-repo",
            "--local-path",
            "/tmp/some-repo",
            "--until-stage",
            "0",
        ]);
        assert!(result.is_err(), "0 must be rejected");

        let result = Cli::try_parse_from([
            "akashic-ingest",
            "ingest",
            "--repo-name",
            "my-repo",
            "--local-path",
            "/tmp/some-repo",
            "--until-stage",
            "10",
        ]);
        assert!(result.is_err(), "10 must be rejected");
    }

    #[test]
    fn rejects_missing_local_path() {
        let result = Cli::try_parse_from(["akashic-ingest", "ingest", "--repo-name", "my-repo"]);
        assert!(
            result.is_err(),
            "missing --local-path must be rejected at parse time"
        );
    }

    #[test]
    fn parses_export_args() {
        let cli = Cli::parse_from([
            "akashic-ingest",
            "export",
            "--repo-name",
            "my-repo",
            "--out",
            "snap.tar.zst",
        ]);
        match cli.command {
            Commands::Export { repo_name, out } => {
                assert_eq!(repo_name, "my-repo");
                assert_eq!(out, "snap.tar.zst");
            }
            _ => panic!("expected Commands::Export"),
        }
    }

    #[test]
    fn parses_import_args() {
        let cli = Cli::parse_from([
            "akashic-ingest",
            "import",
            "--file",
            "snap.tar.zst",
            "--repo-name",
            "my-repo",
        ]);
        match cli.command {
            Commands::Import { file, repo_name } => {
                assert_eq!(file, "snap.tar.zst");
                assert_eq!(repo_name, "my-repo");
            }
            _ => panic!("expected Commands::Import"),
        }
    }

    #[test]
    fn snapshot_meta_round_trips_through_json() {
        let meta = SnapshotMeta {
            schema_version: SCHEMA_VERSION,
            repo_name: "my-repo".into(),
            exported_at: "2026-07-04T00:00:00+00:00".into(),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: SnapshotMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.repo_name, "my-repo");
        assert_eq!(back.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn archive_write_then_read_round_trips_empty_snapshots() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("akashic-e2-test-{}.tar.zst", std::process::id()));

        let meta = SnapshotMeta {
            schema_version: SCHEMA_VERSION,
            repo_name: "archive-test-repo".into(),
            exported_at: "2026-07-04T00:00:00+00:00".into(),
        };
        let pg_snapshot = akashic_domain::types::PgRepoSnapshot::default();
        let graph_snapshot = GraphRepoSnapshot::default();

        write_archive(&path, &meta, &pg_snapshot, &graph_snapshot).unwrap();
        let (read_meta, read_pg, read_graph) = read_archive(&path).unwrap();

        assert_eq!(read_meta.repo_name, "archive-test-repo");
        assert_eq!(read_meta.schema_version, SCHEMA_VERSION);
        assert!(read_pg.modules.is_empty());
        assert!(read_graph.chunk_nodes.is_empty());

        std::fs::remove_file(&path).ok();
    }
}
