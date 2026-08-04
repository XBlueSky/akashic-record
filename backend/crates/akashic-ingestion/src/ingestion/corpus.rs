//! `CorpusIngestService` — the untrusted-input entry point for the docs-kit
//! corpus store (Task 5, B2 core pipeline). Turns an uploaded
//! `corpus.tar.gz` (push path, Task 7's HTTP handler) or an in-memory file
//! tree already resolved by the platform (pull path, Task 9) into a
//! validated, persisted corpus version.
//!
//! `ingest_artifact`'s tar extraction is the security-critical boundary of
//! the whole feature: every entry is validated (type, path safety, size)
//! BEFORE its bytes are trusted anywhere else, and byte accounting happens
//! incrementally as the gzip stream is decoded — never by fully buffering
//! an entry (or the whole archive) and checking its size afterward. A tar
//! entry's header can freely declare a huge size backed by a tiny
//! highly-compressible payload (a decompression bomb); trusting that header
//! for allocation, or calling `read_to_end` before checking a cap, would
//! let such an entry exhaust memory before this code ever gets a chance to
//! reject it.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use uuid::Uuid;

use akashic_domain::algos::corpus_contract::{check_links, parse_nav};
use akashic_domain::ports::corpus::{CorpusStore, InsertOutcome, NewCorpusFile, NewCorpusVersion};
use akashic_domain::types::corpus::{
    ContractFinding, CorpusManifest, CorpusSource, NavTree, Severity,
};

/// Maximum total uncompressed bytes across every entry in one artifact (A2).
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum uncompressed size of any single file entry (A2).
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum number of entries (files + directories) in one artifact (A2).
const MAX_ENTRY_COUNT: usize = 2000;
/// Chunk size used when streaming an entry's bytes out of the gzip/tar
/// decoders. Bounds how far a single `read()` call can push a running total
/// past a cap before it's checked again — the cap is enforced within one
/// chunk's slack, never after buffering an entire (attacker-declared) entry.
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// The corpus packer's manifest is always the tar root entry named exactly
/// this; it is captured into `NewCorpusVersion::manifest` (a dedicated JSONB
/// column) and is deliberately excluded from `NewCorpusVersion::files` so it
/// isn't ALSO persisted as an ordinary `corpus_files` row.
const MANIFEST_ENTRY: &str = "manifest.json";

static MARKDOWN_GLOB: LazyLock<globset::GlobMatcher> = LazyLock::new(|| {
    globset::Glob::new("**/*.md")
        .expect("literal glob is valid")
        .compile_matcher()
});

// ── Outcome types ────────────────────────────────────────────────────────

/// Successful outcome of [`CorpusIngestService::ingest_artifact`] /
/// [`CorpusIngestService::ingest_tree`].
#[derive(Debug, Clone)]
pub struct PublishAccepted {
    pub repo: String,
    pub version: String,
    pub sha: String,
    pub pages: usize,
    pub assets: usize,
    pub warnings: Vec<ContractFinding>,
    pub derive_job_id: Option<Uuid>,
    /// `true` when this exact `(repo, sha)` was already ingested and the
    /// store returned the pre-existing version (`InsertOutcome::Existing`)
    /// rather than inserting a new row — the idempotent-replay path, not an
    /// error. No new derive job is spawned on a replay (see
    /// [`CorpusIngestService`]'s doc comment on the derive-spawner seam).
    pub replayed: bool,
}

/// Successful outcome of [`CorpusIngestService::validate_extracted`] (Task 1,
/// docs-kit Plan 2 B2 addendum) — the dry-run twin of [`PublishAccepted`].
/// Carries exactly the fields a `check --remote` caller needs to report
/// findings; no `derive_job_id` or `replayed` because a dry run never inserts
/// a version or spawns a derive job, so neither concept applies.
#[derive(Debug, Clone)]
pub struct ValidationReport {
    pub repo: String,
    pub version: String,
    pub sha: String,
    pub pages: usize,
    pub assets: usize,
    pub warnings: Vec<ContractFinding>,
}

