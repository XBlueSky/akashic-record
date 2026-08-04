//! Async derive job (Task 8, B3): turns a stored raw corpus version into the
//! searchable Doc space — documents + sections + embeddings (PG), Neo4j
//! graph nodes, and EXPLAINS anchoring — without blocking the publish HTTP
//! call that triggers it.
//!
//! [`run_derive`] is the whole job. It reads the raw layer through
//! [`CorpusStore`] (`list_md_paths` / `get_file`) — never writes to it — so a
//! derive failure can never corrupt or roll back the corpus a CI packer
//! already pushed: `corpus_versions` / `corpus_files` stay exactly as
//! `CorpusIngestService::validate_and_store` (Task 5) left them. The only
//! `corpus_versions` columns this job ever touches are the three
//! `derive_status` / `derive_error` / `derive_job_id` lifecycle columns, via
//! `claim_derive_running` (the `Running` transition, CAS-guarded — see
//! `run_derive`'s doc comment) and `set_derive_status` (`Complete`/`Failed`).
//!
//! [`RealCorpusDeriveSpawner`] is the production [`CorpusDeriveSpawner`]
//! (Task 5's `NoopCorpusDeriveSpawner` placeholder, replaced) — it fires
//! [`run_derive`] on a background `tokio::spawn` and returns immediately.
//!
//! **Per-repo serialization** (review fix): `claim_derive_running`'s CAS
//! only serializes two runs for the SAME `version_id`. Two publishes for the
//! SAME repo with DIFFERENT shas each get their own `version_id`, so the CAS
//! does not serialize them — yet `clean_repo_corpus_docs` is repo-scoped.
//! `run_derive_inner` closes that window with a per-repo Postgres
//! session-level advisory lock (see its doc comment) plus a
//! superseded-version bail check taken under that lock.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing::{error, info, warn};
use uuid::Uuid;

use akashic_domain::ports::{CorpusStore, EdgeRepo, IngestionJobRepo};
use akashic_domain::types::corpus::DeriveStatus;
use akashic_extraction::special::sections::{RawSection, parse_markdown_sections};
use akashic_store_neo4j::Neo4jEdgeRepo;

use super::corpus::CorpusDeriveSpawner;
use super::doc_store::DocStore;

/// Dependencies for one [`run_derive`] invocation.
///
/// This is deliberately built fresh per run (by [`RealCorpusDeriveSpawner`]
/// or by a test/the retry path) rather than being a long-lived shared
/// object, which is what lets it carry per-run context — `job_id` in
/// particular — through a `run_derive(deps, version_id)` signature that
/// otherwise has nowhere to put it. See `job_id`'s own doc comment for why
/// that matters.
pub struct DeriveDeps {
    pub corpus_store: Arc<dyn CorpusStore>,
    pub doc_store: Arc<DocStore>,
    pub ingestion_job_repo: Arc<dyn IngestionJobRepo>,
    /// Job id to persist alongside every `set_derive_status` write this run
    /// makes. `CorpusStore::set_derive_status` is an unconditional
    /// `UPDATE ... SET derive_status=$2, derive_error=$3, derive_job_id=$4`
    /// — passing `None` here would silently null out a job id a spawner
    /// already recorded. Threading it through `DeriveDeps` (rather than a
    /// third `run_derive` parameter, which the brief's signature doesn't
    /// have room for) lets [`RealCorpusDeriveSpawner`] generate the id
    /// before spawning and have `run_derive`'s own first status write
    /// persist it — no separate "record the job id" call needed. `None` is
    /// fine for ad-hoc callers (tests, the retry endpoint's direct spawn
    /// path) that don't track a job id themselves.
    pub job_id: Option<Uuid>,
}

/// Outcome of the guarded body ([`run_derive_locked`]) on success — either
/// the Doc space was actually rebuilt, or this run bailed because a newer
/// publish for the same repo superseded it while it waited on the per-repo
/// derive lock. [`run_derive`] maps this to the `derive_status` it persists.
enum DeriveOutcome {
    Completed,
    Superseded,
}

