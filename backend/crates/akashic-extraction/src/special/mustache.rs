//! Hand-parsed Mustache / Handlebars template [`SpecialExtractor`] (DSL/infra
//! track).
//!
//! There is no mustache/handlebars tree-sitter grammar crate compatible with
//! this crate's tree-sitter 0.25 runtime, and mustache's `{{ ... }}` tag syntax
//! is small and cleanly hand-parsable, so this extractor scans the source for
//! tags directly rather than depending on a grammar. No new Cargo dependency.
//!
//! ## Tag scanner — sigil classification
//! [`scan_tags`] walks the source looking for `{{`. It distinguishes the triple
//! `{{{ ... }}}` (unescaped interpolation) from the double `{{ ... }}` by peeking
//! at the third byte, and reads forward to the matching `}}}` / `}}` so that a
//! `}` *inside* a tag body (e.g. a comment `{{! has a } brace}}`) does NOT close
//! the tag early. Each tag's inner text is classified by its first non-space
//! sigil:
//! - `#` / `^` → SECTION open (`^` = inverted); push onto the section stack;
//! - `/` → SECTION close; pop the stack (lenient: pop even on a name mismatch).
//!   Handlebars closes a block helper by its helper key only (`{{/if}}` for an
//!   `{{#if user.active}}` open), so the close is compared against the open's
//!   FIRST token as well as its full name; a genuine mismatch (`{{#a}}{{/b}}`)
//!   is recorded in `metadata.fields["close_mismatch"]`;
//! - `>` → PARTIAL include → an [`EdgeKind::Import`] edge;
//! - `!` → comment (ignored, but still consumed so its braces don't break us);
//! - `{` / `&` → unescaped interpolation (NOT a chunk, NOT an edge);
//! - otherwise → plain interpolation (NOT a chunk, NOT an edge).
//!
//! The scanner is robust to: tags spanning a line, multiple tags per physical
//! line, interior whitespace (`{{# name }}`), comments containing braces, and
//! arbitrary text between tags.
//!
//! ## Chunk model — one chunk per SECTION
//! Only `{{#name}}...{{/name}}` and inverted `{{^name}}...{{/name}}` sections are
//! chunks (interpolations are too granular to chunk). Each becomes one
//! `chunk_type = "section"` [`RawChunk`]:
//! - `name` = the section name = the trimmed inner text after the `#`/`^` sigil.
//!   Handlebars-ish block helpers (`{{#if user.active}}`, `{{#each items}}`)
//!   therefore keep their FULL inner text as the name (`if user.active`,
//!   `each items`) rather than just the leading helper token — this preserves
//!   the discriminating argument and keeps sibling helpers distinguishable.
//! - `fqn` = `.`-joined ancestor section names + this one (mirroring the
//!   markdown heading-section model), e.g. `users.user.address`. `parent_fqn`
//!   is the ancestor path (`None` at top level).
//! - `start_line`/`start_byte` at the opening `{{#name}}` tag; `end_line`/
//!   `end_byte` at the END of the closing `{{/name}}` tag. `content` = the full
//!   section text (open tag through close tag); `signature` = the opening tag;
//!   `visibility = Public`. Inverted sections additionally carry
//!   `metadata.fields["inverted"] = "true"`.
//!
//! ## Edge model (all [`Provenance::Static`])
//! - `{{>partial}}` → [`EdgeKind::Import`] with target `Name { name: <partial>,
//!   module_specifier: Some(<partial>) }` — a partial template reference (the
//!   template-composition graph; this is the key edge). The edge `source` is the
//!   enclosing section's `Name`, or the file stem's `Name` at top level
//!   (matching the dockerfile/nginx convention). `metadata.fields["tag"]` =
//!   `"partial"`. Interpolations are intentionally NOT edges.
//!
//! ## Delimiter-change tags (`{{=<% %>=}}`)
//! Treated as a no-op (the inner text starts with `=`, so it falls through to the
//! "plain interpolation" branch and is ignored). Custom delimiters introduced by
//! such a tag are NOT honoured — an accepted limitation for this track.
//!
//! ## Matching / scope
//! `extensions` claims `.mustache`, `.hbs`, and `.handlebars`;
//! `filename_matches` is empty.

use crate::types::*;

pub static MUSTACHE: MustacheExtractor = MustacheExtractor;

pub struct MustacheExtractor;

impl SpecialExtractor for MustacheExtractor {
    fn name(&self) -> &'static str {
        "mustache"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &[".mustache", ".hbs", ".handlebars"]
    }

    fn parse(&self, source: &str, rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        Ok(parse_mustache(source, rel_path))
    }
}

/// What a scanned tag is, after sigil classification.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TagKind {
    /// `{{#name}}` — section open.
    SectionOpen,
    /// `{{^name}}` — inverted section open.
    InvertedOpen,
    /// `{{/name}}` — section close.
    SectionClose,
    /// `{{>name}}` — partial include.
    Partial,
    /// `{{!...}}` — comment.
    Comment,
    /// `{{var}}`, `{{{var}}}`, `{{&var}}` — interpolation (also delimiter-change).
    Interpolation,
}

