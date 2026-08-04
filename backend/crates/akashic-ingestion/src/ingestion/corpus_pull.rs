//! Pull bootstrap (Task 9, B4): lets a repo WITHOUT a CI docs-packer get
//! its docs onto the docs corpus store via akashic's
//! existing git-clone ingestion, by detecting a `.akashic/docs.toml`
//! contract in the checkout and feeding its `docs/` tree through the SAME
//! [`CorpusIngestService::ingest_tree`] core the push path (Task 7's HTTP
//! handler) uses — [`CorpusSource::Pull`] is the only thing that
//! distinguishes the two at the store layer.
//!
//! This step is deliberately INDEPENDENT of code analysis: it runs once,
//! right after the checkout is available, before Stage 2 (code analyze)
//! ever starts, and it never propagates an error up into `run_inner` — a
//! missing contract, a malformed one, or an outright `ingest_tree`
//! rejection are all just logged and treated as "nothing to pull-bootstrap
//! this run", never a code-ingestion failure.

use std::path::Path;

use tracing::{info, warn};

use akashic_domain::algos::corpus_contract::parse_version_header;
use akashic_domain::ports::corpus::NewCorpusFile;
use akashic_domain::types::corpus::{CorpusManifest, CorpusSource, DocsToml};

use super::corpus::{CorpusIngestService, PublishAccepted};

/// The contract file's fixed location within a checkout.
const CONTRACT_PATH: &str = ".akashic/docs.toml";

/// Detect and, if present, ingest a repo checkout's `.akashic/docs.toml`
/// docs contract into the docs-kit corpus store.
///
/// Returns `None` — logging a warning for anything past "no contract file"
/// (a bare absence is silent; that is the overwhelming common case for
/// every repo that hasn't opted in yet) — for:
///   - no `.akashic/docs.toml` in `checkout` at all;
///   - a contract that fails to parse (missing required key, wrong type,
///     invalid TOML);
///   - a `version_header` that doesn't exist, or whose content doesn't
///     contain all three `#define ..._VERSION_{MAJOR,MINOR,PATCH}` lines
///     ([`parse_version_header`] returning `None`) — treated as a contract
///     defect, not something `ingest_tree` should ever see;
///   - a `docs_root` that doesn't exist or can't be walked;
///   - `ingest_tree` itself rejecting the collected tree (schema/contract
///     validation failure at the shared core).
///
/// Never returns `Err` and never panics on a malformed/missing contract —
/// this is a best-effort step the caller (`IngestionPipeline::run_inner`)
/// runs independently of, and without blocking, code analysis.
pub async fn maybe_ingest_repo_docs(
    service: &CorpusIngestService,
    checkout: &Path,
    repo: &str,
    head_sha: &str,
) -> Option<PublishAccepted> {
    let contract_path = checkout.join(CONTRACT_PATH);
    if !contract_path.exists() {
        return None;
    }

    let contract = match load_contract(&contract_path) {
        Ok(c) => c,
        Err(e) => {
            warn!(repo, err = %e, "pull-bootstrap: failed to parse .akashic/docs.toml; skipping");
            return None;
        }
    };

    let version_header_path = checkout.join(&contract.version_header);
    let version_text = match std::fs::read_to_string(&version_header_path) {
        Ok(t) => t,
        Err(e) => {
            warn!(
                repo,
                path = %version_header_path.display(),
                err = %e,
                "pull-bootstrap: failed to read version_header; skipping"
            );
            return None;
        }
    };
    let Some((major, minor, patch)) = parse_version_header(&version_text) else {
        warn!(
            repo,
            path = %version_header_path.display(),
            "pull-bootstrap: version_header did not contain all three VERSION_{{MAJOR,MINOR,PATCH}} defines; skipping"
        );
        return None;
    };
    let version = format!("{major}.{minor}.{patch}");

    let files = match collect_docs_files(checkout, &contract.docs_root, &contract.corpus.exclude) {
        Ok(f) => f,
        Err(e) => {
            warn!(
                repo,
                docs_root = %contract.docs_root,
                err = %e,
                "pull-bootstrap: failed to walk docs_root; skipping"
            );
            return None;
        }
    };

    let manifest = CorpusManifest {
        repo: contract.name.clone().unwrap_or_else(|| repo.to_string()),
        version,
        sha: head_sha.to_string(),
        index: contract.index.clone(),
        languages: contract.languages.clone(),
        tool_version: "pull-bootstrap".to_string(),
    };

    match service
        .ingest_tree(manifest, files, CorpusSource::Pull)
        .await
    {
        Ok(accepted) => {
            info!(
                repo,
                version = %accepted.version,
                sha = %accepted.sha,
                pages = accepted.pages,
                assets = accepted.assets,
                "pull-bootstrap: corpus docs ingested"
            );
            Some(accepted)
        }
        Err(rejection) => {
            warn!(
                repo,
                code = ?rejection.code,
                message = %rejection.message,
                findings = ?rejection.findings,
                "pull-bootstrap: ingest_tree rejected the collected docs tree; skipping"
            );
            None
        }
    }
}

