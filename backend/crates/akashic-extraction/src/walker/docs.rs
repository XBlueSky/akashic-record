//! walker::docs — carved from walker.rs (EXT god-file split). See `super` for the
//! module overview; cross-cluster fns are re-exported there for `use super::*`.
use super::*;

// ─────────────────────────────────────────────────────────────
// Doc-comment extraction
// ─────────────────────────────────────────────────────────────

/// Capture the leading doc-comment block of `node`, if any.
///
/// HISTORY (altitude finding): this path was formerly dead — every shipped
/// [`LanguageConfig`] set `doc_comment_kinds: &[]`, so it returned `None` for
/// all languages and `RawChunk.doc` was universally `None`. It was deliberately
/// left dead rather than "fixed" by a blanket enablement, because enabling it
/// correctly is NOT a one-line config flip:
///
///  * **Kind alone is insufficient for several grammars.** In tree-sitter-rust a
///    `///` doc comment is NOT a distinct node kind — it is a `line_comment`
///    whose doc-ness lives in a child `outer_doc_comment_marker` /
///    `doc_comment` field. Setting `doc_comment_kinds: &["line_comment"]` would
///    misclassify every ordinary `//` comment as documentation. Distinguishing
///    them requires inspecting the marker child, which this kind-only matcher
///    does not do.
///  * **Single-sibling capture is too narrow.** Idiomatic docs span multiple
///    consecutive `///` (or `*`-prefixed block) lines.
///
/// This now implements the two pieces a correct enablement needs:
///  1. a doc-ness predicate ([`is_doc_comment`]) that distinguishes `///`/`//!`
///     / `/**`/`/*!` documentation from ordinary `//` and `/* */` comments by
///     their marker prefix, so setting `doc_comment_kinds` to a comment kind no
///     longer misclassifies every plain comment as documentation; and
///  2. backward coalescing of the run of consecutive doc-comment siblings, so a
///     multi-line `///` block is captured in full (top-to-bottom), not just the
///     last line.
///
/// Enablement is still **per language, behind its own golden fixture** — only
/// languages that set a non-empty `doc_comment_kinds` extract docs. Languages
/// that keep `doc_comment_kinds: &[]` remain dormant until they opt in with a
/// fixture. Currently enabled: Rust, Java, TypeScript, JavaScript, C#, C, C++.
pub(crate) fn extract_leading_doc(
    node: Node<'_>,
    src: &[u8],
    doc_kinds: &[&str],
) -> Option<String> {
    if doc_kinds.is_empty() {
        return None;
    }
    // Most languages put the doc as a direct preceding sibling of the construct.
    // But a construct can be WRAPPED — e.g. a TS/JS `export function`, where the
    // doc precedes the `export_statement`, while the walker chunks the inner
    // `function_declaration` (whose own preceding sibling is None). When a node
    // has no preceding sibling, ascend through such wrappers (bounded) and look
    // again. Ascent stops the moment a preceding sibling exists, so a doc that
    // belongs to an ENCLOSING construct (e.g. a class's doc) is never stolen by
    // its first child — that child's ascent halts at the class name/body sibling,
    // which is not a doc comment.
    let mut current = node;
    for _ in 0..4 {
        match current.prev_named_sibling() {
            Some(prev) => return collect_doc_run(prev, src, doc_kinds),
            None => match current.parent() {
                Some(parent) => current = parent,
                None => return None,
            },
        }
    }
    None
}

/// Walk backwards from `last` over the run of consecutive doc-comment siblings,
/// newest-first, returning their text joined in source order — or `None` if
/// `last` is not itself a doc comment (an ordinary comment, a statement, or a
/// different node kind ends the run). A multi-line `///` block is several
/// sibling comment nodes; this captures the whole run, not just the last line.
pub(crate) fn collect_doc_run(last: Node<'_>, src: &[u8], doc_kinds: &[&str]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cursor = Some(last);
    while let Some(prev) = cursor {
        if !doc_kinds.contains(&prev.kind()) {
            break;
        }
        let Ok(text) = prev.utf8_text(src) else {
            break;
        };
        if !is_doc_comment(text) {
            break;
        }
        lines.push(text.trim_end().to_string());
        cursor = prev.prev_named_sibling();
    }
    if lines.is_empty() {
        return None;
    }
    // Collected newest-first; restore source order.
    lines.reverse();
    Some(lines.join("\n"))
}

/// A comment is documentation if it opens with an outer/inner doc marker:
/// `///` or `//!` (line) or `/**` / `/*!` (block). Plain `//` and `/* */`
/// comments are excluded. (`////`+ rulers also pass the `///` test, but that is
/// a rare, harmless over-capture.)
pub(crate) fn is_doc_comment(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("///") || t.starts_with("//!") || t.starts_with("/**") || t.starts_with("/*!")
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────
