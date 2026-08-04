use anyhow::{Result, anyhow};

use super::file_discovery::AnalyzedFile;
use akashic_extraction::{ChunkMetadata, ExtractionOutput, ExtractorKind, RawChunk, Visibility};

const MIN_CHUNK_SIZE: usize = 50;

/// Extract chunks **and** edges from a single file via the declarative
/// extraction layer, then enforce size limits on the resulting chunks.
///
/// Dispatch is via the file's pre-resolved `extractor` (set at discovery time
/// by the registry). The returned [`ExtractionOutput`] carries both the chunks
/// (filtered/truncated) and the raw edges (calls + imports) so the pipeline can
/// resolve them without re-parsing.
pub fn extract_file(
    file: &AnalyzedFile,
    source: &str,
    max_chunk_size: usize,
) -> Result<ExtractionOutput> {
    let mut output = match file.extractor {
        ExtractorKind::TreeSitter(cfg) => {
            akashic_extraction::extract(cfg, source.as_bytes(), &file.relative_path)
                .map_err(|e| anyhow!("extraction failed for {}: {e}", file.relative_path))?
        }
        ExtractorKind::Custom(sp) => sp
            .parse(source, &file.relative_path)
            .map_err(|e| anyhow!("extraction failed for {}: {e}", file.relative_path))?,
    };

    // Filter and truncate chunks (preserves the pre-refactor MIN_CHUNK_SIZE
    // filter + max-size char-boundary-safe truncation).
    // BUG 5(a): `route` chunks emitted by the query runner
    // (`run_framework_routes`) have small spans (a single registration call,
    // often < MIN_CHUNK_SIZE) but are semantically load-bearing — dropping them
    // here silently orphans their RoutesTo edge, which then mis-attributes to
    // the enclosing function. Exempt `route` chunks from the size filter,
    // mirroring how EXT-7-4 file-based route chunks are appended AFTER it.
    output.chunks = output
        .chunks
        .into_iter()
        .filter(|c| {
            c.chunk_type == "route"
                // http_call chunks are tiny call-site chunks that would otherwise be
                // dropped by MIN_CHUNK_SIZE; exempt them like route chunks.
                || c.chunk_type == "http_call"
                || c.content.len() >= MIN_CHUNK_SIZE
        })
        .map(|mut c| {
            if c.content.len() > max_chunk_size {
                // Floor to char boundary before truncating (String::truncate panics mid-char)
                let mut cut = max_chunk_size;
                while cut > 0 && !c.content.is_char_boundary(cut) {
                    cut -= 1;
                }
                c.content.truncate(cut);
                c.content.push_str("\n// ... truncated");
            }
            c
        })
        .collect::<Vec<RawChunk>>();

    // EXT-7-4: append a synthetic `route` chunk for SvelteKit/Nuxt route files.
    // Added AFTER the size filter because synthetic route chunks have no real
    // body (content is a short description string) and would be filtered out
    // if inserted before.
    if let Some(fr) = crate::ingestion::file_routes::file_based_route(&file.relative_path) {
        output.chunks.push(RawChunk {
            chunk_type: "route".to_string(),
            name: fr.route.clone(),
            fqn: Some(fr.route.clone()),
            parent_fqn: None,
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 0,
            signature: None,
            content: format!(
                "file-based {} route {} in {}",
                fr.kind, fr.route, file.relative_path
            ),
            doc: None,
            receiver: None,
            is_async: false,
            is_static: false,
            is_const: false,
            is_exported: true,
            visibility: Visibility::Public,
            metadata: ChunkMetadata::default(),
        });
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::super::file_discovery::{AnalyzedFile, Priority};
    use super::extract_file;
    use akashic_extraction::ExtractorKind;
    use akashic_extraction::languages::RUST;

    static RUST_EK: ExtractorKind = ExtractorKind::TreeSitter(&RUST);

    #[test]
    fn http_call_chunk_survives_min_size_filter() {
        let file = AnalyzedFile {
            path: std::path::PathBuf::from("client.rs"),
            relative_path: "client.rs".to_string(),
            extractor: &RUST_EK,
            priority: Priority::P4,
            size_bytes: 0,
        };
        // The reqwest call chunk (~27 bytes) is well under MIN_CHUNK_SIZE (50);
        // the enclosing fn is padded past it so only the exemption keeps the
        // http_call chunk alive.
        let src = "async fn caller() {\n    // padding padding padding padding padding\n    let _ = reqwest::get(\"/api/widget\").await;\n}\n";
        let out = extract_file(&file, src, 5120).unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "http_call" && c.content.len() < super::MIN_CHUNK_SIZE),
            "a sub-MIN_CHUNK_SIZE http_call chunk must survive the size filter"
        );
    }
}