/// Machine-readable rejection reason for [`PublishRejection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectCode {
    /// `manifest.json` failed schema validation (missing/extra/mistyped
    /// field) or wasn't present in the archive at all.
    Schema,
    /// `manifest.repo` didn't match the caller-supplied `expected_repo`.
    RepoMismatch,
    /// A size/count cap (A2) was exceeded: single file, total uncompressed
    /// bytes, or entry count.
    TooLarge,
    /// A contract-validation rule failed: unsafe tar entry (type or path),
    /// non-UTF-8 markdown, missing/malformed nav, or a `Severity::Error`
    /// link/anchor finding.
    Contract,
    /// The store (or another infra dependency) failed — e.g. Postgres
    /// unreachable. Distinct from the four business-validation rejections
    /// above so a caller (Task 7's HTTP handler) can map it to a 5xx
    /// instead of a 4xx. Not one of the brief's originally enumerated
    /// variants; added because this fn's signature is a plain
    /// `Result<PublishAccepted, PublishRejection>` with no third,
    /// infra-error channel — mirrors this codebase's own
    /// `akashic_domain::error::DomainError::Internal(#[from] anyhow::Error)`
    /// catch-all convention. Flagged here as a deliberate, documented
    /// extension, not a silent guess.
    Internal,
}

/// Rejection outcome of [`CorpusIngestService::ingest_artifact`] /
/// [`CorpusIngestService::ingest_tree`]. `findings` holds exactly the
/// finding(s) that caused the rejection (not unrelated warnings that didn't
/// contribute to it — those only ever appear in [`PublishAccepted::warnings`]
/// on the accept path).
#[derive(Debug, Clone)]
pub struct PublishRejection {
    pub code: RejectCode,
    pub message: String,
    pub findings: Vec<ContractFinding>,
}

impl PublishRejection {
    fn plain(code: RejectCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            findings: Vec::new(),
        }
    }

    fn one_finding(code: RejectCode, message: impl Into<String>, finding: ContractFinding) -> Self {
        Self {
            code,
            message: message.into(),
            findings: vec![finding],
        }
    }

    fn many_findings(
        code: RejectCode,
        message: impl Into<String>,
        findings: Vec<ContractFinding>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            findings,
        }
    }
}

fn unsafe_entry_rejection(file: impl Into<String>, detail: impl Into<String>) -> PublishRejection {
    let file = file.into();
    let detail = detail.into();
    PublishRejection::one_finding(
        RejectCode::Contract,
        detail.clone(),
        ContractFinding {
            rule: "unsafe_entry".to_string(),
            file,
            line: None,
            detail,
            severity: Severity::Error,
        },
    )
}

fn too_large_rejection(detail: impl Into<String>) -> PublishRejection {
    let detail = detail.into();
    PublishRejection::one_finding(
        RejectCode::TooLarge,
        detail.clone(),
        ContractFinding {
            rule: "too_large".to_string(),
            file: String::new(),
            line: None,
            detail,
            severity: Severity::Error,
        },
    )
}

// ── Derive-spawner seam ──────────────────────────────────────────────────

/// Seam for triggering the derive job (Task 8, not yet built) after a
/// version lands. Injected as `Arc<dyn CorpusDeriveSpawner>` so this crate
/// doesn't need to depend on Task 8's implementation to compile or be
/// tested; whatever composes `CorpusIngestService` in production supplies
/// the real spawner once Task 8 exists. Only called on a fresh insert
/// (`InsertOutcome::Inserted`) — a replay of an already-ingested `(repo,
/// sha)` does not spawn a second derive job for the same version.
#[async_trait]
pub trait CorpusDeriveSpawner: Send + Sync {
    /// Returns the spawned job's id, or `None` if nothing was spawned.
    async fn spawn(&self, version_id: Uuid) -> Option<Uuid>;
}

/// Always declines to spawn a derive job. The placeholder spawner for this
/// task's tests and for any composition root until Task 8 supplies the real
/// one.
pub struct NoopCorpusDeriveSpawner;