/// Run the derive job for one corpus version: `Running` → rebuild the Doc
/// space (documents/sections/embeddings/EXPLAINS) → `Complete`, or `Failed`
/// with `derive_error` set on any error, or `Superseded` if a newer publish
/// for the same repo won the rebuild instead. Never touches `corpus_files` /
/// `corpus_versions`' raw columns (see module doc comment).
///
/// **Concurrency guards** — two independent ones, for two independent races:
///
/// 1. **Same-version CAS**: the transition into `Running` is an atomic CAS
///    claim (`CorpusStore::claim_derive_running`), not an unconditional
///    write. Two `run_derive` calls for the SAME `version_id` racing each
///    other (retry-vs-retry, or retry-vs-a-fresh-publish's auto-derive
///    spawn) would otherwise both run `clean_repo_corpus_docs` + the
///    per-page insert loop interleaved. A run that loses the claim bails
///    immediately (logged, not an error) without touching the Doc space at
///    all, closing that window. This alone does NOT serialize two DIFFERENT
///    versions of the same repo — see the next guard.
/// 2. **Per-repo advisory lock**: `run_derive_locked` holds a Postgres
///    session-level advisory lock (keyed by `hashtext(repo_name)`) across
///    the whole clean+rebuild, and re-checks — under that lock — that
///    `version_id` is still the repo's latest version before touching
///    anything. Two publishes for the same repo with DIFFERENT shas each
///    get their own `version_id`, so guard 1 doesn't serialize them, but
///    `clean_repo_corpus_docs` is repo-scoped — without this second guard
///    both runs' clean+rebuild would interleave into duplicate or
///    stale-version documents (`create_document` has no upsert-by-path
///    uniqueness to catch it). The loser of the lock race (or the run whose
///    version got superseded while it waited) bails as `Superseded` instead
///    of rebuilding a version that's no longer latest.
pub async fn run_derive(deps: DeriveDeps, version_id: Uuid) -> Result<()> {
    let claimed = deps
        .corpus_store
        .claim_derive_running(version_id, deps.job_id)
        .await
        .context("failed to claim derive_status=running")?;
    if !claimed {
        info!(
            version_id = %version_id,
            "derive already in progress for {version_id}; this run is a no-op bail"
        );
        return Ok(());
    }

    match run_derive_inner(&deps, version_id).await {
        Ok(DeriveOutcome::Completed) => {
            deps.corpus_store
                .set_derive_status(version_id, DeriveStatus::Complete, None, deps.job_id)
                .await
                .context("failed to set derive_status=complete")?;
            Ok(())
        }
        Ok(DeriveOutcome::Superseded) => {
            deps.corpus_store
                .set_derive_status(version_id, DeriveStatus::Superseded, None, deps.job_id)
                .await
                .context("failed to set derive_status=superseded")?;
            Ok(())
        }
        Err(e) => {
            let message = format!("{e:#}");
            // Best-effort: if even the failure-status write fails, log it
            // but still return the ORIGINAL derive error — that's the
            // actionable one, not a status-write plumbing failure masking it.
            if let Err(status_err) = deps
                .corpus_store
                .set_derive_status(
                    version_id,
                    DeriveStatus::Failed,
                    Some(&message),
                    deps.job_id,
                )
                .await
            {
                error!(
                    version_id = %version_id,
                    error = %status_err,
                    "failed to record derive failure status"
                );
            }
            Err(e)
        }
    }
}

async fn run_derive_inner(deps: &DeriveDeps, version_id: Uuid) -> Result<DeriveOutcome> {
    let meta = deps
        .corpus_store
        .get_version_by_id(version_id)
        .await
        .context("failed to look up corpus version")?
        .ok_or_else(|| anyhow::anyhow!("corpus version {version_id} not found"))?;
    let repo = meta.repo_name.clone();

    // ── Per-repo derive lock ─────────────────────────────────────────────
    //
    // A dedicated connection (not a borrow from the shared pool) holds a
    // Postgres session-level advisory lock across the whole clean+rebuild in
    // `run_derive_locked`. `pg_advisory_lock` is session-scoped rather than
    // xact-scoped deliberately: the guarded work spans many statements
    // across two different stores (PG + Neo4j) and embedding-provider
    // calls, so it can't be a single wrapping transaction.
    //
    // `close_on_drop()` is set immediately, before the lock is even
    // acquired, so that no matter how this function exits — the happy path,
    // an early `?` return, or a panic-unwind — the underlying session is
    // closed rather than returned to the pool. Postgres releases every
    // session-level advisory lock a connection holds when that connection's
    // session ends, so this is what actually GUARANTEES the lock can never
    // leak across derive runs (a leaked lock would wedge every future derive
    // for this repo), independent of whether the explicit unlock below runs
    // or succeeds.
    let mut lock_conn = deps
        .doc_store
        .pg
        .acquire()
        .await
        .context("failed to acquire a dedicated connection for the per-repo derive lock")?;
    lock_conn.close_on_drop();
    sqlx::query("SELECT pg_advisory_lock(hashtext($1))")
        .bind(&repo)
        .execute(&mut *lock_conn)
        .await
        .context("failed to acquire per-repo derive advisory lock")?;

    let outcome = run_derive_locked(deps, version_id, &repo, &meta.sha).await;

    // Best-effort prompt release. `close_on_drop` above is what actually
    // guarantees the lock is never leaked (see its comment); this just spares
    // any other waiter the latency of the connection's async close teardown.
    if let Err(e) = sqlx::query("SELECT pg_advisory_unlock(hashtext($1))")
        .bind(&repo)
        .execute(&mut *lock_conn)
        .await
    {
        warn!(
            repo = %repo,
            version_id = %version_id,
            error = %e,
            "failed to explicitly release the per-repo derive advisory lock; \
             the dedicated connection is close-on-drop, so the session-level \
             lock will still be released when the connection closes"
        );
    }

    outcome
}

