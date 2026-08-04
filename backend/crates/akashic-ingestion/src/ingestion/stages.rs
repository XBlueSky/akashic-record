//! Compute stages of the ingestion pipeline (Stages 1–3): clone/crawl, website
//! ingest, file analysis, and parse → chunk → module-grouping. These are
//! `self`-free free functions carved out of `IngestionPipeline::run_inner`.
//! Originally carved so the orchestrator kept only a checkpoint/compensation
//! tail (Stages 4–9); that tail is gone (Roadmap F retired checkpoint/resume
//! entirely — Stages 4–9 are RAM-first resolve-only/accumulator-based now),
//! but this module's own split (Stages 1–3 here, 4–9 in `pipeline.rs`) is
//! unchanged.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use secrecy::ExposeSecret;
use sqlx::PgPool;
use tracing::{debug, info, warn};
use uuid::Uuid;

use akashic_config::Config;
use akashic_domain::types::{ChunkRow, LargeChunkSnapshotRow, ModuleSnapshotRow};
use akashic_embed::EmbeddingProvider;
use akashic_extraction::resolution::{ChunkIndex, ImportMap, ResolvedEdge, resolve_call_edge};
use akashic_extraction::special::sections;
use akashic_extraction::{EdgeEndpoint, EdgeKind, MetadataValue, RawEdge};
use akashic_llm::LlmProvider;
use akashic_store_neo4j::Neo4jPool;

use super::adapter::resolver::AdapterResolver;
use super::adapter::{CrawlLimits, ExtractedPage, ProbeContext, SourceSpec};
use super::chunker;
use super::clone;
use super::crawler;
use super::doc_clustering::DocClustering;
use super::doc_store::DocStore;
use super::file_discovery::{self, AnalyzedFile, extractor_name};
use super::pipeline::IngestRequest;
use super::store::IngestionStore;
use super::virtual_modules;

/// A file that has been read and chunked, ready for module grouping.
///
/// `chunks` and `edges` are the output of a single declarative extraction pass
/// (`chunker::extract_file`); stashing the edges here lets the import (Stage 5)
/// and call (Stage 6) stages resolve references without re-parsing the source.
///
/// Owns its `AnalyzedFile` (rather than borrowing) so `chunk_and_group` can own
/// `files` and return the grouped modules across the function boundary.
pub(crate) struct ParsedFile {
    pub(crate) file: AnalyzedFile,
    pub(crate) chunks: Vec<akashic_extraction::RawChunk>,
    pub(crate) edges: Vec<RawEdge>,
}

/// One file that failed to read or parse during Stage 3. Tracked (not just
/// logged) so the commit gate (Task 7) can compute a completeness ratio and
/// report exactly which files were skipped.
///
/// `path`/`reason` are populated starting in Task 1 but only *read* starting
/// with Task 3's orchestration (Roadmap F): only `.len()` of the containing
/// `Vec<FailedFile>` is consumed today. `#[allow(dead_code)]` is a deliberate,
/// temporary task-boundary artifact rather than an underscore-prefix, because
/// the field names are a pinned interface Task 3/Task 7 consume verbatim.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct FailedFile {
    pub path: String,
    pub reason: String,
}

/// Fraction of `total_files` that were NOT in `failed` — the crash-consistency
/// commit gate's go/no-go signal. `0` total files is defined as fully
/// complete (nothing was attempted, nothing failed), avoiding a `0/0` NaN.
pub(crate) fn completeness_ratio(total_files: usize, failed: &[FailedFile]) -> f64 {
    if total_files == 0 {
        return 1.0;
    }
    (total_files - failed.len()) as f64 / total_files as f64
}

/// Module path → its files, after roll-up + LLM split + post-split roll-up.
pub(crate) type FinalModules = HashMap<String, Vec<ParsedFile>>;
/// The set of module paths that are LLM-created virtual sub-modules.
pub(crate) type VirtualPaths = HashSet<String>;

/// Derive `(source_module, raw_target)` import pairs from a parsed file's edges,
/// reproducing the shape the old `chunker::extract_file_imports` returned.
///
/// Import edges carry the file-derived current-module on their source side and
/// the canonical resolved target in `target.module_specifier`. The old import
/// extractor always used the *virtual* `module_path` as the source, so we
/// override the source side with `module_path` here. The raw target is the
/// resolved `module_specifier` (falling back to the bare specifier name when no
/// canonical form was produced), which the Stage-5 resolver maps to a module.
pub(crate) fn imports_from_edges(edges: &[RawEdge], module_path: &str) -> Vec<(String, String)> {
    edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import)
        .filter_map(|e| match &e.target {
            EdgeEndpoint::Name {
                name,
                module_specifier,
            } => {
                let raw_target = module_specifier.clone().unwrap_or_else(|| name.clone());
                Some((module_path.to_string(), raw_target))
            }
            _ => None,
        })
        .collect()
}

/// Resolve a raw import target (a module specifier as the extractor produced it)
/// to a final virtual module path, using the file/dir indexes built in Stage 5.
/// Returns `None` when the target cannot be mapped to a known module.
///
/// Shared by the IMPORTS_FROM module-edge resolution and the per-symbol import_map
/// (EXT-3b): both need the same specifier→module mapping.
pub(crate) fn resolve_import_target(
    raw_target: &str,
    known_modules: &std::collections::HashSet<&str>,
    file_to_module: &HashMap<String, String>,
    dir_to_modules: &HashMap<String, Vec<String>>,
    aliases: &crate::ingestion::import_aliases::ImportAliases,
) -> Option<String> {
    // -1. Alias rewrite (tsconfig paths / go module prefix / cargo workspace
    //     crate).  Normalises the specifier to a repo-relative path so the
    //     existing matching tiers below can handle it without extra knowledge.
    let normalized = crate::ingestion::import_aliases::normalize(raw_target, aliases);
    let raw_target: &str = normalized.as_deref().unwrap_or(raw_target);

    // 0. Rust-style logical module path (`crate::a::b::Item`, `self::a::b`,
    //    bare `a::b::c`). File-path targets (C/C++ includes, Python relative
    //    imports) never contain `::`, so this branch is Rust-specific and is
    //    tried first. `crate`/`self` are crate-root markers (we treat the
    //    ingest root AS the crate root — true when the crate is the ingest
    //    target); the trailing segment(s) are usually imported ITEMS
    //    (types/fns), not modules, so we scan progressively shorter path
    //    prefixes (longest first) against the module / dir / file maps and
    //    take the first hit. `super::`-relative paths need the importing
    //    module for context we don't have here, so we skip them rather than
    //    emit a wrong edge (the per-symbol import_map still binds the call).
    if raw_target.contains("::") {
        if raw_target.starts_with("super::") || raw_target.contains("::super::") {
            return None;
        }
        let segs: Vec<&str> = raw_target
            .split("::")
            .filter(|s| !s.is_empty() && !matches!(*s, "crate" | "self"))
            .collect();
        for end in (1..=segs.len()).rev() {
            let path = segs[..end].join("/");
            if known_modules.contains(path.as_str()) {
                return Some(path);
            }
            if let Some(mods) = dir_to_modules.get(&path) {
                return Some(mods[0].clone());
            }
            if let Some(m) = file_to_module.get(&format!("{path}.rs")) {
                return Some(m.clone());
            }
        }
        return None;
    }
    // 1. Same-dir include with /@filename marker.
    if raw_target.contains("/@") {
        if let Some(target_mod) = file_to_module.get(raw_target) {
            return Some(target_mod.clone());
        }
        let (dir, at_name) = raw_target.split_once("/@")?;
        let stem = at_name.rsplit_once('.').map(|(s, _)| s).unwrap_or(at_name);
        for ext in &["hpp", "h", "cpp", "c", "cc"] {
            let key = format!("{dir}/@{stem}.{ext}");
            if let Some(target_mod) = file_to_module.get(&key) {
                return Some(target_mod.clone());
            }
        }
        if let Some(mods) = dir_to_modules.get(dir)
            && mods.len() == 1
        {
            return Some(mods[0].clone());
        }
        return None;
    }
    // 2. Exact match against known modules.
    if known_modules.contains(raw_target) {
        return Some(raw_target.to_string());
    }
    // 3. Directory → a module that owns files there.
    if let Some(mods) = dir_to_modules.get(raw_target) {
        return Some(mods[0].clone());
    }
    // 4. Parent directory.
    if let Some((parent, _)) = raw_target.rsplit_once('/') {
        if known_modules.contains(parent) {
            return Some(parent.to_string());
        }
        if let Some(mods) = dir_to_modules.get(parent) {
            return Some(mods[0].clone());
        }
    }
    // 5. Common prefixes.
    for prefix in &["src/", "libs/", "include/"] {
        let prefixed = format!("{prefix}{raw_target}");
        if known_modules.contains(prefixed.as_str()) {
            return Some(prefixed);
        }
        if let Some(mods) = dir_to_modules.get(&prefixed) {
            return Some(mods[0].clone());
        }
    }
    None
}

/// Stage 1: clone a git repo into the clone dir. (Website sources go through
/// `crawl_pages` + `ingest_pages` instead.)
pub(crate) async fn clone_or_crawl(
    store: &IngestionStore,
    job_id: Uuid,
    req: &IngestRequest,
    cfg: &Config,
) -> Result<PathBuf> {
    store
        .update_job_status(job_id, &req.repo_name, "cloning", None, None, None)
        .await?;

    let clone_token = req
        .user_token
        .as_deref()
        .or(cfg.gitlab_service_token.as_ref().map(|s| s.expose_secret()));

    let repo_dir = clone::clone_repo(
        &req.repo_name,
        &req.git_ref,
        &cfg.gitlab_url,
        clone_token,
        &cfg.ingest_clone_dir,
        req.local_path.as_deref(),
    )?;

    Ok(repo_dir)
}

