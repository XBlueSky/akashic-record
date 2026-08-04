use std::sync::Arc;

use anyhow::Result;
use sqlx::PgPool;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};
use uuid::Uuid;

use akashic_config::Config;
use akashic_domain::ports::{
    ChunkGraphRepo, ChunkRepo, CommunityGraphRepo, CommunityRepo, EdgeRepo, IngestEdgeRepo,
    IngestionJobRepo, ModuleGraphRepo, ModuleRepo, NoteHealthRepo, NoteRepo, RepoGraphRepo,
    SymbolRepo,
};
use akashic_embed::EmbeddingProvider;
use akashic_kernel::AppEvent;
use akashic_llm::LlmProvider;
use akashic_store_neo4j::{
    Neo4jChunkGraphRepo, Neo4jCommunityGraphRepo, Neo4jEdgeRepo, Neo4jIngestEdgeRepo,
    Neo4jModuleGraphRepo, Neo4jPool, Neo4jRepoGraphRepo,
};
use akashic_store_pg::{
    PgChunkRepo, PgCommunityRepo, PgIngestionJobRepo, PgModuleRepo, PgNoteHealthRepo, PgNoteRepo,
    PgSymbolRepo,
};

use super::corpus::CorpusIngestService;
use super::corpus_pull;
use super::stages;
use akashic_quota::{CURRENT_ACTOR, system_ingestion_actor};

/// The minimum extractable root-text length (chars) for a generic-fallback site
/// to be considered crawlable rather than an empty JS shell.
const MIN_PROBE_TEXT: usize = 200;

/// Verdict of a submission probe.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub classification: String, // "good" | "generic" | "unsupported" | "unprobed"
    pub adapter_id: Option<String>,
    pub note: String,
}

/// Pure classification: given the adapter the resolver picked for `root_html`,
/// decide the verdict. A specific adapter (not the generic fallback) is always
/// `good` — its root may be a thin JS shell but its bulk path ingests fine.
fn classify(adapter_id: &str, root_html: &str) -> ProbeOutcome {
    if adapter_id != "generic-web" {
        return ProbeOutcome {
            classification: "good".into(),
            adapter_id: Some(adapter_id.to_string()),
            note: format!("matched adapter {adapter_id}"),
        };
    }
    let text_len = crate::ingestion::crawler::html_to_markdown(root_html)
        .trim()
        .chars()
        .count();
    if text_len >= MIN_PROBE_TEXT {
        ProbeOutcome {
            classification: "generic".into(),
            adapter_id: Some("generic-web".into()),
            note: format!("generic per-page crawl ({text_len} chars of root content)"),
        }
    } else {
        ProbeOutcome {
            classification: "unsupported".into(),
            adapter_id: None,
            note: "JS-rendered or empty root; no matching adapter".into(),
        }
    }
}

use super::store::IngestionStore;
use crate::ingestion::adapter::generic_web::GenericWebAdapter;
use crate::ingestion::adapter::gitbook_shelf::GitbookShelfAdapter;
use crate::ingestion::adapter::gitbook_static::GitbookStaticAdapter;
use crate::ingestion::adapter::mediawiki::MediaWikiAdapter;
use crate::ingestion::adapter::registry::PresetRegistry;
use crate::ingestion::adapter::resolver::AdapterResolver;
use crate::ingestion::adapter::vitepress_llms::VitepressLlmsAdapter;

/// Request to start an ingestion job.
#[derive(Debug, Clone)]
pub struct IngestRequest {
    pub repo_name: String,
    pub git_ref: String,
    pub source: String, // "gitlab", "local", or "website"
    pub local_path: Option<String>,
    pub user_token: Option<String>,
    pub seed_url: Option<String>,    // for website crawl
    pub crawl_depth: Option<u8>,     // for website crawl
    pub url_pattern: Option<String>, // for website crawl
}