/// The guarded body of [`run_derive_inner`]: re-checks — now that the
/// per-repo lock is held — that `version_id` is still the repo's latest
/// version before touching the Doc space at all. Another publish for the
/// same repo may have superseded it while this run waited on the lock; if
/// so, this run bails cleanly (`DeriveOutcome::Superseded`) rather than
/// wiping and rebuilding a version that's no longer current — the newer
/// version's own derive run (which is either already running under this
/// same lock, having queued behind this one, or about to acquire it next)
/// is the one that ends up owning the Doc space.
async fn run_derive_locked(
    deps: &DeriveDeps,
    version_id: Uuid,
    repo: &str,
    corpus_sha: &str,
) -> Result<DeriveOutcome> {
    let latest = deps
        .corpus_store
        .resolve_version(repo, "latest")
        .await
        .context("failed to re-check latest corpus version under the derive lock")?;
    if latest.as_ref().map(|m| m.id) != Some(version_id) {
        info!(
            version_id = %version_id,
            repo,
            corpus_sha,
            "corpus version was superseded by a newer publish while this run waited for \
             the per-repo derive lock; skipping rebuild — the newer version's derive run \
             owns the Doc space instead"
        );
        return Ok(DeriveOutcome::Superseded);
    }

    // Corpus-scoped clean BEFORE rebuilding: makes re-derivation idempotent
    // (no stale sections from a previous run) without touching documents
    // another ingestion path (website crawl) created for the same repo.
    deps.doc_store
        .clean_repo_corpus_docs(repo)
        .await
        .context("failed to clean existing corpus docs before rebuild")?;

    let md_paths = deps
        .corpus_store
        .list_md_paths(version_id)
        .await
        .context("failed to list corpus markdown paths")?;

    // B5: best-effort code_sha staleness signal. `None` means the repo was
    // never code-ingested — EXPLAINS anchoring is naturally a no-op in that
    // case too (the symbol lookups `store_sections` runs internally find
    // nothing to link against), so this is purely a log-and-skip note, not
    // a branch that changes what documents/sections get stored.
    let code_sha = deps
        .ingestion_job_repo
        .get_last_job_git_ref(repo)
        .await
        .context("failed to look up code snapshot git_ref for EXPLAINS code_sha annotation")?;
    match code_sha.as_deref() {
        Some(sha) if sha != corpus_sha => info!(
            repo,
            corpus_sha,
            code_sha = sha,
            "EXPLAINS anchoring against a code snapshot at a different sha than the corpus \
             (code_sha recorded on edges as a staleness signal)"
        ),
        Some(_) => {}
        None => info!(
            repo,
            "repo has no ingested code snapshot; EXPLAINS anchoring is a no-op for this derive run"
        ),
    }

    let edge_repo: Arc<dyn EdgeRepo> = Arc::new(Neo4jEdgeRepo::new(deps.doc_store.neo4j.clone()));

    for path in &md_paths {
        let file = deps
            .corpus_store
            .get_file(version_id, path)
            .await
            .with_context(|| format!("failed to fetch corpus file {path}"))?
            .ok_or_else(|| anyhow::anyhow!("corpus file {path} listed but not found"))?;
        let content = std::str::from_utf8(&file.content)
            .with_context(|| format!("corpus file {path} is not valid UTF-8"))?;

        let sections = parse_markdown_sections(content);
        let title = page_title(path, &sections);

        let doc_id = deps
            .doc_store
            .create_document(repo, &title, "corpus", Some(path), None)
            .await
            .with_context(|| format!("failed to create document for {path}"))?;

        let stored = deps
            .doc_store
            .store_sections(doc_id, repo, &title, &sections, None, None)
            .await
            .with_context(|| format!("failed to store sections for {path}"))?;

        if let Some(sha) = &code_sha {
            for (section_id, _heading) in &stored {
                if let Err(e) = edge_repo.annotate_explains_code_sha(*section_id, sha).await {
                    warn!(
                        section_id = %section_id,
                        error = %e,
                        "best-effort EXPLAINS code_sha annotation failed"
                    );
                }
            }
        }
    }

    Ok(DeriveOutcome::Completed)
}