/// A scanned tag with its classified kind, its inner name (sigil stripped and
/// trimmed), and its source span.
struct Tag {
    kind: TagKind,
    /// The tag's identifier: inner text after the sigil, trimmed. For a section
    /// this is the full name (block helpers keep their arguments).
    name: String,
    /// Byte offset of the leading `{` of `{{`.
    start_byte: usize,
    /// Byte offset just AFTER the trailing `}` of `}}` (one past the tag).
    end_byte: usize,
    /// 1-based line of the tag's start.
    line: usize,
}

/// Scan the source into a flat list of classified tags. Plain text between tags
/// is skipped. Robust to triples, interior whitespace, and braces inside tags.
fn scan_tags(source: &str) -> Vec<Tag> {
    let bytes = source.as_bytes();
    let n = bytes.len();
    let mut tags = Vec::new();
    let mut i = 0usize;
    // 1-based line of byte offset `i`, maintained incrementally.
    let mut line = 1usize;

    while i < n {
        if bytes[i] == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        // Look for the start of a tag `{{`.
        if bytes[i] == b'{' && i + 1 < n && bytes[i + 1] == b'{' {
            let start_byte = i;
            let start_line = line;
            // Triple `{{{ ... }}}` (unescaped interpolation)?
            let triple = i + 2 < n && bytes[i + 2] == b'{';
            let (open_len, close_seq): (usize, &[u8]) =
                if triple { (3, b"}}}") } else { (2, b"}}") };
            let body_start = i + open_len;
            // Find the matching close sequence, scanning past interior braces.
            match find_subsequence(bytes, body_start, close_seq) {
                Some(close_at) => {
                    let inner = &source[body_start..close_at];
                    let end_byte = close_at + close_seq.len();
                    // Count newlines inside the tag so `line` stays accurate.
                    let interior_newlines = bytes[start_byte..end_byte]
                        .iter()
                        .filter(|&&b| b == b'\n')
                        .count();
                    let (kind, name) = classify(inner, triple);
                    tags.push(Tag {
                        kind,
                        name,
                        start_byte,
                        end_byte,
                        line: start_line,
                    });
                    line += interior_newlines;
                    i = end_byte;
                    continue;
                }
                None => {
                    // Unterminated tag — treat the rest as plain text and stop
                    // looking for tags (be lenient, don't panic).
                    break;
                }
            }
        }
        i += 1;
    }

    tags
}

/// Find the first occurrence of `needle` in `hay[from..]`, returning its
/// absolute byte offset.
fn find_subsequence(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    let last = hay.len().checked_sub(needle.len())?;
    (from..=last).find(|&idx| &hay[idx..idx + needle.len()] == needle)
}

/// Classify a tag's inner text (between the delimiters) by its first non-space
/// sigil, returning the kind and the cleaned name (sigil stripped, trimmed).
fn classify(inner: &str, triple: bool) -> (TagKind, String) {
    if triple {
        // `{{{ ... }}}` is always unescaped interpolation.
        return (TagKind::Interpolation, inner.trim().to_string());
    }
    let trimmed = inner.trim_start();
    let mut chars = trimmed.chars();
    match chars.next() {
        Some('#') => (TagKind::SectionOpen, chars.as_str().trim().to_string()),
        Some('^') => (TagKind::InvertedOpen, chars.as_str().trim().to_string()),
        Some('/') => (TagKind::SectionClose, chars.as_str().trim().to_string()),
        Some('>') => (TagKind::Partial, chars.as_str().trim().to_string()),
        Some('!') => (TagKind::Comment, chars.as_str().trim().to_string()),
        // `{{&var}}` unescaped, and `{{=<% %>=}}` delimiter-change (starts `=`):
        // both are non-chunking, non-edge interpolation-class tags.
        Some('&') => (TagKind::Interpolation, chars.as_str().trim().to_string()),
        _ => (TagKind::Interpolation, trimmed.trim().to_string()),
    }
}

/// An open section awaiting its close, tracked on the stack while emitting.
struct OpenSection {
    name: String,
    fqn: String,
    inverted: bool,
    start_byte: usize,
    start_line: usize,
    /// The opening tag text (the signature).
    signature: String,
}