#[async_trait]
impl CorpusDeriveSpawner for NoopCorpusDeriveSpawner {
    async fn spawn(&self, _version_id: Uuid) -> Option<Uuid> {
        None
    }
}

// ── Retry-derive outcome ─────────────────────────────────────────────────

/// Outcome of [`CorpusIngestService::retry_derive`] (Task 8's
/// `POST /api/v1/docs/{repo}/derive/retry`).
#[derive(Debug, Clone)]
pub enum RetryDeriveOutcome {
    /// The repo has no ingested corpus version at all — nothing to retry.
    NotFound,
    /// The repo's latest version was resolved and handed to the derive
    /// spawner. `derive_job_id` is `None` only if the spawner itself
    /// declined to spawn (e.g. `NoopCorpusDeriveSpawner`, never the real one
    /// in production).
    Spawned {
        version_id: Uuid,
        derive_job_id: Option<Uuid>,
    },
}

// ── CorpusIngestService ──────────────────────────────────────────────────

pub struct CorpusIngestService {
    store: Arc<dyn CorpusStore>,
    derive_spawner: Arc<dyn CorpusDeriveSpawner>,
}

impl CorpusIngestService {
    pub fn new(store: Arc<dyn CorpusStore>, derive_spawner: Arc<dyn CorpusDeriveSpawner>) -> Self {
        Self {
            store,
            derive_spawner,
        }
    }

    /// Push entry point: an uploaded `corpus.tar.gz`, untarred and
    /// validated here before anything is trusted.
    pub async fn ingest_artifact(
        &self,
        gz: bytes::Bytes,
        expected_repo: &str,
        is_tagged: bool,
    ) -> Result<PublishAccepted, PublishRejection> {
        let extracted = extract_tar_gz(&gz)?;
        self.ingest_extracted(extracted, expected_repo, is_tagged)
            .await
    }

    /// Push entry point, split at the extraction boundary (Task 7): the HTTP
    /// publish handler needs `manifest.repo`/`manifest.sha` (to peek and to
    /// build its idempotency key) BEFORE reserving and calling into the
    /// store, so it runs `extract_tar_gz` itself — inside `spawn_blocking`,
    /// since untarring is the CPU-heavy, security-critical step (see this
    /// module's doc comment) — and hands the result here. Extraction thus
    /// happens exactly once; this fn is the shared tail `ingest_artifact`
    /// also calls after extracting internally.
    pub async fn ingest_extracted(
        &self,
        extracted: ExtractedArtifact,
        expected_repo: &str,
        is_tagged: bool,
    ) -> Result<PublishAccepted, PublishRejection> {
        self.validate_and_store(
            extracted.manifest,
            extracted.files,
            expected_repo,
            CorpusSource::Push,
            is_tagged,
        )
        .await
    }

    /// The docs-kit kit's `check --remote` dry-run twin of
    /// [`Self::ingest_extracted`] (Task 1, B2 addendum): runs exactly the
    /// validation half of [`Self::validate_and_store`] — manifest/token repo
    /// cross-check, markdown UTF-8 validation, nav parsing, link/anchor
    /// contract checking — via the same [`validate_core`] both methods share,
    /// and then STOPS. No `store.insert_version`, no derive-job spawn, no
    /// idempotency: validating an artifact must never leave a trace a
    /// subsequent real publish of the same artifact would have to work
    /// around.
    ///
    /// `#[allow(clippy::unused_async)]`: `validate_core` is pure computation
    /// (no store/derive-spawner dependency), so this fn makes no `.await`
    /// call of its own. Kept `async` (not de-asynced) to match every sibling
    /// method on this struct (`ingest_extracted`, `ingest_tree`,
    /// `retry_derive`) and the brief's literal signature — the same
    /// precedent `stages::stage7_flows` established for an identical
    /// "validation-only, mandate removed every await" situation.
    #[allow(clippy::unused_async)]
    pub async fn validate_extracted(
        &self,
        extracted: ExtractedArtifact,
        expected_repo: &str,
    ) -> Result<ValidationReport, PublishRejection> {
        let ExtractedArtifact { manifest, files } = extracted;
        let core = validate_core(&manifest, &files, expected_repo)?;
        Ok(ValidationReport {
            repo: manifest.repo,
            version: manifest.version,
            sha: manifest.sha,
            pages: core.pages,
            assets: core.assets,
            warnings: core.warnings,
        })
    }