/// Derive a document title for one corpus markdown page.
///
/// `NavTree.groups[].pages[].title` (the manifest-declared page title) would
/// be the ideal source, but `CorpusStore`'s port never returns the persisted
/// `nav` JSONB back out for a bare `version_id` — only `insert_version`
/// accepts it as input, nothing reads it back. Falling back to the page's
/// own first top-level heading (its natural title), and to the file's path
/// stem when the page has no heading at all
/// (`parse_markdown_sections`' documented "Content" fallback), is the best
/// title obtainable through the interface this task consumes.
fn page_title(path: &str, sections: &[RawSection]) -> String {
    match sections.first() {
        Some(s) if s.heading != "Content" => s.heading.clone(),
        _ => path
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .trim_end_matches(".md")
            .to_string(),
    }
}

// ── Real spawner ──────────────────────────────────────────────────────────

/// Production [`CorpusDeriveSpawner`]: fires [`run_derive`] on a background
/// `tokio::spawn` and returns immediately (fire-and-forget), logging on
/// failure. Replaces Task 5's `NoopCorpusDeriveSpawner` placeholder.
pub struct RealCorpusDeriveSpawner {
    corpus_store: Arc<dyn CorpusStore>,
    doc_store: Arc<DocStore>,
    ingestion_job_repo: Arc<dyn IngestionJobRepo>,
}

impl RealCorpusDeriveSpawner {
    pub fn new(
        corpus_store: Arc<dyn CorpusStore>,
        doc_store: Arc<DocStore>,
        ingestion_job_repo: Arc<dyn IngestionJobRepo>,
    ) -> Self {
        Self {
            corpus_store,
            doc_store,
            ingestion_job_repo,
        }
    }
}

#[async_trait]
impl CorpusDeriveSpawner for RealCorpusDeriveSpawner {
    async fn spawn(&self, version_id: Uuid) -> Option<Uuid> {
        let job_id = Uuid::new_v4();
        let deps = DeriveDeps {
            corpus_store: self.corpus_store.clone(),
            doc_store: self.doc_store.clone(),
            ingestion_job_repo: self.ingestion_job_repo.clone(),
            job_id: Some(job_id),
        };
        tokio::spawn(async move {
            if let Err(e) = run_derive(deps, version_id).await {
                error!(
                    version_id = %version_id,
                    job_id = %job_id,
                    error = %e,
                    "corpus derive job failed"
                );
            }
        });
        Some(job_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_title_prefers_first_top_level_heading() {
        let sections = vec![RawSection {
            heading: "Welcome".to_string(),
            content: String::new(),
            depth: 0,
            position: 0,
            children: Vec::new(),
            tags: Vec::new(),
        }];
        assert_eq!(page_title("guide/intro.md", &sections), "Welcome");
    }

    #[test]
    fn page_title_falls_back_to_path_stem_when_no_heading() {
        let sections = vec![RawSection {
            heading: "Content".to_string(),
            content: "no headings here".to_string(),
            depth: 0,
            position: 0,
            children: Vec::new(),
            tags: Vec::new(),
        }];
        assert_eq!(page_title("guide/plain.md", &sections), "plain");
    }

    #[test]
    fn page_title_falls_back_to_path_stem_when_no_sections_at_all() {
        assert_eq!(page_title("index.md", &[]), "index");
    }
}
