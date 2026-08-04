use crate::languages::TYPESCRIPT;
use crate::types::*;
use crate::walker;

pub static VUE: VueExtractor = VueExtractor;

pub struct VueExtractor;

impl SpecialExtractor for VueExtractor {
    fn name(&self) -> &'static str {
        "vue"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &[".vue"]
    }

    fn parse(&self, source: &str, rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        let Some((script, byte_off, line_off)) = extract_script_block(source) else {
            // No <script> block → empty output (not an error).
            return Ok(ExtractionOutput::default());
        };
        let mut out = walker::extract(&TYPESCRIPT, script.as_bytes(), rel_path)?;
        for c in &mut out.chunks {
            c.start_byte += byte_off;
            c.end_byte += byte_off;
            c.start_line += line_off;
            c.end_line += line_off;
        }
        for e in &mut out.edges {
            if let Some(l) = e.line.as_mut() {
                *l += line_off;
            }
            if let EdgeEndpoint::ByteRange { start, end } = &mut e.source {
                *start += byte_off;
                *end += byte_off;
            }
            if let EdgeEndpoint::ByteRange { start, end } = &mut e.target {
                *start += byte_off;
                *end += byte_off;
            }
        }
        Ok(out)
    }
}

/// Return (script_contents, byte_offset_of_contents, line_offset).
///
/// `byte_off` is the byte index in the .vue source where the script content
/// starts (i.e. one past the closing `>` of the `<script ...>` tag).
///
/// `line_off` is the number of newlines before that byte position — i.e. the
/// number of lines preceding the script content.  A chunk that tree-sitter
/// reports at row R (0-based, so 1-based line = R + 1) within the extracted
/// script maps to .vue line (R + 1) + line_off.
fn extract_script_block(source: &str) -> Option<(String, usize, usize)> {
    // Search the ORIGINAL bytes with ASCII-case-insensitive matching. `<script`
    // and `</script>` are pure-ASCII tag markers, so this needs no
    // `to_lowercase()` — and crucially keeps every byte index aligned with
    // `source`. Lowercasing can change a character's byte length (e.g. 'İ'
    // U+0130 → "i̇", 2 bytes → 3), and offsets computed on the lowercased copy
    // then drift against `source`, corrupting the slice or panicking on a
    // non-char boundary.
    let bytes = source.as_bytes();
    // Opening `<script` tag.
    let open = find_ascii_ci(bytes, b"<script")?;
    // Closing `>` of the opening tag.
    let gt_rel = bytes[open..].iter().position(|&b| b == b'>')?;
    let gt = open + gt_rel + 1; // byte index just past `>` (an ASCII char boundary)
    // Closing `</script>` after the content start.
    let close_rel = find_ascii_ci(&bytes[gt..], b"</script>")?;
    let end = gt + close_rel; // index of `<` in `</script>` (an ASCII char boundary)
    let script = source[gt..end].to_string();
    // Count newlines before the script content.
    let line_off = source[..gt].matches('\n').count();
    Some((script, gt, line_off))
}

/// ASCII-case-insensitive substring search over bytes. Returns the byte index in
/// `haystack` of the first window equal to `needle` ignoring ASCII case. Keeping
/// the search on the original bytes (rather than a `to_lowercase()` copy) means
/// returned indices are always valid offsets into the source.
fn find_ascii_ci(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_block_offsets_survive_non_ascii_lowercasing() {
        // 'İ' (U+0130) lowercases to "i̇" — 2 source bytes become 3 — so any
        // byte-offset arithmetic done on a `to_lowercase()` copy drifts versus
        // the original source and corrupts (or panics on) the sliced content.
        let source = "<script>\nconst s = \"İ\";\n</script>";
        let (script, byte_off, line_off) =
            extract_script_block(source).expect("script block present");
        assert_eq!(
            script, "\nconst s = \"İ\";\n",
            "sliced script must match the original source exactly (no drift)"
        );
        assert_eq!(&source[byte_off..byte_off + 1], "\n");
        assert_eq!(line_off, 0);
    }
}
