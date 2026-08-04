use crate::types::*;

pub static FALLBACK: FallbackExtractor = FallbackExtractor;

pub struct FallbackExtractor;

impl SpecialExtractor for FallbackExtractor {
    fn name(&self) -> &'static str {
        "fallback"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &[] // catch-all via registry order, not extension
    }

    fn parse(&self, source: &str, _rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        let total_lines = source.matches('\n').count() + 1;
        let mut out = ExtractionOutput::default();
        out.chunks.push(RawChunk {
            chunk_type: "document".into(),
            name: "fallback".into(),
            fqn: None,
            parent_fqn: None,
            start_line: 1,
            end_line: total_lines,
            start_byte: 0,
            end_byte: source.len(),
            signature: None,
            content: source.to_string(),
            doc: None,
            receiver: None,
            is_async: false,
            is_static: false,
            is_const: false,
            is_exported: false,
            visibility: Visibility::Unknown,
            metadata: ChunkMetadata::default(),
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_emits_single_document_chunk() {
        let src = "line one\nline two\nline three";
        let out = FALLBACK.parse(src, "weird.xyz").unwrap();
        assert_eq!(out.chunks.len(), 1);
        assert_eq!(out.chunks[0].chunk_type, "document");
        assert_eq!(out.chunks[0].start_line, 1);
        assert_eq!(out.chunks[0].end_line, 3);
        assert_eq!(out.chunks[0].content, src);
        assert!(out.edges.is_empty());
    }

    #[test]
    fn fallback_handles_empty_source() {
        let out = FALLBACK.parse("", "empty.xyz").unwrap();
        assert_eq!(out.chunks.len(), 1);
        assert_eq!(out.chunks[0].end_line, 1);
        assert_eq!(out.chunks[0].end_byte, 0);
    }
}