    /// Pull entry point: a file tree the platform already resolved
    /// in-memory, plus a manifest it built itself (B4) — no untar, and no
    /// `expected_repo` check (the manifest IS what the platform decided to
    /// pull; there's no separate "expected" value to cross-check it
    /// against). `is_tagged` is not a parameter of this entry point per its
    /// spec'd signature; pulled corpora represent "whatever's there now" on
    /// a live upstream mirror, never a tagged release, so it's hardcoded to
    /// `false` here — a deliberate default, documented rather than guessed.
    pub async fn ingest_tree(
        &self,
        manifest: CorpusManifest,
        files: Vec<NewCorpusFile>,
        source: CorpusSource,
    ) -> Result<PublishAccepted, PublishRejection> {
        // No separate "expected" repo to cross-check against here (see this
        // fn's doc comment) — pass the manifest's own repo through as
        // `expected_repo` so `validate_and_store`'s shared repo check is a
        // trivial no-op, identical to this entry point never having checked
        // at all.
        let expected_repo = manifest.repo.clone();
        self.validate_and_store(manifest, files, &expected_repo, source, false)
            .await
    }

    /// Re-trigger the derive job for a repo's latest published corpus
    /// version (Task 8's `POST /api/v1/docs/{repo}/derive/retry`).
    ///
    /// Unlike the spawn-on-insert path in `validate_and_store`, this is
    /// deliberately callable regardless of the version's current
    /// `derive_status` — a caller retrying after a `Failed` derive (the
    /// primary use case) needs exactly this, and re-running a `Complete`
    /// derive is just a redundant rebuild (idempotent — see
    /// `corpus_derive::run_derive`'s corpus-scoped clean), not an error.
    pub async fn retry_derive(&self, repo: &str) -> anyhow::Result<RetryDeriveOutcome> {
        match self.store.resolve_version(repo, "latest").await? {
            None => Ok(RetryDeriveOutcome::NotFound),
            Some(meta) => {
                let derive_job_id = self.derive_spawner.spawn(meta.id).await;
                Ok(RetryDeriveOutcome::Spawned {
                    version_id: meta.id,
                    derive_job_id,
                })
            }
        }
    }

    /// Shared tail of both entry points: runs [`validate_core`] (Task 1's
    /// shared validation seam — manifest/repo cross-check, UTF-8, nav
    /// parsing, link/anchor contract checking) and then the store/
    /// derive-spawn step.
    async fn validate_and_store(
        &self,
        manifest: CorpusManifest,
        files: Vec<NewCorpusFile>,
        expected_repo: &str,
        source: CorpusSource,
        is_tagged: bool,
    ) -> Result<PublishAccepted, PublishRejection> {
        let core = validate_core(&manifest, &files, expected_repo)?;

        let repo = manifest.repo.clone();
        let version = manifest.version.clone();
        let sha = manifest.sha.clone();

        let new_version = NewCorpusVersion {
            manifest,
            nav: core.nav,
            source,
            is_tagged,
            files,
        };

        match self.store.insert_version(new_version).await {
            Ok(InsertOutcome::Inserted(meta)) => {
                let derive_job_id = self.derive_spawner.spawn(meta.id).await;
                Ok(PublishAccepted {
                    repo,
                    version,
                    sha,
                    pages: core.pages,
                    assets: core.assets,
                    warnings: core.warnings,
                    derive_job_id,
                    replayed: false,
                })
            }
            Ok(InsertOutcome::Existing(_meta)) => Ok(PublishAccepted {
                repo,
                version,
                sha,
                pages: core.pages,
                assets: core.assets,
                warnings: core.warnings,
                derive_job_id: None,
                replayed: true,
            }),
            Err(e) => Err(PublishRejection::plain(
                RejectCode::Internal,
                format!("failed to persist corpus version: {e:#}"),
            )),
        }
    }
}