/// Builds the ratio-gate abort message for `run_inner`'s `anyhow::bail!`
/// (review finding 4, Task 7 fix-up). Pulled into a plain function — instead
/// of an inline `format!` at the `bail!` call site — purely so its exact
/// text is unit-testable without spinning up the full pipeline: this
/// message MUST include the failed file paths, not just a count, because
/// `start()`'s top-level `tokio::spawn` error handler unconditionally
/// overwrites whatever `run_inner`'s own (richer, path-including)
/// `update_job_status` call just wrote with `e.to_string()` from this same
/// bail!. Whichever `update_job_status` call ends up being the last one to
/// actually land in the `jobs` row, the stored `error_message` must still
/// carry the useful detail.
fn ratio_gate_bail_message(
    ratio: f64,
    threshold: f64,
    failed_count: usize,
    total_files: i32,
    failed_paths_joined: &str,
) -> String {
    format!(
        "ingest aborted: completeness {ratio:.4} below threshold {threshold:.4} \
         ({failed_count} of {total_files} files failed); failed files: {failed_paths_joined}"
    )
}

/// Summary returned by [`IngestionPipeline::run_sync`] once the run completes.
#[derive(Debug, Clone, Copy)]
pub struct JobSummary {
    pub processed_files: i32,
    pub total_chunks: i32,
}

/// The ingestion pipeline orchestrator.
#[derive(Clone)]
pub struct IngestionPipeline {
    pg: PgPool,
    neo4j: Neo4jPool,
    embedder: Arc<dyn EmbeddingProvider>,
    llm: Arc<dyn LlmProvider>,
    config: Config,
    semaphore: Arc<Semaphore>,
    event_tx: tokio::sync::broadcast::Sender<AppEvent>,
    /// Port-backed repo for job creation/status and job-lineage bookkeeping
    /// (`create_job_resumed`). Kept on pipeline (not IngestionStore) because
    /// `start_resume` needs it before `make_store()` is called.
    job_repo: Arc<dyn IngestionJobRepo>,
    /// Port-backed repo for MERGE Repository node (Stage 3).
    repo_graph: Arc<dyn RepoGraphRepo>,
    /// Resolves the SiteAdapter for a website source: registry pin → sniff →
    /// generic. Built once here; the adapter set grows in A2b/c.
    resolver: AdapterResolver,
    /// Task 9 (B4): the pull-bootstrap corpus service, wired in via
    /// [`Self::with_corpus_ingest_service`]. `None` for every construction
    /// path that doesn't opt in (most tests) — `run_inner` simply skips the
    /// `.akashic/docs.toml` detection step when this is unset, rather than
    /// requiring every one of this crate's ~30 `IngestionPipeline::new` call
    /// sites to supply one.
    corpus_ingest_service: Option<Arc<CorpusIngestService>>,
}

impl IngestionPipeline {
    pub fn new(
        pg: PgPool,
        neo4j: Neo4jPool,
        embedder: Arc<dyn EmbeddingProvider>,
        llm: Arc<dyn LlmProvider>,
        config: Config,
        semaphore: Arc<Semaphore>,
        event_tx: tokio::sync::broadcast::Sender<AppEvent>,
    ) -> Self {
        let job_repo: Arc<dyn IngestionJobRepo> = Arc::new(PgIngestionJobRepo::new(pg.clone()));
        let repo_graph: Arc<dyn RepoGraphRepo> = Arc::new(Neo4jRepoGraphRepo::new(neo4j.clone()));
        let resolver = AdapterResolver::new(
            vec![
                Arc::new(GitbookStaticAdapter::new()),
                Arc::new(VitepressLlmsAdapter::new()),
                Arc::new(MediaWikiAdapter::new()),
                Arc::new(GitbookShelfAdapter::new()),
            ],
            Arc::new(GenericWebAdapter::new()),
            // Startup fail-fast: a broken operator presets file must not be
            // silently ignored (the deployment would crawl with wrong adapters).
            PresetRegistry::load_merged(
                config
                    .ingest_presets_path
                    .as_deref()
                    .map(std::path::Path::new),
            )
            .unwrap_or_else(|e| panic!("invalid runtime presets file (AKASHIC_PRESETS_PATH): {e}")),
        );
        Self {
            pg,
            neo4j,
            embedder,
            llm,
            config,
            semaphore,
            event_tx,
            job_repo,
            repo_graph,
            resolver,
            corpus_ingest_service: None,
        }
    }

