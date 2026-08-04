//! Integration tests that cross the extraction + chunker boundary.
//! These live in akashic-server (not akashic-extraction) because they rely on
//! `chunker::extract_file` and `file_discovery` types that are server-only.

use akashic_extraction::{ExtractionOutput, ExtractorKind};
use akashic_ingestion::ingestion::chunker;
use akashic_ingestion::ingestion::file_discovery::{AnalyzedFile, Priority};
use std::path::PathBuf;

fn route_names(out: &ExtractionOutput) -> Vec<String> {
    out.chunks
        .iter()
        .filter(|c| c.chunk_type == "route")
        .map(|c| c.name.clone())
        .collect()
}

// BUG 5(a): query-emitted route chunks with tiny content must survive the
// MIN_CHUNK_SIZE filter in the chunker.
#[test]
fn small_route_chunks_survive_min_size_filter() {
    static RUST_EXTRACTOR: ExtractorKind =
        ExtractorKind::TreeSitter(&akashic_extraction::languages::RUST);
    // Two registrations in a builder chain. Each individual route's
    // own span is tiny (well under MIN_CHUNK_SIZE = 50 bytes).
    let src = "fn a()->R{Router::new().route(\"/x\",get(h1)).route(\"/y\",get(h2))}\n";
    let file = AnalyzedFile {
        path: PathBuf::from("x/y.rs"),
        relative_path: "x/y.rs".to_string(),
        extractor: &RUST_EXTRACTOR,
        priority: Priority::P4,
        size_bytes: src.len() as u64,
    };
    let out = chunker::extract_file(&file, src, 10_000).unwrap();
    let names = route_names(&out);
    assert!(
        names.iter().any(|r| r == "GET /x") && names.iter().any(|r| r == "GET /y"),
        "both small route chunks must survive the size filter, got {names:?}"
    );
}