/// Non-`pub` result of [`validate_core`]: everything [`CorpusIngestService::
/// validate_and_store`] needs to persist a version (`nav`) plus everything
/// [`CorpusIngestService::validate_extracted`]'s public [`ValidationReport`]
/// surfaces (`pages`/`assets`/`warnings`). `repo`/`version`/`sha` aren't
/// included — both callers already own the [`CorpusManifest`] they passed in
/// and read those straight off it.
struct ValidationCore {
    nav: NavTree,
    pages: usize,
    assets: usize,
    warnings: Vec<ContractFinding>,
}

/// The single validation seam shared by [`CorpusIngestService::
/// validate_and_store`] (the persisting path) and [`CorpusIngestService::
/// validate_extracted`] (Task 1's dry-run path): manifest/token repo
/// cross-check, markdown UTF-8 validation, nav parsing, and link/anchor
/// contract checking. A free function (no store/derive-spawner dependency)
/// so both callers reach the exact same reject-or-accept logic — publish's
/// reject behavior stays byte-identical whether reached via the persisting
/// path or the dry-run path.
fn validate_core(
    manifest: &CorpusManifest,
    files: &[NewCorpusFile],
    expected_repo: &str,
) -> Result<ValidationCore, PublishRejection> {
    if manifest.repo != expected_repo {
        return Err(PublishRejection::plain(
            RejectCode::RepoMismatch,
            format!(
                "manifest.repo \"{}\" does not match expected repo \"{expected_repo}\"",
                manifest.repo
            ),
        ));
    }

    let markdown = build_markdown_map(files)?;

    let index_md = markdown.get(&manifest.index).ok_or_else(|| {
        PublishRejection::one_finding(
            RejectCode::Contract,
            format!(
                "manifest.index \"{}\" does not resolve to a markdown file in the corpus",
                manifest.index
            ),
            ContractFinding {
                rule: "missing_index".to_string(),
                file: manifest.index.clone(),
                line: None,
                detail: "index file listed in manifest.json does not exist in the corpus"
                    .to_string(),
                severity: Severity::Error,
            },
        )
    })?;

    let nav = parse_nav(index_md).map_err(|mut finding| {
        // parse_nav is a pure akashic-domain algorithm that hardcodes
        // "index.md" as the finding's file — patch in the REAL index
        // path from the manifest so the finding points at the actual
        // offending file.
        finding.file = manifest.index.clone();
        PublishRejection::one_finding(
            RejectCode::Contract,
            format!("nav parse failed: {}", finding.detail),
            finding,
        )
    })?;

    // Task 11 E2E finding: `check_links` only ever sees markdown content
    // (`markdown`, built above) — a link to a real, existing non-markdown
    // corpus asset (an image, the common case) was therefore
    // unconditionally "unresolved" from `check_links`'s point of view and
    // got flagged `broken_link`. Assets don't need their content for
    // link-checking (no anchor/heading scanning applies to them, and their
    // bytes are frequently not even valid UTF-8) — an empty-string
    // placeholder is enough to make `check_links` treat the path as a known
    // file. `markdown` itself is untouched (still exactly what
    // `index_md`/`parse_nav` above need).
    let mut link_check_files = markdown.clone();
    for f in files {
        if !f.is_markdown {
            link_check_files.entry(f.path.clone()).or_default();
        }
    }
    let findings = check_links(&link_check_files, &nav, &manifest.index);
    let (errors, warnings): (Vec<_>, Vec<_>) = findings
        .into_iter()
        .partition(|f| f.severity == Severity::Error);
    if !errors.is_empty() {
        return Err(PublishRejection::many_findings(
            RejectCode::Contract,
            format!("{} link/anchor contract error(s) found", errors.len()),
            errors,
        ));
    }

    let pages = files.iter().filter(|f| f.is_markdown).count();
    let assets = files.iter().filter(|f| !f.is_markdown).count();

    Ok(ValidationCore {
        nav,
        pages,
        assets,
        warnings,
    })
}