    /// Wire in the pull-bootstrap corpus service (Task 9, B4). Optional —
    /// composition roots that care about `.akashic/docs.toml` detection
    /// (main.rs) call this after `new()`; everything else leaves it unset
    /// and `run_inner` skips the step entirely.
    #[must_use]
    pub fn with_corpus_ingest_service(mut self, service: Arc<CorpusIngestService>) -> Self {
        self.corpus_ingest_service = Some(service);
        self
    }

    /// Build an [`IngestionStore`] from the pipeline's pools, wiring the
    /// concrete PG and Neo4j adapters for all port-backed repos (T3, T6, T8).
    fn make_store(&self) -> IngestionStore {
        let chunks: Arc<dyn ChunkRepo> = Arc::new(PgChunkRepo::new(self.pg.clone()));
        let chunk_graph: Arc<dyn ChunkGraphRepo> =
            Arc::new(Neo4jChunkGraphRepo::new(self.neo4j.clone()));
        let modules: Arc<dyn ModuleRepo> = Arc::new(PgModuleRepo::new(self.pg.clone()));
        let module_graph: Arc<dyn ModuleGraphRepo> =
            Arc::new(Neo4jModuleGraphRepo::new(self.neo4j.clone()));
        let community: Arc<dyn CommunityRepo> = Arc::new(PgCommunityRepo::new(self.pg.clone()));
        let community_graph: Arc<dyn CommunityGraphRepo> =
            Arc::new(Neo4jCommunityGraphRepo::new(self.neo4j.clone()));
        // T8: job/edge/note adapters
        let job_repo = self.job_repo.clone();
        let ingest_edge: Arc<dyn IngestEdgeRepo> =
            Arc::new(Neo4jIngestEdgeRepo::new(self.neo4j.clone()));
        let note: Arc<dyn NoteRepo> = Arc::new(PgNoteRepo::new(self.pg.clone()));
        let note_health: Arc<dyn NoteHealthRepo> = Arc::new(PgNoteHealthRepo::new(self.pg.clone()));
        IngestionStore::new(
            chunks,
            chunk_graph,
            modules,
            module_graph,
            community,
            community_graph,
            job_repo,
            ingest_edge,
            note,
            note_health,
            self.embedder.clone(),
            self.event_tx.clone(),
        )
    }

    /// Re-establish note→code `ATTACHED_TO` edges after a production reingest.
    ///
    /// The production commit wipes and re-creates the repo's Chunk/Module nodes
    /// with brand-new UUIDs, so every `ATTACHED_TO` edge a note built at save
    /// time is gone. `link_note_to_chunks` matches notes to chunks by symbol
    /// NAME (not UUID), so replaying it for each note in the repo re-links the
    /// curated note→code associations against the fresh chunks. Per-note
    /// failures are logged and skipped: the reingest already committed, so a
    /// relink hiccup must not fail the whole job.
    async fn relink_notes_after_reingest(&self, repo_name: &str) -> Result<()> {
        let note_repo: Arc<dyn NoteRepo> = Arc::new(PgNoteRepo::new(self.pg.clone()));
        let symbol_repo: Arc<dyn SymbolRepo> = Arc::new(PgSymbolRepo::new(self.pg.clone()));
        let edge_repo: Arc<dyn EdgeRepo> = Arc::new(Neo4jEdgeRepo::new(self.neo4j.clone()));

        let notes = note_repo.get_all_with_symbols(repo_name).await?;
        let mut relinked = 0usize;
        for n in &notes {
            match akashic_retrieval::linking::link_note_to_chunks(
                &symbol_repo,
                &edge_repo,
                &n.id.to_string(),
                repo_name,
                &n.symbols,
            )
            .await
            {
                Ok(()) => relinked += 1,
                Err(e) => {
                    warn!(repo = repo_name, note_id = %n.id, err = %e, "note→code relink failed");
                }
            }
        }
        info!(
            repo = repo_name,
            notes = notes.len(),
            relinked,
            "Re-linked notes to code after reingest"
        );
        Ok(())
    }