/// Stage 1 (website): resolve the source's adapter, then discover + extract its
/// pages in memory. Replaces the disk-writing `crawl_website` path.
pub(crate) async fn crawl_pages(
    store: &IngestionStore,
    job_id: Uuid,
    req: &IngestRequest,
    cfg: &Config,
    resolver: &AdapterResolver,
) -> Result<Vec<ExtractedPage>> {
    store
        .update_job_status(job_id, &req.repo_name, "crawling", None, None, None)
        .await?;

    let seed_url = req
        .seed_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Website source requires seed_url"))?;

    let seed_host = reqwest::Url::parse(seed_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();

    crawler::CRAWL_ALLOWED_HOST
        .scope(seed_host, async move {
            // ── existing body, verbatim, from the probe client through Ok(pages) ──
            let probe_client = crawler::build_crawl_client(15);
            let root_html = crawler::fetch_page(&probe_client, seed_url)
                .await
                .unwrap_or_default();
            let ctx = ProbeContext {
                seed_url: seed_url.to_string(),
                root_html,
            };
            let adapter = resolver.resolve(&ctx).await;
            info!(job_id = %job_id, adapter = adapter.id(), "Resolved site adapter");
            let limits = CrawlLimits {
                depth: req.crawl_depth.unwrap_or(2).min(10),
                max_pages: cfg.ingest_crawl_max_pages,
                delay_ms: cfg.ingest_crawl_delay_ms,
                url_pattern: req.url_pattern.clone(),
            };
            let src = SourceSpec {
                repo_name: req.repo_name.clone(),
                seed_url: seed_url.to_string(),
            };
            let page_refs = adapter.discover(&src, &limits).await?;
            let mut pages = Vec::with_capacity(page_refs.len());
            for p in &page_refs {
                match adapter.extract(p).await {
                    Ok(page) => pages.push(page),
                    Err(e) => warn!(url = %p.url, err = %e, "Page extraction failed; skipping"),
                }
            }
            info!(job_id = %job_id, pages = pages.len(), "Crawl complete (in-memory)");
            Ok(pages)
        })
        .await
}

/// Website sources: route extracted pages through the Doc Space pipeline.
/// In-memory replacement for `ingest_website` (which read `.md` off disk).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn ingest_pages(
    store: &IngestionStore,
    pg: &PgPool,
    neo4j: &Neo4jPool,
    embedder: &Arc<dyn EmbeddingProvider>,
    llm: &Arc<dyn LlmProvider>,
    job_id: Uuid,
    req: &IngestRequest,
    pages: Vec<ExtractedPage>,
) -> Result<()> {
    let doc_store = DocStore::new(
        pg.clone(),
        neo4j.clone(),
        embedder.clone(),
        Some(llm.clone()),
    );

    doc_store.clean_repo_docs(&req.repo_name).await?;
    store.set_total_files(job_id, pages.len() as i32).await?;
    store
        .update_job_status(job_id, &req.repo_name, "storing", None, None, None)
        .await?;

    let mut total_sections = 0;
    for (i, page) in pages.iter().enumerate() {
        let vc_json: Option<serde_json::Value> = page
            .version_coordinate
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;

        let doc_id = doc_store
            .create_document(
                &req.repo_name,
                &page.title,
                "website",
                Some(&page.url),
                vc_json.as_ref(),
            )
            .await?;

        let raw_sections = sections::parse_markdown_sections(&page.markdown);
        let stored = doc_store
            .store_sections(
                doc_id,
                &req.repo_name,
                &page.title,
                &raw_sections,
                None,
                vc_json.as_ref(),
            )
            .await?;

        total_sections += stored.len();

        if (i + 1) % 5 == 0 || i + 1 == pages.len() {
            store
                .update_job_status(
                    job_id,
                    &req.repo_name,
                    "storing",
                    Some((i + 1) as i32),
                    Some(total_sections as i32),
                    None,
                )
                .await?;
        }
    }

    let clustering = DocClustering::new(pg.clone(), neo4j.clone(), llm.clone());
    match clustering.cluster_repo_sections(&req.repo_name).await {
        Ok(count) => info!(clusters = count, "Doc clustering complete"),
        Err(e) => warn!(err = %e, "Doc clustering failed (non-fatal)"),
    }

    store
        .update_job_status(
            job_id,
            &req.repo_name,
            "done",
            Some(pages.len() as i32),
            Some(total_sections as i32),
            None,
        )
        .await?;

    info!(
        job_id = %job_id,
        repo = %req.repo_name,
        pages = pages.len(),
        total_sections,
        "Website ingestion complete (Doc Space pipeline, in-memory)"
    );
    Ok(())
}

/// Stage 2: walk the repo tree and resolve each file's extractor.
pub(crate) fn analyze(repo_dir: &Path, cfg: &Config) -> Result<Vec<AnalyzedFile>> {
    file_discovery::analyze_repo(
        repo_dir,
        cfg.ingest_max_file_size,
        &cfg.ingest_skip_patterns,
    )
}

/// Stages 3a–3d: parse + chunk every file, bottom-up roll-up, LLM semantic
/// split of oversized modules, then post-split roll-up. Owns `files`.
pub(crate) async fn chunk_and_group(
    job_id: Uuid,
    files: Vec<AnalyzedFile>,
    cfg: &Config,
    llm: &Arc<dyn LlmProvider>,
) -> Result<(FinalModules, VirtualPaths, Vec<FailedFile>)> {
    // Parse every file, collecting chunks in memory grouped by directory
    let mut dir_files: HashMap<String, Vec<ParsedFile>> = HashMap::new();
    let mut failed_files: Vec<FailedFile> = Vec::new();

    for file in files {
        let source = match std::fs::read_to_string(&file.path) {
            Ok(s) => s,
            Err(e) => {
                warn!(path = %file.relative_path, err = %e, "Failed to read file, skipping");
                failed_files.push(FailedFile {
                    path: file.relative_path.clone(),
                    reason: e.to_string(),
                });
                continue;
            }
        };

        // Enforce max lines
        let source: String = source
            .lines()
            .take(cfg.ingest_max_lines)
            .collect::<Vec<_>>()
            .join("\n");

        let output = match chunker::extract_file(&file, &source, cfg.ingest_chunk_max_size) {
            Ok(o) => o,
            Err(e) => {
                warn!(path = %file.relative_path, err = %e, "Parse failed, skipping");
                failed_files.push(FailedFile {
                    path: file.relative_path.clone(),
                    reason: e.to_string(),
                });
                continue;
            }
        };

        let dir = file
            .relative_path
            .rsplit_once('/')
            .map(|(d, _)| d.to_string())
            .unwrap_or_else(|| ".".into());

        dir_files.entry(dir).or_default().push(ParsedFile {
            file,
            chunks: output.chunks,
            edges: output.edges,
        });
    }

    info!(job_id = %job_id, directories = dir_files.len(), "Chunking complete, starting module grouping");

    // ════════════════════════════════════════════════════════════
    // Stage 3b: Bottom-up Roll-up (merge tiny directories into parent)
    // ════════════════════════════════════════════════════════════
    let module_min = cfg.module_min_files as usize;
    let mut changed = true;
    while changed {
        changed = false;
        let small_dirs: Vec<String> = dir_files
            .iter()
            .filter(|(dir, pfiles)| pfiles.len() < module_min && dir.as_str() != ".")
            .map(|(dir, _)| dir.clone())
            .collect();

        for dir in small_dirs {
            let parent = dir
                .rsplit_once('/')
                .map(|(p, _)| p.to_string())
                .unwrap_or_else(|| ".".into());

            if let Some(orphan_files) = dir_files.remove(&dir) {
                dir_files.entry(parent).or_default().extend(orphan_files);
                changed = true;
            }
        }
    }

    info!(job_id = %job_id, modules_after_rollup = dir_files.len(), "Roll-up complete");

    // ════════════════════════════════════════════════════════════
    // Stage 3c: LLM Semantic Grouping (split large directories)
    // ════════════════════════════════════════════════════════════
    let mut final_modules: HashMap<String, Vec<ParsedFile>> = HashMap::new();
    let mut virtual_paths: HashSet<String> = HashSet::new();

    for (dir_path, parsed_files) in dir_files {
        if parsed_files.len() as u32 > cfg.module_max_files {
            // Build rich metadata from already-parsed chunks
            let file_meta: Vec<virtual_modules::FileGroupingMeta> = parsed_files
                .iter()
                .map(|pf| {
                    let filename = pf
                        .file
                        .relative_path
                        .rsplit_once('/')
                        .map(|(_, n)| n)
                        .unwrap_or(&pf.file.relative_path)
                        .to_string();
                    let symbols: Vec<(String, String, Option<String>)> = pf
                        .chunks
                        .iter()
                        .map(|c| (c.chunk_type.clone(), c.name.clone(), c.signature.clone()))
                        .collect();
                    virtual_modules::FileGroupingMeta { filename, symbols }
                })
                .collect();

            if let Some(result) = virtual_modules::maybe_split_module(
                cfg.module_max_files,
                llm.as_ref(),
                &dir_path,
                &file_meta,
            )
            .await
            {
                info!(
                    dir_path,
                    groups = result.groups.len(),
                    "Split oversized module into virtual sub-modules"
                );

                // Build a filename -> ParsedFile lookup
                let mut by_name: HashMap<String, ParsedFile> = HashMap::new();
                for pf in parsed_files {
                    let fname = pf
                        .file
                        .relative_path
                        .rsplit_once('/')
                        .map(|(_, n)| n)
                        .unwrap_or(&pf.file.relative_path)
                        .to_string();
                    by_name.insert(fname, pf);
                }

                for (group_name, group_files) in &result.groups {
                    let virtual_path = format!("{dir_path}/{group_name}");
                    let matching: Vec<ParsedFile> = group_files
                        .iter()
                        .filter_map(|fname| by_name.remove(fname))
                        .collect();
                    if !matching.is_empty() {
                        virtual_paths.insert(virtual_path.clone());
                        final_modules.insert(virtual_path, matching);
                    }
                }

                // Any files not claimed by the LLM go into the parent dir
                if !by_name.is_empty() {
                    let leftover: Vec<ParsedFile> = by_name.into_values().collect();
                    warn!(
                        dir_path,
                        unclaimed = leftover.len(),
                        "LLM missed some files, adding to parent module"
                    );
                    final_modules.entry(dir_path).or_default().extend(leftover);
                }
            } else {
                final_modules.insert(dir_path, parsed_files);
            }
        } else {
            final_modules.insert(dir_path, parsed_files);
        }
    }

    // ════════════════════════════════════════════════════════════
    // Stage 3d: Post-split roll-up (merge tiny virtual sub-modules)
    // ════════════════════════════════════════════════════════════
    let mut post_changed = true;
    while post_changed {
        post_changed = false;
        let small_mods: Vec<String> = final_modules
            .iter()
            .filter(|(path, files)| {
                files.len() < module_min
                    && path.as_str() != "."
                    && virtual_paths.contains(path.as_str())
            })
            .map(|(path, _)| path.clone())
            .collect();

        for mod_path in small_mods {
            let parent = mod_path
                .rsplit_once('/')
                .map(|(p, _)| p.to_string())
                .unwrap_or_else(|| ".".into());

            if let Some(orphan_files) = final_modules.remove(&mod_path) {
                virtual_paths.remove(&mod_path);
                final_modules
                    .entry(parent)
                    .or_default()
                    .extend(orphan_files);
                post_changed = true;
            }
        }
    }

    info!(job_id = %job_id, final_modules = final_modules.len(), "Post-split roll-up complete");

    Ok((final_modules, virtual_paths, failed_files))
}