fn parse_mustache(source: &str, rel_path: &str) -> ExtractionOutput {
    let mut out = ExtractionOutput::default();
    let tags = scan_tags(source);

    let mut stack: Vec<OpenSection> = Vec::new();

    let file_source = || EdgeEndpoint::Name {
        name: file_stem(rel_path),
        module_specifier: None,
    };

    for tag in &tags {
        // The current enclosing section's fqn (for nesting) and edge-source name.
        match tag.kind {
            TagKind::SectionOpen | TagKind::InvertedOpen => {
                let parent_fqn = stack.last().map(|s| s.fqn.clone());
                let fqn = match &parent_fqn {
                    Some(p) => format!("{p}.{}", tag.name),
                    None => tag.name.clone(),
                };
                let signature = source[tag.start_byte..tag.end_byte].to_string();
                stack.push(OpenSection {
                    name: tag.name.clone(),
                    fqn,
                    inverted: tag.kind == TagKind::InvertedOpen,
                    start_byte: tag.start_byte,
                    start_line: tag.line,
                    signature,
                });
            }
            TagKind::SectionClose => {
                // Lenient: pop the top of the stack regardless of whether the
                // close name matches the open name (record the mismatch).
                if let Some(open) = stack.pop() {
                    let parent_fqn = stack.last().map(|s| s.fqn.clone());
                    let end_byte = tag.end_byte;
                    let content = source[open.start_byte..end_byte].to_string();
                    let end_line = open.start_line + content.matches('\n').count();

                    let mut metadata = ChunkMetadata::default();
                    if open.inverted {
                        metadata.fields.insert(
                            "inverted".to_string(),
                            MetadataValue::String("true".to_string()),
                        );
                    }
                    // A close name legitimately differs from the open name for
                    // Handlebars block helpers, whose open keeps the full inner
                    // text (`if user.active`) but whose close names only the
                    // helper (`{{/if}}`). So compare the close against the open's
                    // FIRST token (the helper / section key); only flag a real
                    // mismatch (e.g. `{{#a}}...{{/b}}`).
                    let open_key = open.name.split_whitespace().next().unwrap_or("");
                    if !tag.name.is_empty() && tag.name != open_key && tag.name != open.name {
                        metadata.fields.insert(
                            "close_mismatch".to_string(),
                            MetadataValue::String(tag.name.clone()),
                        );
                    }

                    out.chunks.push(RawChunk {
                        chunk_type: "section".into(),
                        name: open.name.clone(),
                        fqn: Some(open.fqn.clone()),
                        parent_fqn,
                        start_line: open.start_line,
                        end_line,
                        start_byte: open.start_byte,
                        end_byte,
                        signature: Some(open.signature.clone()),
                        content,
                        doc: None,
                        receiver: None,
                        is_async: false,
                        is_static: false,
                        is_const: false,
                        is_exported: false,
                        visibility: Visibility::Public,
                        metadata,
                    });
                }
                // Stray close with empty stack: ignore (lenient).
            }
            TagKind::Partial => {
                if tag.name.is_empty() {
                    continue;
                }
                let source_endpoint = match stack.last() {
                    Some(open) => EdgeEndpoint::Name {
                        name: open.name.clone(),
                        module_specifier: None,
                    },
                    None => file_source(),
                };
                let mut metadata = EdgeMetadata::default();
                metadata.fields.insert(
                    "tag".to_string(),
                    MetadataValue::String("partial".to_string()),
                );
                out.edges.push(RawEdge {
                    source: source_endpoint,
                    target: EdgeEndpoint::Name {
                        name: tag.name.clone(),
                        module_specifier: Some(tag.name.clone()),
                    },
                    kind: EdgeKind::Import,
                    provenance: Provenance::Static,
                    line: Some(tag.line),
                    metadata,
                });
            }
            // Comments and interpolations are neither chunks nor edges.
            TagKind::Comment | TagKind::Interpolation => {}
        }
    }

    // Any sections left open at EOF (unbalanced) are closed at end-of-file so
    // nothing is silently lost.
    while let Some(open) = stack.pop() {
        let parent_fqn = stack.last().map(|s| s.fqn.clone());
        let end_byte = source.len();
        let content = source[open.start_byte..end_byte].to_string();
        let end_line = open.start_line + content.matches('\n').count();
        let mut metadata = ChunkMetadata::default();
        if open.inverted {
            metadata.fields.insert(
                "inverted".to_string(),
                MetadataValue::String("true".to_string()),
            );
        }
        metadata.fields.insert(
            "unclosed".to_string(),
            MetadataValue::String("true".to_string()),
        );
        out.chunks.push(RawChunk {
            chunk_type: "section".into(),
            name: open.name.clone(),
            fqn: Some(open.fqn.clone()),
            parent_fqn,
            start_line: open.start_line,
            end_line,
            start_byte: open.start_byte,
            end_byte,
            signature: Some(open.signature.clone()),
            content,
            doc: None,
            receiver: None,
            is_async: false,
            is_static: false,
            is_const: false,
            is_exported: false,
            visibility: Visibility::Public,
            metadata,
        });
    }

    out
}

/// The file stem (basename without extension), used as the file-level edge
/// source for top-level partials.
fn file_stem(rel_path: &str) -> String {
    let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
    match base.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
        _ => base.to_string(),
    }
}
