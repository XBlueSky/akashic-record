use crate::special::sections::{self, RawSection};
use crate::types::*;
use regex::Regex;

pub static MARKDOWN: MarkdownExtractor = MarkdownExtractor;

pub struct MarkdownExtractor;

impl SpecialExtractor for MarkdownExtractor {
    fn name(&self) -> &'static str {
        "markdown"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &[".md", ".markdown"]
    }

    fn parse(&self, source: &str, _rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        let sects = sections::parse_markdown_sections(source);
        let mut out = ExtractionOutput::default();
        let mut byte_cursor = 0usize;
        emit(&sects, &mut out, source, &mut byte_cursor, None);
        Ok(out)
    }
}

/// Recursively emit one `section` RawChunk per heading. fqn is the dotted
/// path of ancestor headings + this heading; parent_fqn is the ancestor path.
fn emit(
    sects: &[RawSection],
    out: &mut ExtractionOutput,
    source: &str,
    byte_cursor: &mut usize,
    parent_fqn: Option<&str>,
) {
    for s in sects {
        // Locate this heading in the source (search forward from the cursor) for
        // stable line/byte positions. We search for the marker-prefixed heading
        // *line* (e.g. "## Foo") rather than the bare title, so start_byte points
        // at the `#` marker and a title that appears verbatim in a preceding
        // section's body cannot mis-locate the heading. depth is 0-based
        // (0=H1, 1=H2, ...), so the marker count is depth + 1.
        //
        // The needle must be tolerant of exactly what the section parser
        // (`parse_markdown_sections`, regex `(?m)^(#{1,6})\s+(.+)$`) accepted:
        // an arbitrary whitespace run after the marker (tabs / multiple spaces)
        // and a title that has been `.trim()`-ed (so the raw line may carry
        // trailing whitespace, an ATX trailing-hash close, or — for CRLF
        // sources — a trailing `\r`). A literal `format!("{marker} {title}")`
        // needle uses a single space and the trimmed title, so headings written
        // with a tab/double-space after the marker fail to match and the code
        // silently falls back to the stale `byte_cursor`, yielding wrong byte
        // offsets. Instead, build a per-heading regex that mirrors the parser:
        // line-anchored, exact marker count, `\s+`, then the escaped trimmed
        // title. The title is escaped so titles containing regex metacharacters
        // (e.g. backticks, `()`, `.`) are matched literally.
        let marker_count = (s.depth as usize) + 1;
        let needle_re = Regex::new(&format!(
            r"(?m)^#{{{marker_count}}}\s+{}",
            regex::escape(&s.heading)
        ))
        .expect("heading needle regex is well-formed");
        let mat = needle_re.find_at(source, *byte_cursor);
        let start_byte = mat.as_ref().map_or(*byte_cursor, regex::Match::start);
        let start_line = source[..start_byte].matches('\n').count() + 1;
        let content = format!("{}\n\n{}", s.heading, s.content);
        let end_line = start_line + content.matches('\n').count();

        // `end_byte` must be the section's *source* span, not the length of the
        // synthetic `content` string above. `content` = trimmed title + "\n\n" +
        // trimmed body, which diverges from the original bytes on every axis:
        // the heading line carries `#` markers (dropped from `s.heading`) plus a
        // possible whitespace run / ATX trailing-hash close / `\r`, and `s.content`
        // is the `.trim()`-ed body (so leading/trailing blank lines between the
        // heading and the next heading are gone). Adding `content.len()` to
        // `start_byte` therefore points into the wrong place (short or past the
        // real section). Instead, derive the end from where the trimmed body
        // actually sits in the source.
        //
        // The heading line ends at the regex match end (covers "##\tFoo" incl.
        // the whitespace run); fall back to `start_byte` when the heading was not
        // located. `s.content` is `source[..].trim()`, i.e. a verbatim contiguous
        // slice of the source, so searching forward from the heading line locates
        // it exactly; `end_byte` is then the byte just past that body slice. An
        // empty body (heading immediately followed by a sub/sibling heading or
        // EOF) spans only the heading line, so `end_byte` is the heading-line end.
        let heading_line_end = mat.as_ref().map_or(start_byte, regex::Match::end);
        let end_byte = if s.content.is_empty() {
            heading_line_end
        } else {
            source[heading_line_end..]
                .find(s.content.as_str())
                .map_or(heading_line_end, |off| {
                    heading_line_end + off + s.content.len()
                })
        };

        let fqn = match parent_fqn {
            Some(p) => format!("{p}.{}", s.heading),
            None => s.heading.clone(),
        };

        out.chunks.push(RawChunk {
            chunk_type: "section".into(),
            name: s.heading.clone(),
            fqn: Some(fqn.clone()),
            parent_fqn: parent_fqn.map(String::from),
            start_line,
            end_line,
            start_byte,
            end_byte,
            signature: Some(s.heading.clone()),
            content,
            doc: None,
            receiver: None,
            is_async: false,
            is_static: false,
            is_const: false,
            is_exported: false,
            visibility: Visibility::Unknown,
            metadata: ChunkMetadata::default(),
        });

        // Advance the cursor past the matched heading line so the next sibling/
        // child search starts after it. Use the regex match end when we found
        // the line (covers the full "##\tFoo" prefix incl. any whitespace run);
        // otherwise nudge past start_byte to guarantee forward progress and
        // avoid re-matching the same offset. Clamp to `source.len()`:
        // `Regex::find_at` panics when `start > haystack.len()`, so the
        // `start_byte + 1` fallback at end-of-source must not overshoot.
        *byte_cursor = mat
            .as_ref()
            .map_or(start_byte + 1, regex::Match::end)
            .min(source.len());
        emit(&s.children, out, source, byte_cursor, Some(&fqn));
    }
}
