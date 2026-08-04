use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::{debug, warn};

use akashic_extraction::ExtractorKind;
use akashic_extraction::registry;

/// Priority level for ingestion (P1 = highest priority).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    P1, // README, package.json, Cargo.toml
    P2, // Headers, type defs, .d.ts
    P3, // Module entry points (index.ts, mod.rs, lib.rs)
    P4, // Implementation files
}

/// A file discovered during analysis, with metadata.
///
/// `extractor` is resolved once at discovery time via the extraction registry
/// (replacing the former `Language` enum). The pipeline derives a language
/// *name* from it via [`extractor_name`].
#[derive(Clone)]
pub struct AnalyzedFile {
    pub path: PathBuf,
    pub relative_path: String,
    pub extractor: &'static ExtractorKind,
    pub priority: Priority,
    pub size_bytes: u64,
}

/// The canonical language name for an extractor (e.g. "rust", "cpp", "markdown",
/// "fallback"). Mirrors the old `Language::as_str`.
pub fn extractor_name(extractor: &ExtractorKind) -> &'static str {
    match extractor {
        ExtractorKind::TreeSitter(c) => c.name,
        ExtractorKind::Custom(sp) => sp.name(),
    }
}

/// Walk the repository tree, resolve each file's extractor, assign priorities,
/// and return files sorted by priority (P1 first).
pub fn analyze_repo(
    root: &Path,
    max_file_size: usize,
    skip_patterns: &[String],
) -> Result<Vec<AnalyzedFile>> {
    let mut files = Vec::new();

    let mut builder = ignore::WalkBuilder::new(root);
    builder.hidden(true).git_ignore(true).git_global(false);

    for entry in builder.build().flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        // Skip by size.
        // FINDING #2 FIX: a single failing metadata() must NOT abort the whole
        // repo walk (previously `path.metadata()?` propagated and killed the job).
        // On a per-file error, warn and skip that one file. This also resolves the
        // TOCTOU double-stat: we no longer rely on the earlier `is_file()` stat for
        // correctness — `metadata()` is the single authoritative stat, and if the
        // entry vanished between the walk and now, the error path skips it cleanly.
        let meta = match path.metadata() {
            Ok(m) => m,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Skipping: metadata() failed");
                continue;
            }
        };
        if meta.len() > max_file_size as u64 {
            debug!(path = %path.display(), "Skipping: too large");
            continue;
        }

        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // Skip patterns
        if skip_patterns.iter().any(|pat| rel.contains(pat)) {
            continue;
        }

        let extractor = registry::lookup_for_path(&rel);
        let priority = assign_priority(&rel);

        files.push(AnalyzedFile {
            path: path.to_path_buf(),
            relative_path: rel,
            extractor,
            priority,
            size_bytes: meta.len(),
        });
    }

    files.sort_by_key(|f| f.priority);
    Ok(files)
}

/// Assign an ingestion priority based purely on the file name / extension.
///
/// Rules preserved from the former analyzer:
/// - manifests / readme → P1
/// - markdown → P1
/// - headers / `.d.ts` / `.types.ts` → P2
/// - module entry points (index.*, mod.rs, lib.rs, main.rs, __init__.py) → P3
/// - everything else → P4
fn assign_priority(rel_path: &str) -> Priority {
    let filename = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let lower = filename.to_lowercase();

    // P1: manifest / readme files
    if matches!(
        lower.as_str(),
        "readme.md"
            | "readme"
            | "package.json"
            | "cargo.toml"
            | "go.mod"
            | "pyproject.toml"
            | "setup.py"
            | "makefile"
            | "cmakelists.txt"
    ) {
        return Priority::P1;
    }

    // P1: all markdown is high priority
    if lower.ends_with(".md") || lower.ends_with(".markdown") {
        return Priority::P1;
    }

    // P2: header files, type definitions
    if lower.ends_with(".h")
        || lower.ends_with(".hpp")
        || lower.ends_with(".d.ts")
        || lower.ends_with(".types.ts")
    {
        return Priority::P2;
    }

    // P3: module entry points
    if matches!(
        lower.as_str(),
        "index.ts" | "index.js" | "index.tsx" | "mod.rs" | "lib.rs" | "main.rs" | "__init__.py"
    ) {
        return Priority::P3;
    }

    // P4: everything else
    Priority::P4
}