pub(crate) struct Stage4Output {
    pub processed_files: i32,
    pub total_chunks: i32,
    pub all_imports: Vec<(String, String)>,
}

/// Stage 4: Resolve & Accumulate (Roadmap F — RAM-first ingest). Embeds
/// new/changed chunks and stages module/large-chunk rows into `acc`; writes
/// NOTHING to Postgres or Neo4j. The actual commit (via
/// `SnapshotPgRepo::import_repo_snapshot` + the Neo4j write-port replay)
/// happens later, in the commit gate (Task 7) — strictly after every stage's
/// resolution is done, so nothing here needs to defend against a partial
/// write becoming visible mid-run: there is no write to be partial.
///
/// This retires the checkpoint/resume machinery the pre-Roadmap-F version of
/// this function carried (per-module checkpoint skip, per-module
/// `clean_module_data` before re-storing, and the failure-compensation clean):
/// all of that existed solely to make a crash-then-resume run safe when a
/// PARTIAL set of modules had already been written for real. Since nothing is
/// written for real until the commit gate, a crash anywhere in Stages 1-9
/// simply means "nothing committed, retry the whole ingest" — there is no
/// partial DB state left behind to reconcile.
///
/// Carved from the original checkpoint/compensation-driven `stage4_embed_store`.
pub(crate) async fn stage4_embed_store(
    store: &IngestionStore,
    job_id: Uuid,
    req: &IngestRequest,
    final_modules: &FinalModules,
    virtual_paths: &HashSet<String>,
    acc: &mut crate::ingestion::accumulator::IngestAccumulator,
    // ★ Roadmap F Task 8 review fix: gates the content-match skip's
    // `existing_chunks` fetch. Only `run_sync` (`preserve_existing = true`)
    // may byte-match-and-reuse without re-staging, because only its commit
    // gate skips the repo-wide wipe; the production path
    // (`preserve_existing = false`) wipes at commit and re-imports the
    // accumulator, so it MUST resolve every chunk freshly (empty existing
    // map) or the wipe decimates byte-matched chunks. See the per-module
    // fetch's own comment for the full rationale + the regression test.
    preserve_existing: bool,
) -> Result<Stage4Output> {
    store
        .update_job_status(job_id, &req.repo_name, "storing", None, None, None)
        .await?;

    let mut processed_files = 0i32;
    let mut total_chunks = 0i32;
    let mut all_imports: Vec<(String, String)> = Vec::new();

    // Judgment call (module id freshness across re-ingests): a module keeps
    // its ORIGINAL Postgres id across every re-ingest today — `upsert_module`
    // is `ON CONFLICT (repo_name, path) DO UPDATE`, and that SET list never
    // touches `id`, so a conflict always returns the pre-existing id. That
    // stability is load-bearing OUTSIDE ingestion: `EdgeRepo::
    // merge_explains_to_module` (`linking/explains.rs`, real/used) MERGEs a
    // `Section -[:EXPLAINS]-> Module {pg_id}` edge, and `GraphReadRepo::
    // get_module_note_ids` reads `(Note)-[:ATTACHED_TO]->(Module {pg_id})` —
    // both persist a module's pg_id across separate operations, not just
    // within one ingestion run. Minting a fresh id unconditionally whenever a
    // module changes (this roadmap's stated default, absent this check) would
    // silently orphan both on every content-changing re-ingest — the old
    // Module node's identity disappears from under them. So: resolve existing
    // ids for every module path up front, in one batched, read-only query
    // (unconditionally safe, same reasoning as `existing_chunks` below), and
    // reuse them; only a genuinely new module path gets a fresh id.
    let all_module_paths: Vec<String> = final_modules.keys().cloned().collect();
    // Review finding 2 (Roadmap F Task 3 fix-up): this lookup MUST propagate
    // a real error rather than silently degrading to an empty map. A
    // transient DB error (timeout, pool exhaustion) here would otherwise
    // make every module in this run look brand new, minting a FRESH id for
    // modules that already have real cross-referenced ids in Postgres — the
    // exact id-orphaning bug this batched lookup exists to prevent (see the
    // judgment-call comment above), just triggered by a transient error
    // instead of a design choice.
    let existing_module_ids: HashMap<String, Uuid> = store
        .modules
        .resolve_module_paths(&req.repo_name, &all_module_paths)
        .await?
        .into_iter()
        .collect();

    // Reused whenever a file's whole-file `all_match` is false, to force
    // `resolve_chunks` to treat every chunk in that file as new/changed (see
    // the granularity comment in the per-file loop below).
    let empty_existing: HashMap<String, ChunkRow> = HashMap::new();

    for (module_path, module_files) in final_modules {
        // D-roadmap E1 / Roadmap F: fetch this module's EXISTING chunks (if
        // any) ONCE, indexed by name, so each file's chunks below can be
        // compared for an exact byte-match before re-embedding. Two DIFFERENT
        // files sharing a (module_path, name) collision is a narrow,
        // pre-existing, and harmless edge case: reuse only fires when content
        // ALSO matches byte-for-byte, so the embedding being reused is still a
        // valid embedding of that exact text regardless of which file it
        // originally came from.
        //
        // ★ The fetch is GATED on `preserve_existing` (Roadmap F Task 8
        // review fix). The content-match skip only reuses an existing row's
        // id WITHOUT re-staging that chunk into the delta-only accumulator —
        // so a byte-matched chunk survives ONLY if its DB row is left in
        // place. That holds for `run_sync` (`preserve_existing = true`), whose
        // commit gate deliberately SKIPS `clean_old_data` precisely so
        // reused rows survive. It does NOT hold for the production path
        // (`start()`/`start_resume()`, `preserve_existing = false`), whose
        // commit gate WIPES the whole repo and re-imports only the
        // accumulator: any chunk that byte-matched (and was therefore not
        // staged) would be wiped and never re-imported — decimating the graph
        // on every production re-ingest of unchanged content (regression
        // proven by `e2e_production_reingest_unchanged_content_preserves_chunks`:
        // pre-fix, a second production ingest of identical content dropped
        // the repo from 5 chunks to 0). Feeding an EMPTY map when
        // `preserve_existing = false` forces every chunk through the
        // fresh-embed-and-stage path, so the accumulator is COMPLETE before
        // the commit-gate wipe — making wipe-then-import-all lossless. This
        // restores E1's original `preserve_existing`-gated design; it was
        // Task 3 (which dropped the gate) interacting with Task 8 (which
        // removed the early Stage-3 wipe that used to keep production's repo
        // empty at Stage 4) that reintroduced the byte-match on the
        // production path. The cost when gated off is a redundant re-embed of
        // unchanged content on production re-ingest — exactly the pre-Roadmap-F
        // behavior (production always wiped + fully re-embedded), never a
        // correctness issue.
        let existing_chunks: HashMap<String, ChunkRow> = if preserve_existing {
            store
                .chunks
                .list_chunks_in_module_all(&req.repo_name, module_path)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|c| (c.name.clone(), c))
                .collect()
        } else {
            HashMap::new()
        };

        // Review finding 1 (Roadmap F Task 3 fix-up): resolve this module's
        // id ONCE, up front, independent of whether this run resolves any
        // chunks for it. Task 4 depends on `acc.module_path_to_id` having an
        // entry for EVERY module in `final_modules` — a module whose every
        // file resolves to zero chunks is a legitimate case (module
        // insertion only requires a non-empty file list, not non-empty
        // chunks) and can already have a row in Postgres from a prior
        // ingest. Either reuse that existing id, or — for a genuinely new
        // module path — mint the fresh id that would be used if this module
        // ever gets staged.
        let module_id = existing_module_ids
            .get(module_path.as_str())
            .copied()
            .unwrap_or_else(Uuid::new_v4);

        let mut module_chunk_ids = Vec::new();
        let mut module_language = None;
        let mut module_exports = 0i32;
        let mut module_processed = 0i32;
        let mut module_chunks_count = 0i32;
        // D-roadmap E1: true iff at least one chunk in this module was
        // actually freshly embedded this run (new content, or no existing
        // match), OR the module's final chunk-id count differs from what
        // existed before this run (see the count check right before the gate
        // below). A per-file content match alone is NOT sufficient to prove
        // the module is unchanged: it only proves no SURVIVING file's chunks
        // differ — it says nothing about whether the module's chunk SET
        // shrank (e.g. a function deleted from a file whose remaining chunks
        // still byte-match). The count check closes that gap. Only when
        // NEITHER condition holds — every file matched AND the total count is
        // unchanged — are the module row and its large_chunks already correct
        // from the run that first resolved this exact content, making a fresh
        // module summary embed + large_chunks rebuild pure waste.
        let mut module_changed = false;

        // Collect imports first (doesn't touch the accumulator, safe to do
        // before the resolve loop below).
        for pf in module_files {
            let file_imports = imports_from_edges(&pf.edges, module_path);
            all_imports.extend(file_imports);
        }

        let module_result: Result<()> = async {
            for pf in module_files {
                if !pf.chunks.is_empty() {
                    let lang_name = extractor_name(pf.file.extractor);
                    module_language = Some(lang_name);
                    module_exports += pf.chunks.len() as i32;

                    // Judgment call (module_changed / reuse granularity):
                    // `resolve_chunks` (Task 2) already does its OWN per-chunk
                    // content-match-vs-fresh-embed decision, which is
                    // strictly finer-grained than the check below — today's
                    // behavior is that a SINGLE differing/new chunk forces a
                    // full re-embed of the WHOLE file, even chunks that would
                    // individually byte-match. `build_contextual_embed_text`
                    // embeds only this chunk's own fields (module_path/
                    // repo_name/language/fqn/doc/signature/content) with no
                    // sibling-chunk context, so there's no correctness reason
                    // a byte-matched sibling MUST be re-embedded just because
                    // another chunk in the file changed — finer-grained reuse
                    // would very likely be equally correct. But that's an
                    // unproven behavior change with no dedicated test
                    // coverage, on the highest-risk task in this roadmap, so
                    // the conservative choice wins here: pre-compute the SAME
                    // `all_match` boolean today's code computes, and pass an
                    // intentionally EMPTY existing-map to `resolve_chunks`
                    // when `!all_match` — forcing every chunk in the file to
                    // be treated as new/changed, byte-identical to today's
                    // whole-file granularity.
                    let all_match = pf.chunks.iter().all(|c| {
                        existing_chunks
                            .get(&c.name)
                            .is_some_and(|existing| existing.content == c.content)
                    });
                    let existing_for_call = if all_match {
                        &existing_chunks
                    } else {
                        &empty_existing
                    };

                    let before_len = acc.pg.chunks.len();
                    let results = store
                        .resolve_chunks(
                            &req.repo_name,
                            module_path,
                            lang_name,
                            &pf.chunks,
                            existing_for_call,
                            acc,
                        )
                        .await?;

                    // `resolve_chunks` leaves `git_ref` unset on
                    // freshly-appended rows (it has no `req` in scope) —
                    // backfill it here. `resolve_chunks` only ever appends
                    // (never removes/reorders), so every row from
                    // `before_len` on is exactly what THIS call added.
                    for row in &mut acc.pg.chunks[before_len..] {
                        row.git_ref = Some(req.git_ref.clone());
                    }

                    // Roadmap F, Task 7.5: populate the full chunk-index
                    // source with EVERY chunk this file resolved to this run
                    // — both freshly-embedded ones (`acc.pg.chunks` already
                    // has these) and byte-matched/reused ones (which
                    // `acc.pg.chunks` deliberately omits) — so Stage 6/7 can
                    // see reused chunks too. See `ChunkIndexEntry`'s doc
                    // comment for the full rationale.
                    for (c, &(id, freshly_embedded)) in pf.chunks.iter().zip(results.iter()) {
                        let entry = if freshly_embedded {
                            super::accumulator::ChunkIndexEntry {
                                id,
                                name: c.name.clone(),
                                module_path: module_path.clone(),
                                fqn: c.fqn.clone(),
                                parent_fqn: c.parent_fqn.clone(),
                                signature: c.signature.clone(),
                                chunk_type: c.chunk_type.clone(),
                                content: c.content.clone(),
                                language: Some(lang_name.to_string()),
                            }
                        } else {
                            // A reused chunk was only ever marked
                            // `!freshly_embedded` because `resolve_chunks`
                            // found it in `existing_for_call` by name with a
                            // byte-matching content — and `existing_for_call`
                            // is either `&existing_chunks` (the `all_match`
                            // branch) or `&empty_existing` (which can never
                            // produce a reuse). So this lookup is guaranteed
                            // to hit.
                            let existing = existing_chunks.get(&c.name).expect(
                                "a reused chunk must have a matching existing_chunks entry",
                            );
                            super::accumulator::ChunkIndexEntry {
                                id,
                                name: existing.name.clone(),
                                module_path: module_path.clone(),
                                fqn: existing.fqn.clone(),
                                parent_fqn: existing.parent_fqn.clone(),
                                signature: existing.signature.clone(),
                                chunk_type: existing.chunk_type.clone(),
                                content: existing.content.clone(),
                                language: existing.language.clone(),
                            }
                        };
                        acc.chunk_index_source.push(entry);
                    }

                    if results
                        .iter()
                        .any(|(_, freshly_embedded)| *freshly_embedded)
                    {
                        module_changed = true;
                    }

                    let ids: Vec<Uuid> = results.into_iter().map(|(id, _)| id).collect();
                    module_chunks_count += ids.len() as i32;
                    module_chunk_ids.extend(ids);
                }

                module_processed += 1;
                if (processed_files + module_processed) % 10 == 0 {
                    store
                        .update_job_status(
                            job_id,
                            &req.repo_name,
                            "storing",
                            Some(processed_files + module_processed),
                            Some(total_chunks + module_chunks_count),
                            None,
                        )
                        .await?;
                }
            }

            // D-roadmap E1 fix (reviewer-Important): per-file content
            // matching cannot detect NET REMOVAL from the module — a file
            // whose surviving chunks all byte-match tells us nothing about
            // chunks that used to exist in this module and no longer do.
            // Catch that directly by comparing the final resolved chunk-id
            // count against how many chunks existed for this module before
            // this run. This also (redundantly, harmlessly) catches net
            // addition, which the per-file branch above already flips
            // `module_changed` for.
            //
            // Residual, accepted gap: this is a COUNT comparison, so a pure
            // REORDER within the module (same chunk names/contents/total
            // count, functions physically reordered within/across files) is
            // NOT caught here — `module_changed` would stay false. That
            // leaves the large_chunks adjacency-window text (built from chunk
            // content concatenated in file order) potentially stale even
            // though nothing else is. This is a narrower, softer form of
            // staleness (stale adjacency text in a search index, not a wrong
            // export count or dangling live edges) and is intentionally out
            // of scope for this fix.
            if module_chunk_ids.len() != existing_chunks.len() {
                module_changed = true;
            }

            if !module_chunk_ids.is_empty() && module_changed {
                let summary = format!("{module_path} ({module_exports} exports)");

                // `module_id` was already resolved up front (reusing the
                // module's existing id if this path already exists in
                // Postgres) — see the judgment-call comment above the
                // batched `existing_module_ids` lookup.

                // Embed the module summary — mirrors `upsert_module`'s
                // internal embedding logic verbatim (same guard, same call).
                let embedding = if !summary.is_empty() {
                    Some(store.embedder.embed(&summary).await?.vector)
                } else {
                    None
                };

                acc.pg.modules.push(ModuleSnapshotRow {
                    id: module_id,
                    path: module_path.clone(),
                    language: Some(module_language.unwrap_or("other").to_string()),
                    summary: Some(summary),
                    exports_count: Some(module_exports),
                    file_count: Some(module_files.len() as i32),
                    is_virtual: virtual_paths.contains(module_path.as_str()),
                    embedding,
                    git_ref: Some(req.git_ref.clone()),
                    ingested_at: None, // stamped by the commit gate (Task 7)
                });
                acc.graph
                    .module_nodes
                    .push((module_id.to_string(), module_path.clone()));

                // Generate large chunks (sliding window of 2 adjacent
                // chunks) — same gate, same combined-text construction, and
                // the same per-pair `embedder.embed` call
                // `store_large_chunks`/`insert_large_chunk` make internally
                // today, just staged into the accumulator instead of written.
                let module_raw_chunks: Vec<akashic_extraction::RawChunk> = module_files
                    .iter()
                    .flat_map(|pf| pf.chunks.iter().cloned())
                    .collect();
                if module_raw_chunks.len() >= 2 {
                    for i in 0..module_raw_chunks.len() - 1 {
                        let combined = format!(
                            "{}\n\n{}",
                            module_raw_chunks[i].content,
                            module_raw_chunks[i + 1].content
                        );
                        let large_embedding = store.embedder.embed(&combined).await?.vector;
                        acc.pg.large_chunks.push(LargeChunkSnapshotRow {
                            id: Uuid::new_v4(),
                            module_path: module_path.clone(),
                            chunk_ids: vec![module_chunk_ids[i], module_chunk_ids[i + 1]],
                            content: combined,
                            embedding: large_embedding,
                            git_ref: Some(req.git_ref.clone()),
                            ingested_at: None, // stamped by the commit gate (Task 7)
                        });
                    }
                }
            }
            Ok(())
        }
        .await;

        // Nothing was written this run, so there is nothing to compensate
        // for on failure — just propagate.
        module_result?;

        // Review finding 1 (Roadmap F Task 3 fix-up): record this module's
        // id for EVERY module in `final_modules`, not just ones that staged
        // a fresh `ModuleSnapshotRow` above (the `module_changed` branch) or
        // resolved a non-empty chunk set this run. Stage 5's IMPORTS_FROM
        // resolution — and Task 4 — need to translate EVERY module path in
        // this run via the in-memory map without their own PG round trip,
        // including a module whose every file resolved to zero chunks (a
        // legitimate case) but already has an existing id from a prior
        // ingest.
        acc.module_path_to_id.insert(module_path.clone(), module_id);

        processed_files += module_processed;
        total_chunks += module_chunks_count;
    }

    Ok(Stage4Output {
        processed_files,
        total_chunks,
        all_imports,
    })
}

