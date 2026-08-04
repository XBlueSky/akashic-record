//! Baseline retrieval strategy: deterministic in-process regex-grep + file
//! read over a source tree on disk. Models the text an agent pulls into
//! context when it greps and reads files, WITHOUT the code graph. Never
//! shells out (that would add a runtime dependency + non-determinism) and
//! never errors on a not-found file/pattern (a real agent also just gets
//! nothing back) — a step that finds nothing yields an empty blob + a warn.

use crate::metrics::Blob;
use crate::questions::BaselineStep;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub struct BaselineRunner {
    source_dir: PathBuf,
    context_lines: usize,
}

impl BaselineRunner {
    pub fn new(source_dir: PathBuf, context_lines: usize) -> Self {
        Self {
            source_dir,
            context_lines,
        }
    }

    /// Execute a baseline plan, returning one blob per step (empty blob if the
    /// step found nothing).
    pub fn run(&self, plan: &[BaselineStep]) -> Vec<Blob> {
        plan.iter()
            .map(|step| match step {
                BaselineStep::Grep { pattern, glob } => self.grep(pattern, glob.as_deref()),
                BaselineStep::Read { file, lines } => self.read(file, *lines),
            })
            .collect()
    }

    fn grep(&self, pattern: &str, glob: Option<&str>) -> Blob {
        let re = match regex::Regex::new(pattern) {
            Ok(re) => re,
            Err(e) => {
                tracing::warn!(%pattern, error = %e, "baseline grep: invalid regex");
                return Blob(String::new());
            }
        };
        let suffix = glob.and_then(glob_suffix);
        let mut files = Vec::new();
        collect_files(&self.source_dir, &mut files);
        files.sort();

        let mut out: Vec<String> = Vec::new();
        for file in &files {
            if let Some(sfx) = &suffix {
                let name = file.to_string_lossy();
                if !name.ends_with(sfx.as_str()) {
                    continue;
                }
            }
            let Ok(content) = std::fs::read_to_string(file) else {
                continue; // binary / unreadable — skip like ripgrep would
            };
            let lines: Vec<&str> = content.lines().collect();
            // 0-based indices of every line to emit (match ± context), merged.
            let mut emit: BTreeSet<usize> = BTreeSet::new();
            for (i, line) in lines.iter().enumerate() {
                if re.is_match(line) {
                    let lo = i.saturating_sub(self.context_lines);
                    let hi = (i + self.context_lines).min(lines.len().saturating_sub(1));
                    for j in lo..=hi {
                        emit.insert(j);
                    }
                }
            }
            if emit.is_empty() {
                continue;
            }
            let rel = file.strip_prefix(&self.source_dir).unwrap_or(file);
            let rel = rel.to_string_lossy();
            for idx in emit {
                out.push(format!("{}:{}: {}", rel, idx + 1, lines[idx]));
            }
        }

        if out.is_empty() {
            tracing::warn!(%pattern, "baseline grep: no matches");
        }
        Blob(out.join("\n"))
    }

    fn read(&self, file: &str, lines: Option<[usize; 2]>) -> Blob {
        let path = self.source_dir.join(file);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(file, error = %e, "baseline read: file not found / unreadable");
                return Blob(String::new());
            }
        };
        match lines {
            None => Blob(content),
            Some([a, b]) => {
                // 1-based inclusive range.
                let all: Vec<&str> = content.lines().collect();
                if a == 0 || a > all.len() {
                    return Blob(String::new());
                }
                let start = a - 1;
                let end = b.min(all.len());
                if end <= start {
                    // Inverted / empty range (e.g. [5,2]) — nothing to read.
                    return Blob(String::new());
                }
                Blob(all[start..end].join("\n"))
            }
        }
    }
}

/// Extract an extension suffix from a simple glob. `**/*.rs` -> `Some(".rs")`;
/// a glob with no extension tail (`**/*`, `src/**`) -> `None` (match all
/// files). Only extension-suffix globs are supported.
fn glob_suffix(glob: &str) -> Option<String> {
    let last = glob.rsplit('/').next().unwrap_or(glob); // e.g. "*.rs"
    let idx = last.rfind('.')?;
    let ext = &last[idx..]; // ".rs"
    if ext.len() > 1 {
        Some(ext.to_string())
    } else {
        None
    }
}

/// Recursively collect files under `dir`, skipping `.git`, `target`, and
/// `node_modules` directory segments.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name == ".git" || name == "target" || name == "node_modules" {
                continue;
            }
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("src")).unwrap();
        fs::write(
            d.path().join("src/a.rs"),
            "fn foo() {}\nfn calls_foo() { foo(); }\nlet x = 1;\n",
        )
        .unwrap();
        fs::write(d.path().join("src/b.txt"), "foo appears here too\n").unwrap();
        d
    }

    #[test]
    fn grep_matches_with_context_and_glob() {
        let d = tmp_tree();
        let r = BaselineRunner::new(d.path().to_path_buf(), 0);
        let blobs = r.run(&[BaselineStep::Grep {
            pattern: "foo".into(),
            glob: Some("**/*.rs".into()),
        }]);
        assert_eq!(blobs.len(), 1);
        let text = &blobs[0].0;
        assert!(text.contains("fn foo()"));
        assert!(text.contains("calls_foo"));
        assert!(
            !text.contains("b.txt"),
            "glob *.rs must exclude the .txt file"
        );
    }

    #[test]
    fn read_line_range() {
        let d = tmp_tree();
        let r = BaselineRunner::new(d.path().to_path_buf(), 0);
        let blobs = r.run(&[BaselineStep::Read {
            file: "src/a.rs".into(),
            lines: Some([2, 2]),
        }]);
        assert_eq!(blobs[0].0.trim(), "fn calls_foo() { foo(); }");
    }

    #[test]
    fn missing_file_is_empty_blob_not_error() {
        let d = tmp_tree();
        let r = BaselineRunner::new(d.path().to_path_buf(), 0);
        let blobs = r.run(&[BaselineStep::Read {
            file: "nope.rs".into(),
            lines: None,
        }]);
        assert_eq!(blobs.len(), 1);
        assert!(blobs[0].0.is_empty());
    }

    #[test]
    fn grep_context_lines_widen_window() {
        let d = tmp_tree();
        let r = BaselineRunner::new(d.path().to_path_buf(), 1);
        // matching line 2 (calls_foo) with 1 context line pulls line 1 and 3.
        let blobs = r.run(&[BaselineStep::Grep {
            pattern: "calls_foo".into(),
            glob: Some("**/*.rs".into()),
        }]);
        let text = &blobs[0].0;
        assert!(
            text.contains("fn foo()"),
            "context should include the line above"
        );
        assert!(
            text.contains("let x = 1"),
            "context should include the line below"
        );
    }
}
