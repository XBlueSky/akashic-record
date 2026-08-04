//! PERSISTENT real-corpus snapshot (EXT-1 Task 20).
//!
//! Walks `backend/src/ingestion/**/*.rs` (a stable subtree), runs the NEW
//! declarative walker (`extraction::extract`) on each file, and collects a
//! NORMALIZED aggregate — a sorted list of `(rel_path, chunk_type, name, fqn)`
//! with NO byte ranges or content, so the snapshot stays stable across edits
//! that don't change the symbol set. The aggregate is serialized to JSON and
//! compared against a committed golden file; `UPDATE_GOLDEN=1` regenerates it.
//!
//! This references ONLY the new extraction layer (no old parsers), so it
//! SURVIVES EXT-1 Task 19 as a lasting regression anchor. Gated `#[ignore]`.

use crate::languages;
use std::path::{Path, PathBuf};

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct SnapshotEntry {
    rel_path: String,
    chunk_type: String,
    name: String,
    fqn: Option<String>,
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ingestion_root() -> PathBuf {
    manifest_dir().join("src/ingestion")
}

fn golden_path() -> PathBuf {
    manifest_dir().join("tests/fixtures/real-corpus/ingestion-snapshot.json")
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
#[ignore = "real-corpus snapshot over backend/src/ingestion; run with --ignored on demand"]
fn real_corpus_snapshot() {
    let ing_root = ingestion_root();
    // rel_path is expressed relative to the manifest dir (backend/) so the
    // snapshot reads naturally, e.g. "src/ingestion/extraction/walker.rs".
    let manifest = manifest_dir();

    let mut files = Vec::new();
    collect_rs_files(&ing_root, &mut files);
    // Exclude test-infrastructure files: they are not production extraction
    // targets and self-referencing them makes the snapshot fragile.
    files.retain(|p| {
        let s = p.to_string_lossy();
        !s.ends_with("golden.rs") && !s.ends_with("corpus_snapshot.rs")
    });
    files.sort();

    let mut entries: Vec<SnapshotEntry> = Vec::new();
    for path in &files {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let rel = path
            .strip_prefix(&manifest)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        let out = match crate::extract(&languages::RUST, &bytes, &rel) {
            Ok(o) => o,
            Err(_) => continue,
        };
        for c in &out.chunks {
            entries.push(SnapshotEntry {
                rel_path: rel.clone(),
                chunk_type: c.chunk_type.clone(),
                name: c.name.clone(),
                fqn: c.fqn.clone(),
            });
        }
    }

    entries.sort();

    let actual = serde_json::to_string_pretty(&entries).unwrap();
    let gpath = golden_path();

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = gpath.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&gpath, format!("{actual}\n")).unwrap();
        eprintln!(
            "wrote {} snapshot entries to {}",
            entries.len(),
            gpath.display()
        );
        return;
    }

    let expected = std::fs::read_to_string(&gpath)
        .unwrap_or_else(|e| panic!("read {gpath:?}: {e} — run UPDATE_GOLDEN=1 first"));
    assert_eq!(
        actual.trim(),
        expected.trim(),
        "ingestion-snapshot.json mismatch — re-run with UPDATE_GOLDEN=1 if intended"
    );
}

/// Timing probe: walks ALL `backend/src/**/*.rs` files, runs the declarative
/// walker on each, and reports total wall-clock time + file/chunk counts.
///
/// This is NOT a benchmark — it's a one-shot timing probe. Run with:
///
/// ```
/// cargo test extraction_timing -- --ignored --nocapture 2>&1 | tail -8
/// ```
///
/// No assertion is made; the output is for human inspection.
#[test]
#[ignore = "timing probe over backend/src/**/*.rs — run with --ignored --nocapture"]
fn extraction_timing() {
    let src_root = manifest_dir().join("src");

    let mut files = Vec::new();
    collect_rs_files(&src_root, &mut files);
    files.sort();

    let manifest = manifest_dir();
    let t0 = std::time::Instant::now();
    let mut total_chunks: usize = 0;

    for path in &files {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let rel = path
            .strip_prefix(&manifest)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        if let Ok(out) = crate::extract(&languages::RUST, &bytes, &rel) {
            total_chunks += out.chunks.len();
        }
    }

    let elapsed = t0.elapsed();
    eprintln!(
        "extraction_timing: {} files, {} chunks, wall-clock {:.3}s",
        files.len(),
        total_chunks,
        elapsed.as_secs_f64(),
    );
}