pub(crate) struct Stage5Output {
    pub import_target_map: HashMap<(String, String), String>,
    pub imported_by_module: HashMap<String, HashSet<String>>,
}

/// Stage 5: resolve IMPORTS_FROM module edges. Builds the
/// file/dir → module indexes, resolves raw import specifiers to final modules,
/// stages the resolved edges into `acc.graph.module_import_edges` (Roadmap
/// F Task 4 — no DB write anymore), and returns the `import_target_map` +
/// `imported_by_module` that Stage 6's call resolution depends on. Carved
/// verbatim from `run_inner`.
#[allow(
    clippy::unused_async,
    reason = "kept `async` for call-site/interface consistency with every \
              other stage function `run_inner` awaits in sequence — this \
              task (Roadmap F Task 4) removed this function's only two \
              `.await` points (`save_checkpoint` + `create_import_edges`, \
              both DB writes), making the body itself synchronous, but a \
              future task in this same roadmap re-introducing I/O here \
              (or a caller-side reason to keep the uniform `async fn` \
              shape) is more likely than not, so the signature is left \
              alone rather than churned twice"
)]
pub(crate) async fn stage5_import_edges(
    job_id: Uuid,
    // `req` is no longer read inside this function: its only prior use
    // (`req.repo_name`, feeding `store.create_import_edges`) was removed by
    // this task's resolve-only tail. Kept as a parameter (prefixed to
    // silence `unused_variables`) rather than dropped from the signature —
    // `run_inner`'s call site already has `req` in scope and every other
    // stage function in this module takes it, so keeping the shape
    // consistent costs nothing and avoids a signature that looks
    // accidentally pruned.
    _req: &IngestRequest,
    repo_dir: &Path,
    final_modules: &FinalModules,
    all_imports: Vec<(String, String)>,
    acc: &mut crate::ingestion::accumulator::IngestAccumulator,
) -> Result<Stage5Output> {
    let all_imports_count = all_imports.len();
    // Build lookup maps for import resolution:
    // 1. file_to_module: "libs/webportal/nginx_deploy.cpp" → "libs/webportal/Nginx Config"
    // 2. dir_to_modules: "libs/webportal" → ["libs/webportal/App Portal", "libs/webportal/TLS & Cert", ...]
    let known_modules: HashSet<&str> = final_modules.keys().map(|s| s.as_str()).collect();
    let mut file_to_module: HashMap<String, String> = HashMap::new();
    let mut dir_to_modules: HashMap<String, Vec<String>> = HashMap::new();
    for (module_path, module_files) in final_modules {
        for pf in module_files {
            // Map full file path → module
            file_to_module.insert(pf.file.relative_path.clone(), module_path.clone());
            // Map "dir/@filename" → module (for same-dir include resolution)
            let filename = pf
                .file
                .relative_path
                .rsplit_once('/')
                .map(|(_, f)| f)
                .unwrap_or(&pf.file.relative_path);
            let file_dir = pf
                .file
                .relative_path
                .rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_else(|| ".".into());
            // "dir/@filename.cpp" and also "dir/@filename.h" etc
            file_to_module.insert(format!("{file_dir}/@{filename}"), module_path.clone());
            dir_to_modules
                .entry(file_dir)
                .or_default()
                .push(module_path.clone());
        }
    }
    // Dedup dir_to_modules values
    for modules in dir_to_modules.values_mut() {
        modules.sort();
        modules.dedup();
    }

    // (src_module, raw_target) → final_module. Used for IMPORTS_FROM module
    // edges AND the per-symbol import_map (EXT-3b). Built by reference so
    // `all_imports` stays available for the unresolved-import diagnostics below.
    let import_aliases = crate::ingestion::import_aliases::discover(repo_dir);

    let mut import_target_map: HashMap<(String, String), String> = HashMap::new();
    for (src_mod, raw_target) in &all_imports {
        if let Some(final_mod) = resolve_import_target(
            raw_target,
            &known_modules,
            &file_to_module,
            &dir_to_modules,
            &import_aliases,
        ) && src_mod != &final_mod
        {
            import_target_map.insert((src_mod.clone(), raw_target.clone()), final_mod);
        }
    }
    let resolved_imports: Vec<(String, String)> = import_target_map
        .iter()
        .map(|((src, _), tgt)| (src.clone(), tgt.clone()))
        .collect();

    // Debug: log unresolved imports to diagnose misses
    let resolved_set: HashSet<_> = resolved_imports.iter().cloned().collect();
    let unresolved: Vec<_> = {
        // Re-extract raw imports for logging (all_imports was consumed)
        let mut unresolved = Vec::new();
        for (module_path, module_files) in final_modules {
            for pf in module_files {
                let file_imports = imports_from_edges(&pf.edges, module_path);
                for (src, tgt) in file_imports {
                    if !resolved_set.contains(&(src.clone(), tgt.clone())) {
                        // Check it wasn't resolved to a different target
                        let was_resolved = resolved_imports.iter().any(|(s, _)| s == &src);
                        if !was_resolved {
                            unresolved.push((src, tgt));
                        }
                    }
                }
            }
        }
        unresolved.sort();
        unresolved.dedup();
        unresolved
    };
    for (src, tgt) in &unresolved {
        debug!(src_module = %src, raw_target = %tgt, "Unresolved import");
    }

    info!(
        job_id = %job_id,
        raw_imports = all_imports_count,
        resolved = resolved_imports.len(),
        unresolved = unresolved.len(),
        dir_to_module_entries = dir_to_modules.len(),
        "Import edge resolution"
    );

    let mut unique_imports = resolved_imports;
    unique_imports.sort();
    unique_imports.dedup();

    // EXT-3: per-source-module set of imported target modules, powering the
    // `import_scoped` resolution tier (a call resolves to a name unique among
    // the modules the caller's file imports).
    let mut imported_by_module: HashMap<String, HashSet<String>> = HashMap::new();
    for (src_mod, tgt_mod) in &unique_imports {
        imported_by_module
            .entry(src_mod.clone())
            .or_default()
            .insert(tgt_mod.clone());
    }

    if !unique_imports.is_empty() {
        info!(
            job_id = %job_id,
            import_edges = unique_imports.len(),
            "Resolved IMPORTS_FROM edges (in-memory)"
        );
        for (src_path, tgt_path) in &unique_imports {
            if let (Some(&src_id), Some(&tgt_id)) = (
                acc.module_path_to_id.get(src_path),
                acc.module_path_to_id.get(tgt_path),
            ) {
                acc.graph
                    .module_import_edges
                    .push((src_id.to_string(), tgt_id.to_string()));
            }
            // Same drop-if-missing guard `resolve_module_paths` + the
            // `if let (Some, Some)` filter in today's `create_import_edges`
            // already apply — a module path with no accumulator entry (e.g.
            // resolved to a path that isn't actually one of this run's
            // modules) is silently skipped, matching current behavior.
        }
    }

    Ok(Stage5Output {
        import_target_map,
        imported_by_module,
    })
}