/// Build the `path -> content` map of markdown files from an already-typed
/// file list, validating every markdown file's content is UTF-8 along the
/// way. Shared by both entry points (a pull-path tree can carry non-UTF-8
/// content just as easily as an untarred one).
fn build_markdown_map(
    files: &[NewCorpusFile],
) -> Result<BTreeMap<String, String>, PublishRejection> {
    let mut map = BTreeMap::new();
    for f in files {
        if !f.is_markdown {
            continue;
        }
        match std::str::from_utf8(&f.content) {
            Ok(s) => {
                map.insert(f.path.clone(), s.to_string());
            }
            Err(_) => {
                return Err(PublishRejection::one_finding(
                    RejectCode::Contract,
                    format!("\"{}\" is not valid UTF-8", f.path),
                    ContractFinding {
                        rule: "invalid_utf8".to_string(),
                        file: f.path.clone(),
                        line: None,
                        detail: "markdown file content is not valid UTF-8".to_string(),
                        severity: Severity::Error,
                    },
                ));
            }
        }
    }
    Ok(map)
}

// ── Tar extraction (security-critical) ───────────────────────────────────

/// An already-untarred, already-parsed corpus artifact: [`extract_tar_gz`]'s
/// output. `pub` (not `pub(crate)`) so Task 7's HTTP handler — in a
/// different crate — can call `extract_tar_gz` itself inside
/// `spawn_blocking`, peek `manifest.repo`/`sha` for its idempotency key, and
/// then hand the result to [`CorpusIngestService::ingest_extracted`] without
/// a second extraction pass.
pub struct ExtractedArtifact {
    pub manifest: CorpusManifest,
    pub files: Vec<NewCorpusFile>,
}