    /// Re-cluster a repo's doc sections (Leiden + LLM topic naming). Backs the
    /// `POST /api/v1/repos/:name/relink-explains` clustering half, which used to
    /// build `DocClustering` handler-side from AppState's pg/db/llm.
    pub async fn recluster_doc_sections(&self, repo_name: &str) -> Result<usize> {
        let clustering = crate::ingestion::doc_clustering::DocClustering::new(
            self.pg.clone(),
            self.neo4j.clone(),
            self.llm.clone(),
        );
        clustering.cluster_repo_sections(repo_name).await
    }

    /// Start an ingestion job. Returns the job ID immediately; work runs in background.
    pub async fn start(&self, req: IngestRequest) -> Result<Uuid> {
        let store = self.make_store();

        let job_id = store.create_job(&req.repo_name, &req.git_ref).await?;
        info!(job_id = %job_id, repo = %req.repo_name, "Ingestion job created");

        let pipeline = self.clone();
        let req = req.clone();

        tokio::spawn(async move {
            let _permit = match pipeline.semaphore.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    error!(job_id = %job_id, "Semaphore closed");
                    return;
                }
            };

            if let Err(e) = pipeline.run(job_id, &req).await {
                error!(job_id = %job_id, err = %e, "Ingestion failed");
                let store = pipeline.make_store();
                let _ = store
                    .update_job_status(
                        job_id,
                        &req.repo_name,
                        "failed",
                        None,
                        None,
                        Some(&e.to_string()),
                    )
                    .await;
            }
        });

        Ok(job_id)
    }

    async fn run(&self, job_id: Uuid, req: &IngestRequest) -> Result<()> {
        self.run_inner(job_id, req, false, 9).await.map(|_| ())
    }

    /// Run the pipeline synchronously (no `tokio::spawn`) and return once it
    /// completes — unlike `start()`, which spawns the work in the background
    /// and returns a job id immediately for separate HTTP polling. Intended
    /// for the standalone ingest CLI, which has no polling mechanism and
    /// wants to block until done.
    ///
    /// `until_stage` (1-9) stops the run after that stage; Stages 1-6 always
    /// run unconditionally today (only Stages 7/8/9 are individually gated —
    /// see `run_inner`'s doc comment for why finer-grained stopping isn't
    /// supported). Pass `9` for the full pipeline.
    ///
    /// Always runs with `preserve_existing = true`: unlike the production
    /// `start()`/`start_resume()` paths (which always wipe old data at the
    /// commit gate below), `run_sync` NEVER wipes existing chunks — this is
    /// what makes Stage 4's content-match skip (Roadmap E1 Task 2) possible
    /// on a re-run against an unchanged source tree. A genuinely CHANGED
    /// source tree (files deleted/renamed since the last run_sync) will
    /// accumulate stale orphaned chunks under this mode — an accepted,
    /// documented limitation for this CLI's validated use case (repeated
    /// measurement runs against an UNCHANGED checkout), not a concern for the
    /// production HTTP path, which this does not affect.
    ///
    /// (Roadmap F) `preserve_existing` is no longer about checkpoint/resume —
    /// that machinery is retired entirely, and Stage 4's existing-chunk fetch
    /// (the content-match skip's read side) is unconditionally safe for
    /// EVERY caller now, since nothing is cleaned until the commit gate runs
    /// strictly after every stage's resolution is done (see `run_inner`).
    /// `preserve_existing` survives purely to gate that commit-gate wipe:
    /// production wants it (a fresh, complete graph every run), `run_sync`
    /// does not (it wants successive measurement runs to keep reusing
    /// byte-matched rows instead of losing them to a blind repo-wide wipe).
    pub async fn run_sync(&self, req: IngestRequest, until_stage: u8) -> Result<JobSummary> {
        let store = self.make_store();
        let job_id = store.create_job(&req.repo_name, &req.git_ref).await?;
        info!(job_id = %job_id, repo = %req.repo_name, until_stage, "Synchronous ingestion started");
        self.run_inner(job_id, &req, true, until_stage).await
    }

    async fn run_inner(
        &self,
        job_id: Uuid,
        req: &IngestRequest,
        preserve_existing: bool,
        until_stage: u8,
    ) -> Result<JobSummary> {
        // A2d-4: attribute all embedding spend during this run to the synthetic
        // system ingestion actor (user_id=-1). When `ingest_quota_enabled=false`
        // (the default), spend is recorded for visibility only and never blocked.
        // When enabled, the ingest_quota budget is enforced.
        CURRENT_ACTOR
            .scope(Some(system_ingestion_actor()), async move {
                let store = self.make_store();

                if req.source == "website" {
                    let pages =
                        stages::crawl_pages(&store, job_id, req, &self.config, &self.resolver)
                            .await?;
                    return stages::ingest_pages(
                        &store,
                        &self.pg,
                        &self.neo4j,
                        &self.embedder,
                        &self.llm,
                        job_id,
                        req,
                        pages,
                    )
                    .await
                    .map(|()| JobSummary {
                        processed_files: 0,
                        total_chunks: 0,
                    });
                }

                let repo_dir = stages::clone_or_crawl(&store, job_id, req, &self.config).await?;

                // ════════════════════════════════════════════════════════════
                // Task 9 (B4): pull-bootstrap corpus ingest — independent of
                // code analysis. Placed strictly BEFORE Stage 2 (and thus
                // before the completeness-ratio commit gate further down)
                // so it always runs once per clone regardless of what the
                // code path decides later; `maybe_ingest_repo_docs` already
                // catches and logs every failure mode itself (missing
                // contract, parse error, rejected tree), so this step never
                // propagates an `Err` up into `run_inner` and can never
                // abort code analysis.
                // ════════════════════════════════════════════════════════════
                if let Some(corpus_svc) = &self.corpus_ingest_service {
                    match corpus_pull::head_sha_of(&repo_dir) {
                        Some(sha) => {
                            corpus_pull::maybe_ingest_repo_docs(
                                corpus_svc,
                                &repo_dir,
                                &req.repo_name,
                                &sha,
                            )
                            .await;
                        }
                        None => {
                            info!(
                                job_id = %job_id,
                                repo = %req.repo_name,
                                "no git HEAD in checkout; skipping pull-bootstrap corpus ingest"
                            );
                        }
                    }
                }

                // ════════════════════════════════════════════════════════════
                // Stage 2: Analyze
                // ════════════════════════════════════════════════════════════
                store
                    .update_job_status(job_id, &req.repo_name, "analyzing", None, None, None)
                    .await?;

                let files = stages::analyze(&repo_dir, &self.config)?;

                let total_files = files.len() as i32;
                store.set_total_files(job_id, total_files).await?;
                info!(job_id = %job_id, total_files, "Analysis complete");

                // ════════════════════════════════════════════════════════════
                // Stage 3: Parse & Chunk ALL files (single pass)
                // ════════════════════════════════════════════════════════════
                store
                    .update_job_status(job_id, &req.repo_name, "parsing", None, None, None)
                    .await?;

                // Repo-wide cleanup is NOT done here (Roadmap F, Task 8): it used
                // to run at this point for every fresh, non-resumed ingest, but
                // that made Stage 4's content-match skip fetch see an already-
                // wiped repo for the production path — pure waste, since nothing
                // is written for real until the commit gate anyway. Cleanup now
                // happens ATOMICALLY as part of the commit (Roadmap F Task 9.5):
                // the production path's PG wipe is folded into
                // `import_repo_snapshot_replacing`'s transaction and its Neo4j
                // wipe runs after that tx commits — see `commit_snapshot`'s
                // production branch for the full rationale and for why
                // `preserve_existing` gates whether the repo is replaced at all.

                // Ensure Repository node exists in Neo4j via RepoGraphRepo port.
                self.repo_graph
                    .ensure_repository_node(&req.repo_name)
                    .await?;

                let (final_modules, virtual_paths, failed_files) =
                    stages::chunk_and_group(job_id, files, &self.config, &self.llm).await?;
                let ratio = stages::completeness_ratio(total_files as usize, &failed_files);
                info!(job_id = %job_id, completeness_ratio = ratio, failed_files = failed_files.len(), "Stage 3 complete");

                // ════════════════════════════════════════════════════════════
                // Stage 4: Resolve & Accumulate (Roadmap F — RAM-first ingest)
                // ════════════════════════════════════════════════════════════
                let mut acc = crate::ingestion::accumulator::IngestAccumulator::new();
                let s4 = stages::stage4_embed_store(
                    &store,
                    job_id,
                    req,
                    &final_modules,
                    &virtual_paths,
                    &mut acc,
                    preserve_existing,
                )
                .await?;
                let all_imports = s4.all_imports;
                let processed_files = s4.processed_files;
                let total_chunks = s4.total_chunks;

                // ════════════════════════════════════════════════════════════
                // Stage 5: Create IMPORTS_FROM edges (using final module paths)
                // ════════════════════════════════════════════════════════════
                let s5 = stages::stage5_import_edges(
                    job_id,
                    req,
                    &repo_dir,
                    &final_modules,
                    all_imports,
                    &mut acc,
                )
                .await?;

                // ════════════════════════════════════════════════════════════
                // Stage 6: Create CALLS edges
                // ════════════════════════════════════════════════════════════
                stages::stage6_call_edges(job_id, req, &final_modules, &s5, &mut acc).await?;

                // Stages 7-9 are individually gated by `until_stage` — a
                // recall-measurement run (`run_sync(req, 6)`) stops here,
                // skipping the LLM-calling community stage and the
                // flows/staleness bookkeeping irrelevant to CALLS.method
                // measurement. Stages 1-6 always run unconditionally: they
                // regenerate the in-memory FinalModules/Stage5Output Stage 6
                // itself consumes, so there's no meaningful earlier stopping
                // point without a much larger redesign (documented Non-Goal).
                if until_stage >= 7 {
                    stages::stage7_flows(job_id, req, &mut acc).await?;
                }

                // ════════════════════════════════════════════════════════════
                // Commit gate (Roadmap F): below-threshold completeness means
                // zero database writes, full stop — no partial graph ever
                // lands. Above threshold, the accumulator commits atomically
                // via the SAME machinery Roadmap E2's snapshot import uses.
                //
                // Review finding 3 (Task 7 fix-up) is RESOLVED as of Task 8:
                // that finding was that this gate ran AFTER a Stage-3 wipe
                // which always fired for every production run (gated on
                // `checkpoint.is_none() && !preserve_existing`, unconditionally
                // true for `start()`/`start_resume()`), so a below-threshold
                // abort here didn't just skip landing the new graph — it
                // landed on top of a repo Stage 3 had ALREADY wiped, losing
                // the last-known-good graph too. Task 8 deleted that early
                // Stage-3 wipe outright (it was a leftover duplicate of the
                // one below, which is the only one that should ever have
                // existed — see its own comment). With no wipe before this
                // point, a below-threshold `bail!` below now aborts BEFORE
                // any repo data is ever touched, so the prior committed graph
                // (if any) survives intact. No behavior to patch here.
                // ════════════════════════════════════════════════════════════
                if ratio < self.config.ingest_completeness_threshold {
                    let failed_paths: Vec<String> =
                        failed_files.iter().map(|f| f.path.clone()).collect();
                    let failed_paths_joined = failed_paths.join(", ");
                    store
                        .update_job_status(
                            job_id,
                            &req.repo_name,
                            "failed",
                            Some(processed_files),
                            Some(total_chunks),
                            Some(&format!(
                                "completeness {ratio:.4} below threshold {:.4}; failed files: {}",
                                self.config.ingest_completeness_threshold,
                                failed_paths_joined
                            )),
                        )
                        .await?;
                    // Review finding 4 (Task 7 fix-up): `start()`'s top-level
                    // `tokio::spawn` error handler calls `update_job_status`
                    // a SECOND time with `e.to_string()` from this bail!,
                    // unconditionally overwriting whatever `error_message`
                    // the call just above wrote. See `ratio_gate_bail_message`
                    // for why its text must carry the failed file paths.
                    anyhow::bail!(ratio_gate_bail_message(
                        ratio,
                        self.config.ingest_completeness_threshold,
                        failed_paths.len(),
                        total_files,
                        &failed_paths_joined,
                    ));
                }

                // Commit the accumulator. The repo wipe is NOT a standalone
                // step here anymore (Roadmap F Task 9.5): the production path's
                // PG wipe now lives INSIDE `import_repo_snapshot_replacing` (one
                // transaction with the insert, so a crash rolls the wipe back
                // instead of destroying the prior graph), and its Neo4j wipe
                // lives inside `commit_snapshot` STRICTLY AFTER that PG tx
                // commits. `preserve_existing` selects the strategy inside
                // `commit_snapshot`: production (`false`) REPLACES the repo;
                // `run_sync` (`true`) preserves byte-matched rows and never
                // wipes (its `acc` is delta-only — a byte-matched chunk's row
                // exists ONLY in the database, never in `acc` — so a wipe would
                // permanently lose it; `e2e_run_sync_preserves_existing_chunks_across_calls`
                // and `e2e_run_sync_no_duplicate_chunks_after_second_call`
                // guard that). The ratio-gate `bail!` above still runs strictly
                // BEFORE this, so a below-threshold abort touches nothing.
                crate::ingestion::snapshot_commit::commit_snapshot(
                    &self.pg,
                    &self.neo4j,
                    &req.repo_name,
                    preserve_existing,
                    acc,
                )
                .await?;

                // The production commit wiped this repo's chunk/module nodes and
                // minted new UUIDs, destroying the curated note→code ATTACHED_TO
                // edges. Re-link them (by symbol name) against the fresh chunks.
                // Best-effort: the reingest already committed, so a relink
                // failure warns but does not fail the job. (run_sync preserves
                // existing nodes, so its edges are never wiped — skip it.)
                if !preserve_existing
                    && let Err(e) = self.relink_notes_after_reingest(&req.repo_name).await
                {
                    warn!(job_id = %job_id, repo = %req.repo_name, err = %e, "note→code relink after reingest failed");
                }

                if until_stage >= 8 {
                    stages::stage8_note_staleness(&store, job_id, req).await?;
                }
                if until_stage >= 9 {
                    stages::stage9_communities(&store, &self.llm, &self.embedder, job_id, req)
                        .await?;
                }

                // ════════════════════════════════════════════════════════════
                // Done
                // ════════════════════════════════════════════════════════════
                store
                    .update_job_status(
                        job_id,
                        &req.repo_name,
                        "done",
                        Some(processed_files),
                        Some(total_chunks),
                        None,
                    )
                    .await?;

                if req.local_path.is_none() {
                    let _ = std::fs::remove_dir_all(&repo_dir);
                }

                info!(
                    job_id = %job_id,
                    repo = %req.repo_name,
                    processed_files,
                    total_chunks,
                    "Ingestion complete"
                );

                Ok(JobSummary {
                    processed_files,
                    total_chunks,
                })
            })
            .await
    }

    /// Probe a seed URL: best-effort fetch the root, resolve an adapter, classify.
    /// A fetch failure is `unprobed` (NOT a rejection) — an internal host may need
    /// an admin grant; the admin still gets it in the queue.
    pub async fn probe_source(&self, seed_url: &str) -> ProbeOutcome {
        let client = crate::ingestion::crawler::build_crawl_client(15);
        let root_html = match crate::ingestion::crawler::fetch_page(&client, seed_url).await {
            Ok(h) => h,
            Err(e) => {
                return ProbeOutcome {
                    classification: "unprobed".into(),
                    adapter_id: None,
                    note: format!("probe fetch failed: {e}"),
                };
            }
        };
        let ctx = crate::ingestion::adapter::ProbeContext {
            seed_url: seed_url.to_string(),
            root_html: root_html.clone(),
        };
        let adapter = self.resolver.resolve(&ctx).await;
        classify(adapter.id(), &root_html)
    }

    /// "Resume" a previously failed ingestion job (Roadmap F: this is now a
    /// full fresh re-run, not a checkpoint replay — RAM-first crash-
    /// consistency means a failed run left NOTHING durable to resume FROM.
    /// The HTTP route (`POST /api/v1/repos/{name}/resume`) and its frontend
    /// caller are unchanged; only this internal behavior changed. Job-lineage
    /// bookkeeping (`resumed_from`) is preserved for audit purposes even
    /// though there's no longer any incremental work being resumed).
    pub async fn start_resume(&self, req: IngestRequest, prev_job_id: Uuid) -> Result<Uuid> {
        let job_id = self
            .job_repo
            .create_job_resumed(&req.repo_name, &req.git_ref, prev_job_id)
            .await?;

        info!(job_id = %job_id, resumed_from = %prev_job_id, "Resumed ingestion job created (full re-run)");

        let pipeline = self.clone();

        tokio::spawn(async move {
            let _permit = match pipeline.semaphore.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    error!(job_id = %job_id, "Semaphore closed");
                    return;
                }
            };

            if let Err(e) = pipeline.run(job_id, &req).await {
                error!(job_id = %job_id, err = %e, "Resumed ingestion failed");
                let store = pipeline.make_store();
                let _ = store
                    .update_job_status(
                        job_id,
                        &req.repo_name,
                        "failed",
                        None,
                        None,
                        Some(&e.to_string()),
                    )
                    .await;
            }
        });

        Ok(job_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_specific_adapter_is_good_regardless_of_root() {
        // A thin JS-shell root, but a specific adapter matched → good (not unsupported).
        let out = classify(
            "vitepress-llms",
            "<html><body><div id=\"app\"></div></body></html>",
        );
        assert_eq!(out.classification, "good");
        assert_eq!(out.adapter_id.as_deref(), Some("vitepress-llms"));
    }

    #[test]
    fn classify_generic_with_real_text_is_generic() {
        let body = format!(
            "<html><body><p>{}</p></body></html>",
            "lorem ipsum ".repeat(40)
        );
        let out = classify("generic-web", &body);
        assert_eq!(out.classification, "generic");
        assert_eq!(out.adapter_id.as_deref(), Some("generic-web"));
    }

    #[test]
    fn classify_generic_empty_root_is_unsupported() {
        let out = classify(
            "generic-web",
            "<html><body><div id=\"app\"></div></body></html>",
        );
        assert_eq!(out.classification, "unsupported");
        assert!(out.adapter_id.is_none());
    }

    /// Review finding 4 (Task 7 fix-up) regression guard: the ratio-gate's
    /// `anyhow::bail!` message (built by `ratio_gate_bail_message`, called
    /// verbatim from the real `run_inner` call site) must carry the failed
    /// file paths themselves, not just a count — since `start()`'s
    /// top-level error handler clobbers the earlier, richer
    /// `update_job_status` call with exactly this string.
    #[test]
    fn ratio_gate_bail_message_includes_failed_file_paths() {
        let failed_paths = ["src/broken_a.rs".to_string(), "src/broken_b.rs".to_string()];
        let joined = failed_paths.join(", ");

        let msg = ratio_gate_bail_message(0.5, 1.0, failed_paths.len(), 4, &joined);

        assert!(
            msg.contains("src/broken_a.rs") && msg.contains("src/broken_b.rs"),
            "bail message must include the failed file paths, not just a count: {msg}"
        );
        assert!(
            msg.contains("2 of 4 files failed"),
            "bail message must still include the failed/total summary count: {msg}"
        );
    }
}