/// Stage 6: resolve and create CALLS / References / RoutesTo / Implements /
/// Extends edges (incl. the EXT-8-1 inheritance-aware tier-0 cascade and
/// go_structural fallback) plus per-symbol Import edges. Carved verbatim from
/// `run_inner`; reads the Stage-5 outputs via `s5`.
///
/// `#[allow(unused_variables)]` on `req` (Roadmap F, Task 5): both Postgres
/// reads this stage used to make (`fetch_chunks_for_call_resolution`,
/// `fetch_go_chunks`) and all 6 `store.create_*_edges` writes were the only
/// callers of `req.repo_name` in this function; now that the chunk index and
/// go_structural rows come from `acc.chunk_index_source` (Task 7.5: switched
/// from the delta-only `acc.pg.chunks`) and the resolved edges are appended
/// into `acc.graph.*` instead, nothing left in this function reads `req`.
/// Kept in the signature (not renamed to `_req`) for call-site/interface
/// consistency with every other `stageN_*` function, all of which take
/// `req: &IngestRequest`.
///
/// `#[allow(clippy::unused_async)]`: removing the two Postgres reads and six
/// Neo4j/Postgres writes (this task's entire mandate) also removed every
/// `.await` this function used to make — it is pure computation now. Kept
/// `async` (not de-asynced) per the brief's literal signature and to match
/// the call site in `pipeline.rs` (`stages::stage6_call_edges(...).await?`)
/// and every sibling `stageN_*` function's shape; de-asyncing would be a
/// call-site-rippling signature change outside this task's stated scope.
#[allow(unused_variables, clippy::unused_async)]
pub(crate) async fn stage6_call_edges(
    job_id: Uuid,
    req: &IngestRequest,
    final_modules: &FinalModules,
    s5: &Stage5Output,
    acc: &mut crate::ingestion::accumulator::IngestAccumulator,
) -> Result<()> {
    info!(job_id = %job_id, "Stage 6: Creating CALLS edges");

    // Build the chunk index from THIS RUN's resolved chunks (Stage 4's
    // accumulator output) instead of querying Postgres — under RAM-first,
    // nothing has been written yet, so `fetch_chunks_for_call_resolution`
    // would return stale data from a PRIOR ingest of this repo_name (or
    // nothing, for a first-time ingest), not this run's chunks.
    //
    // Roadmap F, Task 7.5: sourced from `acc.chunk_index_source` (NOT
    // `acc.pg.chunks`) — the latter is delta-only (byte-matched/reused
    // chunks are deliberately never re-staged there), so on a re-run against
    // unchanged content it can be EMPTY even though every chunk still fully
    // exists. `chunk_index_source` carries the union of freshly-resolved AND
    // reused chunks; see `ChunkIndexEntry`'s doc comment for the full
    // rationale.
    //
    // ChunkIndex needs (id, name, module_path, parent_fqn, signature,
    // chunk_type, content) — parent_fqn powers the (type, method) index for
    // D1 type-qualified call resolution; signature (D1c-1) powers
    // by_return_type for method-chain resolution; chunk_type + content
    // (D1c-2) power by_field_type for field-access resolution (parsed only
    // for chunk_type == "struct" rows).
    let chunk_index = ChunkIndex::build_from(
        acc.chunk_index_source
            .iter()
            .map(|r| {
                (
                    r.id,
                    r.name.clone(),
                    r.module_path.clone(),
                    r.parent_fqn.clone(),
                    r.signature.clone(),
                    r.chunk_type.clone(),
                    r.content.clone(),
                )
            })
            .collect(),
    );

    // EXT-8-1 auxiliary maps (built from the same extended rows).
    // Type-level chunk types that can appear as supertypes.
    const TYPE_KINDS: &[&str] = &["struct", "class", "interface", "trait", "enum", "type"];
    // Method-level chunk types that are callable members of a type.
    const METHOD_KINDS: &[&str] = &["method", "function"];

    // chunk id → fqn (skip rows without fqn)
    let mut fqn_by_id: HashMap<Uuid, String> = HashMap::new();
    // fqn → chunk id, restricted to type-level chunks
    let mut type_id_by_fqn: HashMap<String, Uuid> = HashMap::new();
    // (type_fqn, method_name) → method chunk id
    let mut method_by_type_and_name: HashMap<(String, String), Uuid> = HashMap::new();
    // chunk id → enclosing type fqn (skip rows without parent_fqn)
    let mut parent_fqn_by_id: HashMap<Uuid, String> = HashMap::new();

    // Roadmap F, Task 7.5: same source switch as `chunk_index` above — this
    // scan reads `.fqn` too (for `fqn_by_id`/`type_id_by_fqn`), so it needs
    // the same reused-chunk visibility fix.
    for r in &acc.chunk_index_source {
        if let Some(f) = &r.fqn {
            fqn_by_id.insert(r.id, f.clone());
            if TYPE_KINDS.contains(&r.chunk_type.as_str()) {
                // Last-write-wins: an identical bare fqn across modules
                // collapses to the last chunk seen. Acceptable for a
                // 0.9-confidence additive inheritance heuristic.
                type_id_by_fqn.insert(f.clone(), r.id);
            }
        }
        if METHOD_KINDS.contains(&r.chunk_type.as_str())
            && let Some(pfqn) = &r.parent_fqn
        {
            parent_fqn_by_id.insert(r.id, pfqn.clone());
            method_by_type_and_name.insert((pfqn.clone(), r.name.clone()), r.id);
        }
    }

    let mut all_resolved_edges: Vec<ResolvedEdge> = Vec::new();
    let empty_imports: HashSet<String> = HashSet::new();
    let mut symbol_imports: Vec<(String, uuid::Uuid)> = Vec::new();
    // EXT-8-1: accumulate (method_name, caller_chunk_id) for calls the
    // four-tier cascade could not resolve.  Processed after the main loop.
    let mut dropped_calls: Vec<(String, Uuid)> = Vec::new();

    for (module_path, module_files) in final_modules {
        // Skip modules with no chunks (same guard the old code applied via
        // "find first file with chunks → else continue").
        if !module_files.iter().any(|pf| !pf.chunks.is_empty()) {
            continue;
        }

        // Build import_map: imported_name → resolved_module_path, powering the
        // tier-1 (1.0 "import_resolved") call resolution.
        //
        // EXT-3b: for a per-symbol Import edge (name = the imported symbol's
        // local binding, module_specifier = the raw target spec), resolve the
        // spec to its FINAL module via `s5.import_target_map` and accept the
        // mapping iff that module actually defines `name`. This resolves a
        // symbol to its EXACT source module even when the name is ambiguous
        // repo-wide. Aliased imports (local name != source name) won't match a
        // definition chunk and fall through (handled later by import_scoped).
        //
        // Fallback (module-level imports, or specs that didn't resolve): the
        // pre-refactor heuristic — an imported name maps to its chunk iff that
        // name is unique repo-wide.
        let mut import_map: ImportMap = ImportMap::new();
        for pf in module_files {
            for edge in &pf.edges {
                if edge.kind != EdgeKind::Import {
                    continue;
                }
                let EdgeEndpoint::Name {
                    name,
                    module_specifier,
                } = &edge.target
                else {
                    continue;
                };
                // The edge's target name is the LOCAL binding; the SOURCE
                // (definition-module) name lives in `edge.import_source`
                // metadata, present only for aliased imports. Absent =>
                // non-aliased, so source == local.
                let source = match edge.metadata.fields.get("edge.import_source") {
                    Some(MetadataValue::String(s)) => s.clone(),
                    _ => name.clone(),
                };
                // EXT-3b: exact per-symbol resolution via the import's module.
                // Resolve the spec to its final module, then verify that
                // module defines the SOURCE name (the alias bridges to it).
                if let Some(spec) = module_specifier
                    && let Some(final_mod) = s5
                        .import_target_map
                        .get(&(module_path.clone(), spec.clone()))
                    && let Some(&sym_chunk_id) = chunk_index
                        .by_path_and_name
                        .get(&(final_mod.clone(), source.clone()))
                {
                    import_map.insert(name.clone(), (final_mod.clone(), source.clone()));
                    symbol_imports.push((module_path.clone(), sym_chunk_id));
                    continue;
                }
                // Fallback: repo-wide unique name (source == name here).
                if let Some(ids) = chunk_index.by_name.get(name)
                    && ids.len() == 1
                    && let Some(mod_path) = chunk_index.chunk_module.get(&ids[0])
                {
                    import_map.insert(name.clone(), (mod_path.clone(), name.clone()));
                    symbol_imports.push((module_path.clone(), ids[0]));
                }
            }
        }

        // Resolve Call edges per file. Each Call edge's source ByteRange falls
        // inside exactly one enclosing chunk (the call expression's span is
        // contained in the chunk's [start_byte, end_byte) span). Map that
        // enclosing chunk to its PG-assigned UUID via (module_path, name).
        for pf in module_files {
            // (start_byte, end_byte, pg_chunk_id) for this file's chunks.
            let mut chunk_spans: Vec<(usize, usize, Uuid)> = pf
                .chunks
                .iter()
                // Exclude http_call chunks from the source-span candidate list so a
                // MakesHttpCall edge attributes to the ENCLOSING function, not the
                // call-site http_call chunk itself (self-loop avoidance). Target
                // resolution goes through the unfiltered global chunk_index, so this
                // does not block http_call chunks from being resolved as edge TARGETS.
                .filter(|c| c.chunk_type != "http_call")
                .filter_map(|c| {
                    chunk_index
                        .by_path_and_name
                        .get(&(module_path.clone(), c.name.clone()))
                        .map(|&id| (c.start_byte, c.end_byte, id))
                })
                .collect();
            // Narrowest enclosing chunk wins: sort by span width ascending so
            // the first containing match is the tightest fit.
            chunk_spans.sort_by_key(|(s, e, _)| e.saturating_sub(*s));

            for edge in &pf.edges {
                match edge.kind {
                    EdgeKind::Call
                    | EdgeKind::References
                    | EdgeKind::RoutesTo
                    | EdgeKind::MakesHttpCall => {
                        let EdgeEndpoint::ByteRange { start, .. } = &edge.source else {
                            continue;
                        };
                        let enclosing = chunk_spans
                            .iter()
                            .find(|(s, e, _)| *start >= *s && *start < *e)
                            .map(|(_, _, id)| *id);
                        if let Some(src_chunk_id) = enclosing {
                            if let Some(mut resolved) = resolve_call_edge(
                                edge,
                                src_chunk_id,
                                module_path,
                                &chunk_index,
                                &import_map,
                                s5.imported_by_module
                                    .get(module_path)
                                    .unwrap_or(&empty_imports),
                            ) {
                                if edge.kind == EdgeKind::References {
                                    resolved.kind = EdgeKind::References;
                                    resolved.ref_kind = match edge.metadata.fields.get("ref_kind") {
                                        Some(MetadataValue::String(s)) => Some(s.clone()),
                                        _ => Some("type".to_string()),
                                    };
                                } else if edge.kind == EdgeKind::RoutesTo {
                                    resolved.kind = EdgeKind::RoutesTo;
                                    resolved.ref_kind =
                                        match edge.metadata.fields.get("http_method") {
                                            Some(MetadataValue::String(s)) => Some(s.clone()),
                                            _ => Some("ANY".to_string()),
                                        };
                                } else if edge.kind == EdgeKind::MakesHttpCall {
                                    resolved.kind = EdgeKind::MakesHttpCall;
                                    resolved.ref_kind =
                                        match edge.metadata.fields.get("http_method") {
                                            Some(MetadataValue::String(s)) => Some(s.clone()),
                                            _ => Some("ANY".to_string()),
                                        };
                                }
                                all_resolved_edges.push(resolved);
                            } else if edge.kind == EdgeKind::Call {
                                // EXT-8-1: cascade returned None for a Call edge —
                                // record it for tier-0 inheritance resolution below.
                                if let EdgeEndpoint::Name { name, .. } = &edge.target {
                                    dropped_calls.push((name.clone(), src_chunk_id));
                                }
                            }
                        }
                    }
                    EdgeKind::Implements | EdgeKind::Extends => {
                        // Declaration-level name->name edge: resolve the SUBTYPE
                        // (source) by name within this module, then resolve the
                        // supertype (target) via the usual cascade.
                        let EdgeEndpoint::Name { name: subtype, .. } = &edge.source else {
                            continue;
                        };
                        let src_chunk_id = chunk_index
                            .by_path_and_name
                            .get(&(module_path.clone(), subtype.clone()))
                            .copied()
                            .or_else(|| {
                                chunk_index
                                    .by_name
                                    .get(subtype)
                                    .filter(|ids| ids.len() == 1)
                                    .map(|ids| ids[0])
                            });
                        if let Some(src_chunk_id) = src_chunk_id
                            && let Some(mut resolved) = resolve_call_edge(
                                edge,
                                src_chunk_id,
                                module_path,
                                &chunk_index,
                                &import_map,
                                s5.imported_by_module
                                    .get(module_path)
                                    .unwrap_or(&empty_imports),
                            )
                        {
                            resolved.kind = edge.kind;
                            resolved.ref_kind = match edge.metadata.fields.get("impl_kind") {
                                Some(MetadataValue::String(s)) => Some(s.clone()),
                                _ => Some("implements".to_string()),
                            };
                            all_resolved_edges.push(resolved);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Dedup by (src, tgt, kind, ref_kind) keeping highest confidence.
    // Using kind+ref_kind in the key lets a call and a type reference between
    // the same pair coexist rather than collapsing into one edge.
    {
        use std::collections::hash_map::Entry;
        let mut dedup: HashMap<(Uuid, Uuid, EdgeKind, Option<String>), ResolvedEdge> =
            HashMap::new();
        for re in all_resolved_edges.drain(..) {
            let key = (
                re.src_chunk_id,
                re.tgt_chunk_id,
                re.kind,
                re.ref_kind.clone(),
            );
            match dedup.entry(key) {
                Entry::Vacant(e) => {
                    e.insert(re);
                }
                Entry::Occupied(mut e) => {
                    if re.confidence > e.get().confidence {
                        *e.get_mut() = re;
                    }
                }
            }
        }
        all_resolved_edges = dedup.into_values().collect();
    }

    // EXT-8-1: Tier-0 inheritance-aware call resolution.
    // Build the subtype→supertype map from the Implements/Extends edges that
    // are already in all_resolved_edges, then ask the helper whether any of
    // the dropped calls can be resolved via a supertype.
    {
        let mut supertypes_of: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for re in &all_resolved_edges {
            if matches!(re.kind, EdgeKind::Implements | EdgeKind::Extends) {
                supertypes_of
                    .entry(re.src_chunk_id)
                    .or_default()
                    .push(re.tgt_chunk_id);
            }
        }
        // Dedup dropped_calls (same (name, caller) may appear across multiple
        // modules/files if the same site was visited twice).
        dropped_calls.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        dropped_calls.dedup();

        let inherited = super::tier0::inherited_call_targets(
            &dropped_calls,
            &supertypes_of,
            &fqn_by_id,
            &type_id_by_fqn,
            &parent_fqn_by_id,
            &method_by_type_and_name,
        );
        // Drop inherited edges that collide with an already-resolved
        // (src, tgt, kind): the Neo4j MERGE/SET writer applies rows in
        // order, so a later inherited row would overwrite a real, higher-
        // confidence edge's method/confidence. Genuinely-new inherited
        // edges (no pre-existing pair) are kept.
        let inherited = super::tier0::filter_new_inherited_edges(&all_resolved_edges, inherited);
        let inherited_count = inherited.len();
        all_resolved_edges.extend(inherited);
        if inherited_count > 0 {
            info!(
                job_id = %job_id,
                inherited_call_edges = inherited_count,
                "EXT-8-1: resolved dropped calls via supertype inheritance"
            );
        }
    }

    let mut resolved_calls = Vec::new();
    let mut resolved_refs = Vec::new();
    let mut resolved_impls = Vec::new();
    let mut resolved_routes = Vec::new();
    let mut resolved_makes_http_calls = Vec::new();
    for e in all_resolved_edges.into_iter() {
        match e.kind {
            EdgeKind::References => resolved_refs.push(e),
            EdgeKind::Implements | EdgeKind::Extends => resolved_impls.push(e),
            EdgeKind::RoutesTo => resolved_routes.push(e),
            EdgeKind::MakesHttpCall => resolved_makes_http_calls.push(e),
            _ => resolved_calls.push(e),
        }
    }
    // EXT-6c-3: Go structural interface satisfaction (resolution-level).
    // A type implements an interface iff it provides all the interface's
    // required methods (structural typing, no `implements` keyword in Go).
    // We fetch all Go `type` and `method` chunks for this repo and compute
    // the pairs in pure Rust, then append them to `resolved_impls` before
    // the single `create_implements_edges` call below.
    // `module_path` (= the chunk's directory == its Go package) is selected
    // so go_structural can scope interface-satisfaction PER PACKAGE; without
    // it, same-named types across packages conflate (BUG 1).
    // Filter mirrors `fetch_go_chunks`'s actual SQL verbatim (verified against
    // `akashic-store-pg/src/repos/chunk.rs`, not assumed from the method
    // name): `WHERE repo_name = $1 AND language = 'go' AND chunk_type IN
    // ('type', 'method')` — the chunk_type restriction matters because Go
    // structural-typing resolution only makes sense over type/method rows,
    // not e.g. `function` or `struct field` chunks that also carry
    // `language == "go"`.
    // Roadmap F, Task 7.5: sourced from `acc.chunk_index_source` (same
    // visibility fix as `chunk_index`/the EXT-8-1 maps above) so a reused Go
    // type/method chunk isn't invisible to structural-typing resolution.
    let go_rows: Vec<(Uuid, String, String, String, String)> = acc
        .chunk_index_source
        .iter()
        .filter(|r| {
            r.language.as_deref() == Some("go")
                && matches!(r.chunk_type.as_str(), "type" | "method")
        })
        .map(|r| {
            (
                r.id,
                r.name.clone(),
                r.chunk_type.clone(),
                r.content.clone(),
                r.module_path.clone(),
            )
        })
        .collect();
    if !go_rows.is_empty() {
        for (src_id, tgt_id) in crate::ingestion::go_structural::go_structural_implements(&go_rows)
        {
            resolved_impls.push(ResolvedEdge {
                src_chunk_id: src_id,
                tgt_chunk_id: tgt_id,
                kind: EdgeKind::Implements,
                confidence: 1.0,
                method: "structural".to_string(),
                line: None,
                ref_kind: Some("implements".to_string()),
            });
        }
    }

    info!(
        job_id = %job_id,
        call_edges = resolved_calls.len(),
        reference_edges = resolved_refs.len(),
        implements_edges = resolved_impls.len(),
        route_edges = resolved_routes.len(),
        makes_http_call_edges = resolved_makes_http_calls.len(),
        "Creating CALLS + REFERENCES + IMPLEMENTS + ROUTES_TO + MAKES_HTTP_CALL edges"
    );
    fn to_ingest_edges(edges: Vec<ResolvedEdge>) -> Vec<akashic_domain::types::IngestEdge> {
        edges
            .into_iter()
            .map(|e| akashic_domain::types::IngestEdge {
                src_chunk_id: e.src_chunk_id,
                tgt_chunk_id: e.tgt_chunk_id,
                confidence: e.confidence,
                method: e.method,
                ref_kind: e.ref_kind,
            })
            .collect()
    }

    acc.graph
        .calls_edges
        .extend(to_ingest_edges(resolved_calls));
    acc.graph
        .reference_edges
        .extend(to_ingest_edges(resolved_refs));
    acc.graph
        .implements_edges
        .extend(to_ingest_edges(resolved_impls));
    acc.graph
        .routes_to_edges
        .extend(to_ingest_edges(resolved_routes));
    acc.graph
        .http_call_edges
        .extend(to_ingest_edges(resolved_makes_http_calls));

    symbol_imports.sort();
    symbol_imports.dedup();
    acc.graph.symbol_import_edges.extend(symbol_imports);

    Ok(())
}

/// Stage 7: detect entry points and build + accumulate execution flows
/// (Roadmap F — RAM-first ingest, Task 6).
///
/// Used to read entry-point candidate chunks from Postgres
/// (`fetch_chunks_for_entry_points`) and the CALLS adjacency from Neo4j
/// (`FlowCallsRepo::load_calls_adjacency`, via `flows::build_flows`) — both
/// reads only ever worked because Stage 4/Stage 6 used to have already
/// written that data. Now both resolve purely from THIS RUN's in-memory
/// `IngestAccumulator`: `acc.chunk_index_source` for entry-point detection,
/// and `acc.graph.calls_edges` + `acc.chunk_index_source` (via
/// `flows::calls_adjacency_from_accumulator`) for adjacency (Roadmap F, Task
/// 7.5: switched from the delta-only `acc.pg.chunks` — see
/// `ChunkIndexEntry`'s doc comment for why). The result is appended to
/// `acc.graph.flows` instead of written via `FlowGraphRepo::store_flows`.
///
/// `#[allow(clippy::unused_async)]`: removing the Postgres read, the Neo4j
/// adjacency read (via `build_flows`), and the Neo4j `store_flows` write
/// (this task's entire mandate) also removed every `.await` this function
/// used to make — it is pure computation now. Kept `async` (not de-asynced)
/// per the brief's literal signature and to match the call site in
/// `pipeline.rs` (`stages::stage7_flows(...).await?`) and every sibling
/// `stageN_*` function's shape — the same precedent Task 5's
/// `stage6_call_edges` established.
#[allow(clippy::unused_async)]
pub(crate) async fn stage7_flows(
    job_id: Uuid,
    req: &IngestRequest,
    acc: &mut crate::ingestion::accumulator::IngestAccumulator,
) -> Result<()> {
    info!(job_id = %job_id, "Stage 7: Building execution flows");

    let mut entry_points = Vec::new();
    for row in &acc.chunk_index_source {
        let lang = row.language.as_deref().unwrap_or("");
        if let Some((entry_type, display_name)) =
            crate::ingestion::entry_points::detect(&row.content, &row.name, lang)
        {
            entry_points.push(crate::ingestion::entry_points::DetectedEntryPoint {
                chunk_id: row.id,
                chunk_name: row.name.clone(),
                module_path: row.module_path.clone(),
                entry_type,
                display_name,
            });
        }
    }

    info!(
        job_id = %job_id,
        entry_points = entry_points.len(),
        "Detected entry points"
    );

    if !entry_points.is_empty() {
        let adjacency = crate::ingestion::flows::calls_adjacency_from_accumulator(acc);
        let flows: Vec<crate::ingestion::flows::ExecutionFlow> = entry_points
            .iter()
            .filter_map(|ep| {
                let flow = crate::ingestion::flows::build_single_flow(ep, &adjacency);
                (!flow.steps.is_empty()).then_some(flow)
            })
            .collect();

        // Same FlowRecord/FlowStepRecord construction `store_flows` already
        // does (client-generated flow_id, position/depth per step) — just
        // appended to the accumulator instead of written to Neo4j.
        for flow in &flows {
            let flow_id = uuid::Uuid::new_v4().to_string();
            let steps: Vec<akashic_domain::types::FlowStepRecord> = flow
                .steps
                .iter()
                .map(|s| akashic_domain::types::FlowStepRecord {
                    chunk_id: s.chunk_id,
                    position: s.position,
                    depth: s.depth,
                })
                .collect();
            acc.graph.flows.push(akashic_domain::types::FlowRecord {
                flow_id,
                entry_chunk_id: flow.entry_point.chunk_id,
                display_name: flow.entry_point.display_name.clone(),
                entry_type: flow.entry_point.entry_type.clone(),
                repo_name: req.repo_name.clone(),
                step_count: steps.len(),
                truncated: flow.truncated,
                steps,
            });
        }
    }

    Ok(())
}

/// Stage 8: note staleness detection. The detection itself is non-fatal
/// (logged and swallowed). Carved verbatim from `run_inner`. (Roadmap F,
/// Task 8: the stage checkpoint save this used to make is gone — checkpoint/
/// resume machinery is retired entirely, so there is nothing left to record
/// progress for.)
pub(crate) async fn stage8_note_staleness(
    store: &IngestionStore,
    job_id: Uuid,
    req: &IngestRequest,
) -> Result<()> {
    info!(job_id = %job_id, "Stage 8: Checking note staleness");
    // note and note_health are port-backed fields on IngestionStore (A1 T8).
    if let Err(e) = akashic_curation::notes::health::detect_staleness(
        &store.note,
        &store.note_health,
        &req.repo_name,
    )
    .await
    {
        warn!(job_id = %job_id, error = %e, "Note staleness detection failed (non-fatal)");
    }

    Ok(())
}

/// Stage 9: community detection + summarization. Both detection and
/// summarization are non-fatal (logged and swallowed). Carved verbatim from
/// `run_inner`. (Roadmap F, Task 8: the stage checkpoint save this used to
/// make is gone — checkpoint/resume machinery is retired entirely, so there
/// is nothing left to record progress for.)
pub(crate) async fn stage9_communities(
    store: &IngestionStore,
    llm: &Arc<dyn LlmProvider>,
    embedder: &Arc<dyn EmbeddingProvider>,
    job_id: Uuid,
    req: &IngestRequest,
) -> Result<()> {
    info!(job_id = %job_id, "Stage 9: Community detection");
    match crate::community::detect_communities(
        &store.community_graph,
        &store.community,
        &req.repo_name,
    )
    .await
    {
        Ok(levels) => {
            if !levels.is_empty() {
                // Summarize communities (LLM calls — may take time)
                if let Err(e) = crate::community::summarize::summarize_communities(
                    &store.community,
                    llm.as_ref(),
                    embedder,
                    &req.repo_name,
                    &levels,
                )
                .await
                {
                    warn!(job_id = %job_id, error = %e, "Community summarization failed (non-fatal)");
                }
            }
        }
        Err(e) => {
            warn!(job_id = %job_id, error = %e, "Community detection failed (non-fatal)");
        }
    }

    Ok(())
}

#[cfg(test)]
mod resolve_import_target_tests {
    use super::resolve_import_target;
    use std::collections::{HashMap, HashSet};

    /// `(known_modules, file_to_module, dir_to_modules)` — the three indexes
    /// `resolve_import_target` consults.
    type Maps = (
        HashSet<&'static str>,
        HashMap<String, String>,
        HashMap<String, Vec<String>>,
    );

    /// A synthetic module layout mirroring akashic-record's own backend: module
    /// paths are plain directory paths, files carry a `.rs` extension.
    fn fixture() -> Maps {
        let known: HashSet<&str> = [
            "ingestion/extraction",
            "ingestion/extraction/resolution",
            "llm",
        ]
        .into_iter()
        .collect();
        let mut file_to_module = HashMap::new();
        file_to_module.insert(
            "ingestion/extraction/types.rs".to_string(),
            "ingestion/extraction".to_string(),
        );
        file_to_module.insert("llm/mod.rs".to_string(), "llm".to_string());
        let mut dir_to_modules: HashMap<String, Vec<String>> = HashMap::new();
        dir_to_modules.insert(
            "ingestion/extraction".into(),
            vec!["ingestion/extraction".into()],
        );
        dir_to_modules.insert(
            "ingestion/extraction/resolution".into(),
            vec!["ingestion/extraction/resolution".into()],
        );
        dir_to_modules.insert("llm".into(), vec!["llm".into()]);
        (known, file_to_module, dir_to_modules)
    }

    fn resolve(target: &str) -> Option<String> {
        let (k, f, d) = fixture();
        resolve_import_target(
            target,
            &k,
            &f,
            &d,
            &crate::ingestion::import_aliases::ImportAliases::default(),
        )
    }

    #[test]
    fn rust_glob_import_resolves_to_module_via_file() {
        // `use crate::ingestion::extraction::types::*` → specifier
        // `crate::ingestion::extraction::types`; the trailing `types` is the
        // FILE, found via `<path>.rs` → its owning module.
        assert_eq!(
            resolve("crate::ingestion::extraction::types"),
            Some("ingestion/extraction".into())
        );
    }

    #[test]
    fn rust_item_import_drops_item_then_resolves_file() {
        // `use crate::ingestion::extraction::types::ChunkCategory` — the
        // trailing `ChunkCategory` is an ITEM; the longest-first prefix scan
        // drops it and matches `types.rs`.
        assert_eq!(
            resolve("crate::ingestion::extraction::types::ChunkCategory"),
            Some("ingestion/extraction".into())
        );
    }

    #[test]
    fn rust_module_import_matches_known_module_directly() {
        assert_eq!(resolve("crate::llm"), Some("llm".into()));
        assert_eq!(
            resolve("crate::ingestion::extraction::resolution::resolve_call_edge"),
            Some("ingestion/extraction/resolution".into())
        );
    }

    #[test]
    fn rust_self_prefix_is_stripped_like_crate() {
        assert_eq!(resolve("self::llm"), Some("llm".into()));
    }

    #[test]
    fn external_crate_paths_resolve_to_nothing() {
        // std / third-party crates are not repo modules → no IMPORTS_FROM edge.
        assert_eq!(resolve("std::collections::HashMap"), None);
        assert_eq!(resolve("tree_sitter::Node"), None);
    }

    #[test]
    fn super_relative_paths_are_skipped_not_guessed() {
        // `super::` needs the importing module for context we don't have here;
        // skip rather than emit a wrong edge.
        assert_eq!(resolve("super::chunker"), None);
        assert_eq!(resolve("crate::a::super::b"), None);
    }

    #[test]
    fn plain_file_path_targets_still_resolve() {
        // Regression guard: `::`-free (C/C++/Python) targets keep working.
        assert_eq!(resolve("llm"), Some("llm".into()));
        assert_eq!(
            resolve("ingestion/extraction"),
            Some("ingestion/extraction".into())
        );
    }

    // ── alias-aware tests ────────────────────────────────────────────────────

    fn resolve_with_aliases(
        target: &str,
        aliases: crate::ingestion::import_aliases::ImportAliases,
    ) -> Option<String> {
        let (k, f, d) = fixture();
        resolve_import_target(target, &k, &f, &d, &aliases)
    }

    #[test]
    fn ts_alias_resolves_through_to_module() {
        // "@/ingestion/extraction/types" with alias @/→src/ → "src/ingestion/extraction/types"
        // which is NOT in the fixture; but "@/llm" → "llm" which IS.
        use crate::ingestion::import_aliases::ImportAliases;
        let aliases = ImportAliases {
            ts_paths: vec![("@/".to_string(), "".to_string())],
            ..Default::default()
        };
        // After alias rewrite: "@/llm" → "llm"; dir_to_modules["llm"] → Some("llm").
        assert_eq!(resolve_with_aliases("@/llm", aliases), Some("llm".into()));
    }

    #[test]
    fn cargo_crate_alias_resolves_to_member_module() {
        // Workspace crate `mylib` lives at `ingestion/extraction`.
        // `mylib::resolution::something` → `ingestion/extraction/resolution/something`
        // → dir_to_modules["ingestion/extraction/resolution"] → Some("ingestion/extraction/resolution").
        use crate::ingestion::import_aliases::ImportAliases;
        let mut crates = std::collections::HashMap::new();
        crates.insert("mylib".to_string(), "ingestion/extraction".to_string());
        let aliases = ImportAliases {
            cargo_crates: crates,
            ..Default::default()
        };
        assert_eq!(
            resolve_with_aliases("mylib::resolution", aliases),
            Some("ingestion/extraction/resolution".into())
        );
    }
}

#[cfg(test)]
mod completeness_tests {
    use super::{FailedFile, completeness_ratio};

    #[test]
    fn ratio_is_one_when_nothing_failed() {
        assert_eq!(completeness_ratio(5, &[]), 1.0);
    }

    #[test]
    fn ratio_reflects_failed_fraction() {
        let failed = vec![FailedFile {
            path: "src/broken.rs".into(),
            reason: "parse error".into(),
        }];
        assert!((completeness_ratio(3, &failed) - (2.0 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn ratio_is_one_for_zero_total_files() {
        // An empty repo isn't a completeness failure — nothing was attempted,
        // nothing failed. Avoids a 0/0 NaN.
        assert_eq!(completeness_ratio(0, &[]), 1.0);
    }
}

#[cfg(test)]
mod crawl_pages_tests {
    use crate::ingestion::adapter::generic_web::{GenericWebAdapter, url_to_title};
    use crate::ingestion::adapter::{CrawlLimits, SiteAdapter, SourceSpec};

    // Offline guard: GenericWebAdapter::discover on a 0-page-budget returns
    // empty without any network call (the max_pages check trips on entry).
    #[tokio::test]
    async fn discover_respects_zero_page_budget_offline() {
        let a = GenericWebAdapter::new();
        let src = SourceSpec {
            repo_name: "r".into(),
            seed_url: "https://example.com/".into(),
        };
        let limits = CrawlLimits {
            depth: 0,
            max_pages: 0,
            delay_ms: 0,
            url_pattern: None,
        };
        let pages = a.discover(&src, &limits).await.unwrap();
        assert!(pages.is_empty());
    }

    #[test]
    fn ingest_pages_titles_come_from_url() {
        // Guards the title parity contract ingest_pages relies on.
        assert_eq!(url_to_title("https://d/x/y"), "y");
    }

    // Regression guard for the request-budget fix: a positive page budget must
    // not spin or flood when the seed is SSRF-blocked. The fetch fails, no links
    // are extracted, the queue drains, and discover returns empty. Offline — the
    // SSRF guard rejects the literal private IP before any socket is opened.
    #[tokio::test]
    async fn discover_blocked_seed_terminates_and_saves_nothing() {
        let a = GenericWebAdapter::new();
        let src = SourceSpec {
            repo_name: "r".into(),
            seed_url: "http://10.17.250.12/".into(),
        };
        let limits = CrawlLimits {
            depth: 2,
            max_pages: 5,
            delay_ms: 0,
            url_pattern: None,
        };
        let pages = a.discover(&src, &limits).await.unwrap();
        assert!(pages.is_empty());
    }
}