/// Untar + gunzip `gz` in one streaming pass, validating every entry as it
/// arrives:
///   1. entry type: only `Regular` and `Directory` are allowed (rejects
///      symlinks, hardlinks, devices, fifos, ...);
///   2. path safety: no `..` component anywhere, no absolute path, no
///      non-UTF-8 path;
///   3. size caps: enforced INCREMENTALLY as bytes are read out of the
///      gzip/tar decoders (never by trusting a header's declared size or by
///      fully buffering an entry/archive first — see this module's doc
///      comment).
///
/// Never touches a real filesystem (no `unpack`/`unpack_in`): every entry's
/// bytes are read straight into memory, which also means there is no
/// symlink-following or TOCTOU surface to defend against in the first
/// place, not just a path check bolted onto a disk-unpack call.
pub fn extract_tar_gz(gz: &[u8]) -> Result<ExtractedArtifact, PublishRejection> {
    let decoder = flate2::read::GzDecoder::new(gz);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive.entries().map_err(|e| {
        PublishRejection::plain(
            RejectCode::Contract,
            format!("not a valid gzip/tar stream: {e}"),
        )
    })?;

    let mut files: Vec<NewCorpusFile> = Vec::new();
    let mut manifest_bytes: Option<Vec<u8>> = None;
    let mut entry_count: usize = 0;
    let mut total_bytes: u64 = 0;

    for entry in entries {
        let mut entry = entry.map_err(|e| {
            PublishRejection::plain(RejectCode::Contract, format!("corrupt tar entry: {e}"))
        })?;

        entry_count += 1;
        if entry_count > MAX_ENTRY_COUNT {
            return Err(too_large_rejection(format!(
                "archive has more than {MAX_ENTRY_COUNT} entries"
            )));
        }

        let raw_path = entry
            .path()
            .map_err(|e| {
                unsafe_entry_rejection(String::new(), format!("unreadable entry path: {e}"))
            })?
            .into_owned();
        let path = validate_and_normalize_path(&raw_path)
            .map_err(|msg| unsafe_entry_rejection(raw_path.to_string_lossy().into_owned(), msg))?;

        let entry_type = entry.header().entry_type();
        if !matches!(
            entry_type,
            tar::EntryType::Regular | tar::EntryType::Directory
        ) {
            return Err(unsafe_entry_rejection(
                path,
                format!(
                    "entry type {entry_type:?} is not allowed (only regular files and directories)"
                ),
            ));
        }

        if entry_type == tar::EntryType::Directory {
            continue; // directories carry no content and aren't stored.
        }

        let mut content = Vec::new();
        let mut buf = vec![0u8; READ_CHUNK_BYTES];
        loop {
            let n = entry.read(&mut buf).map_err(|e| {
                PublishRejection::plain(
                    RejectCode::Contract,
                    format!("failed reading entry \"{path}\": {e}"),
                )
            })?;
            if n == 0 {
                break;
            }
            content.extend_from_slice(&buf[..n]);
            if content.len() as u64 > MAX_FILE_BYTES {
                return Err(too_large_rejection(format!(
                    "\"{path}\" exceeds the {MAX_FILE_BYTES}-byte single-file cap"
                )));
            }
            total_bytes += n as u64;
            if total_bytes > MAX_TOTAL_BYTES {
                return Err(too_large_rejection(format!(
                    "archive exceeds the {MAX_TOTAL_BYTES}-byte total uncompressed-size cap"
                )));
            }
        }

        if path == MANIFEST_ENTRY {
            manifest_bytes = Some(content);
            continue;
        }

        let is_markdown = MARKDOWN_GLOB.is_match(&path);
        files.push(NewCorpusFile {
            path,
            content,
            is_markdown,
        });
    }

    let manifest_bytes = manifest_bytes.ok_or_else(|| {
        PublishRejection::one_finding(
            RejectCode::Schema,
            format!("archive does not contain {MANIFEST_ENTRY} at its root"),
            ContractFinding {
                rule: "schema_invalid".to_string(),
                file: MANIFEST_ENTRY.to_string(),
                line: None,
                detail: format!("{MANIFEST_ENTRY} entry missing"),
                severity: Severity::Error,
            },
        )
    })?;

    // The generated JSON Schema (`manifest_json_schema()`, Task 2) encodes
    // exactly `additionalProperties: false` + the six fields as `required`
    // — nothing beyond type/required/deny-unknown. `CorpusManifest`'s own
    // `#[serde(deny_unknown_fields)]` derive enforces precisely that same
    // contract byte-for-byte (the schema IS generated FROM this struct), so
    // deserializing into it here already IS validating against the
    // canonical schema — a separate `jsonschema`-crate validation pass
    // would just re-check the same six required-string-field constraints a
    // second time.
    let manifest: CorpusManifest = serde_json::from_slice(&manifest_bytes).map_err(|e| {
        PublishRejection::one_finding(
            RejectCode::Schema,
            format!("{MANIFEST_ENTRY} failed schema validation: {e}"),
            ContractFinding {
                rule: "schema_invalid".to_string(),
                file: MANIFEST_ENTRY.to_string(),
                line: None,
                detail: e.to_string(),
                severity: Severity::Error,
            },
        )
    })?;

    Ok(ExtractedArtifact { manifest, files })
}

/// Reject any path that is absolute or contains a `..` component ANYWHERE
/// (not just a leading one — `assets/../../secret` must be caught just as
/// much as `../../secret`), or that isn't valid UTF-8. Returns the
/// normalized path string on success.
fn validate_and_normalize_path(path: &std::path::Path) -> Result<String, String> {
    let Some(path_str) = path.to_str() else {
        return Err("entry path is not valid UTF-8".to_string());
    };
    if path.is_absolute() {
        return Err(format!("entry path \"{path_str}\" is absolute"));
    }
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                return Err(format!(
                    "entry path \"{path_str}\" contains a \"..\" component"
                ));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(format!("entry path \"{path_str}\" is absolute"));
            }
            std::path::Component::CurDir | std::path::Component::Normal(_) => {}
        }
    }
    Ok(path_str.to_string())
}