fn load_contract(path: &Path) -> Result<DocsToml, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

/// Walk `checkout/<docs_root>`, dropping any file whose path (relative to
/// `docs_root`) matches one of `exclude`'s globs, and return every
/// remaining regular file as a [`NewCorpusFile`] keyed by its path relative
/// to the ARTIFACT ROOT — the repo checkout root, matching the push
/// convention: `manifest.index` (`docs/README.md`) and every collected
/// file's path both carry the `docs_root/` prefix, so they resolve against
/// each other exactly like an untarred push artifact's `index.md` resolves
/// against its own root-relative file paths.
fn collect_docs_files(
    checkout: &Path,
    docs_root: &str,
    exclude_globs: &[String],
) -> std::io::Result<Vec<NewCorpusFile>> {
    let exclude = build_exclude_globset(exclude_globs);
    let docs_root_abs = checkout.join(docs_root);
    let mut files = Vec::new();
    walk_dir(
        &docs_root_abs,
        &docs_root_abs,
        docs_root,
        &exclude,
        &mut files,
    )?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn build_exclude_globset(patterns: &[String]) -> globset::GlobSet {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        match globset::Glob::new(pattern) {
            Ok(glob) => {
                builder.add(glob);
            }
            Err(e) => {
                warn!(pattern, err = %e, "pull-bootstrap: invalid [corpus].exclude glob; ignoring it");
            }
        }
    }
    // An unbuildable (e.g. duplicate-only) builder can't happen from a plain
    // pattern list; `unwrap_or_else` with an empty set is the documented
    // fallback (excludes nothing) rather than panicking mid-ingest.
    builder.build().unwrap_or_else(|_| {
        globset::GlobSetBuilder::new()
            .build()
            .expect("empty builder always builds")
    })
}

fn walk_dir(
    dir: &Path,
    docs_root_abs: &Path,
    docs_root: &str,
    exclude: &globset::GlobSet,
    out: &mut Vec<NewCorpusFile>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            walk_dir(&path, docs_root_abs, docs_root, exclude, out)?;
            continue;
        }
        if !file_type.is_file() {
            // Symlinks and other non-regular entries are skipped, same
            // stance as the push path's tar-safety extraction.
            continue;
        }

        let rel_to_docs_root = path
            .strip_prefix(docs_root_abs)
            .expect("walked path is always under docs_root_abs")
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if exclude.is_match(&rel_to_docs_root) {
            continue;
        }

        let content = std::fs::read(&path)?;
        let artifact_path = format!("{docs_root}/{rel_to_docs_root}");
        let is_markdown = artifact_path.ends_with(".md");
        out.push(NewCorpusFile {
            path: artifact_path,
            content,
            is_markdown,
        });
    }
    Ok(())
}

/// Resolve `checkout`'s current HEAD commit sha via git2, or `None` if it
/// isn't a git repository at all (e.g. a `local_path` ingestion source
/// pointed at a plain directory rather than a real checkout) — the caller
/// treats `None` as "nothing to key a pulled corpus version on" and skips
/// pull-bootstrap for this run rather than failing the whole ingest.
pub(crate) fn head_sha_of(checkout: &Path) -> Option<String> {
    let repo = git2::Repository::open(checkout).ok()?;
    let head = repo.head().ok()?;
    let oid = head.target()?;
    Some(oid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_exclude_globset_matches_nested_paths_under_pattern() {
        let set = build_exclude_globset(&["superpowers/**".to_string()]);
        assert!(set.is_match("superpowers/foo.md"));
        assert!(set.is_match("superpowers/nested/bar.md"));
        assert!(!set.is_match("guide/setup.md"));
    }

    #[test]
    fn build_exclude_globset_empty_patterns_excludes_nothing() {
        let set = build_exclude_globset(&[]);
        assert!(!set.is_match("anything.md"));
    }

    #[test]
    fn docs_contract_tolerates_unconsumed_snippets_and_check_sections() {
        let toml_text = r#"
version_header = "include/foo/version.hpp"
docs_root = "docs"
index = "docs/README.md"
languages = ["en"]

[corpus]
exclude = ["superpowers/**"]

[snippets]
enabled = true

[check]
strict = true
"#;
        let contract: DocsToml = toml::from_str(toml_text).expect("must tolerate extra sections");
        assert_eq!(contract.docs_root, "docs");
        assert_eq!(contract.corpus.exclude, vec!["superpowers/**".to_string()]);
    }

    #[test]
    fn docs_contract_name_defaults_to_none_when_absent() {
        let toml_text = r#"
version_header = "include/foo/version.hpp"
docs_root = "docs"
index = "docs/README.md"
languages = ["en"]
"#;
        let contract: DocsToml = toml::from_str(toml_text).expect("minimal contract must parse");
        assert!(contract.name.is_none());
    }
}
