//! Hand-parsed Makefile / GNUmakefile [`SpecialExtractor`] (DSL/infra track).
//!
//! Makefiles are line-oriented (rules, variable assignments, directives), so
//! this extractor hand-parses rather than using tree-sitter: the build DAG of
//! `targets : prerequisites` plus tab-indented recipes is structural and does
//! NOT fit the code-walker's function/call-pass model, and a Makefile grammar
//! would pull a tree-sitter runtime conflicting with this crate's 0.25. Line
//! parsing is robust and dependency-free.
//!
//! ## Chunk model — one chunk per TARGET
//! A RULE line sits at COLUMN 0 (not tab-indented) and has the shape
//! `<targets> : <prerequisites>` where the `:` is a rule separator (NOT the
//! assignment forms `:=` / `::=`). The part before `:` may list MULTIPLE
//! space-separated targets (`a b c: deps`); we emit ONE `chunk_type = "target"`
//! chunk PER target (each is an independently buildable node, and each carries
//! the same prerequisite edges). For each target chunk:
//! - `name` = the target name; `fqn` = the target name (flat — `parent_fqn`
//!   is always `None`). `visibility = Public` (targets have no access control).
//! - `start_line`/`start_byte` at the rule line; `end_line`/`end_byte` extend
//!   through the tab-indented recipe lines to just before the next rule / EOF.
//!   `content` = the rule line + its recipe; `signature` = the rule line. For a
//!   multi-target rule, every emitted target chunk shares the same span /
//!   content / signature (the whole rule block).
//! - Pattern rules (`%.o: %.c`) are emitted as a normal target named `%.o`.
//!
//! ## NOT chunks
//! - Variable assignments (`VAR = x`, `:=`, `::=`, `?=`, `+=`) — see the
//!   rule-vs-assignment disambiguation below.
//! - The `.PHONY` / `.SUFFIXES` / `.DEFAULT` etc. special/builtin targets: a
//!   leading-`.`-uppercase target is a directive, not a buildable node, so it is
//!   NOT emitted as a chunk (its listed names still chunk via their own rules).
//! - `include` / directive lines.
//!
//! ## Rule-vs-assignment disambiguation
//! On a column-0 line, we find the first unescaped `:` or `=`. The line is an
//! ASSIGNMENT (skipped, not a rule) when the FIRST of those operators is one of
//! `=`, `:=`, `::=`, `?=`, `+=`, `!=` — i.e. an `=` appears at-or-immediately-
//! after the first `:`. It is a RULE when a `:` appears that is NOT immediately
//! followed by `=` (covering `target: deps`, bare `target:`, and the double-
//! colon `target:: deps` form, but never `VAR := val`). A line with neither a
//! rule `:` nor an assignment operator (e.g. a lone directive) is ignored.
//!
//! ## Edge model (all [`Provenance::Static`]; `source` = the target's `Name`)
//! - Each prerequisite → [`EdgeKind::Call`] from the target to the prerequisite
//!   (the build DAG: "target depends on prereq"), `metadata.fields["relation"]
//!   = "prerequisite"`. Order-only prerequisites (after a `|` separator) are
//!   treated as prerequisites too. Prereqs that are files rather than targets
//!   simply won't resolve downstream — fine, same as a call to an undefined
//!   function. `$(VAR)` expansion is NOT modelled: a prereq/target containing
//!   `$(...)` is kept literal.
//! - `include <file...>` / `-include` / `sinclude` directives →
//!   [`EdgeKind::Import`] per included makefile, `module_specifier = Some(file)`,
//!   `source` = the file stem.
//!
//! ## Line handling
//! Trailing `\` continues a logical line onto the next (joined with a space).
//! Unescaped `#` starts a comment running to end of line (recipe-line `#` is
//! kept as part of recipe text). Recipe lines (tab-indented, following a rule)
//! belong to that rule and are NOT parsed as rules.
//!
//! ## Matching / limitation
//! `filename_matches` covers `makefile` / `gnumakefile` (the registry lowercases
//! the basename, so `Makefile` / `makefile` / `GNUmakefile` all fold here);
//! `extensions` covers `.mk` / `.make`. The trait only does exact-filename +
//! extension-suffix matching, so a suffixed name like `Makefile.local` does NOT
//! match (route such files by giving them a `.mk` extension).

use crate::types::*;

pub static MAKEFILE: MakefileExtractor = MakefileExtractor;

pub struct MakefileExtractor;

impl SpecialExtractor for MakefileExtractor {
    fn name(&self) -> &'static str {
        "makefile"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &[".mk", ".make"]
    }

    fn filename_matches(&self) -> &'static [&'static str] {
        // `registry::lookup_for_path` lowercases the basename before matching,
        // so these MUST be lowercase. Canonical names: `Makefile` /
        // `GNUmakefile` (case-insensitive in practice).
        &["makefile", "gnumakefile"]
    }

    fn parse(&self, source: &str, rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        Ok(parse_makefile(source, rel_path))
    }
}

/// A physical line with its 1-based number and byte offset in `source`.
struct PhysLine<'a> {
    no: usize,
    byte: usize,
    text: &'a str,
}

/// A logical statement (continuation-joined) that starts at COLUMN 0.
struct LogicalLine {
    /// 1-based line number of the statement's first physical line.
    first_no: usize,
    /// Byte offset of the statement's first physical line.
    first_byte: usize,
    /// The continuation-joined, comment-stripped text.
    joined: String,
}

fn parse_makefile(source: &str, rel_path: &str) -> ExtractionOutput {
    let mut out = ExtractionOutput::default();

    // Split into physical lines (keep blanks; need byte offsets + line numbers).
    let mut phys: Vec<PhysLine> = Vec::new();
    let mut byte_off = 0usize;
    for (idx, raw) in source.split_inclusive('\n').enumerate() {
        let text = raw.strip_suffix('\n').unwrap_or(raw);
        phys.push(PhysLine {
            no: idx + 1,
            byte: byte_off,
            text,
        });
        byte_off += raw.len();
    }

    // Build logical (column-0) statements, joining `\` continuations. A
    // recipe line (tab-indented) is NOT a column-0 statement and is skipped
    // here — recipes are folded into the preceding rule's content by span.
    let mut logicals: Vec<LogicalLine> = Vec::new();
    let mut i = 0usize;
    while i < phys.len() {
        let line = &phys[i];

        // Tab-indented lines are recipe lines (or recipe continuations); they
        // never start a column-0 statement, so skip them here.
        if line.text.starts_with('\t') {
            i += 1;
            continue;
        }

        let trimmed = line.text.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }

        // Start of a logical statement. Join `\` continuation lines.
        let first_no = line.no;
        let first_byte = line.byte;
        let mut joined = strip_comment(strip_continuation(line.text)).to_string();
        let mut last_idx = i;
        while ends_with_continuation(phys[last_idx].text) && last_idx + 1 < phys.len() {
            last_idx += 1;
            joined.push(' ');
            joined.push_str(strip_comment(strip_continuation(phys[last_idx].text)).trim());
        }
        i = last_idx + 1;

        let joined = joined.trim().to_string();
        if joined.is_empty() {
            continue;
        }
        logicals.push(LogicalLine {
            first_no,
            first_byte,
            joined,
        });
    }

    // Index, for each logical statement, the byte where its block ends: the
    // first byte of the NEXT column-0 statement (or EOF). The block includes
    // the statement's continuation lines AND its tab-indented recipe lines.
    let logical_starts: Vec<usize> = logicals.iter().map(|l| l.first_byte).collect();

    for (n, lg) in logicals.iter().enumerate() {
        // include directive → Import edges.
        if let Some(files) = parse_include(&lg.joined) {
            let src = EdgeEndpoint::Name {
                name: file_stem(rel_path),
                module_specifier: None,
            };
            for f in files {
                out.edges.push(RawEdge {
                    source: src.clone(),
                    target: EdgeEndpoint::Name {
                        name: f.clone(),
                        module_specifier: Some(f),
                    },
                    kind: EdgeKind::Import,
                    provenance: Provenance::Static,
                    line: Some(lg.first_no),
                    metadata: relation_meta("include"),
                });
            }
            continue;
        }

        // Rule vs assignment.
        let Some((targets_part, prereqs_part)) = split_rule(&lg.joined) else {
            // Not a rule (assignment, conditional, or other directive): skip.
            continue;
        };

        let targets: Vec<String> = targets_part
            .split_whitespace()
            .map(str::to_string)
            .collect();
        if targets.is_empty() {
            continue;
        }

        // Prerequisites: space-separated, with order-only `|` treated as a
        // plain separator (its prereqs are prereqs too). Kept literal (no
        // `$(VAR)` expansion).
        let prereqs: Vec<String> = prereqs_part
            .split_whitespace()
            .filter(|t| *t != "|")
            .map(str::to_string)
            .collect();

        // Block span: from this statement's first byte to the next logical
        // statement's first byte (or EOF), trailing newline trimmed. This
        // captures the rule line, its continuations, and its recipe lines.
        let block_start_byte = lg.first_byte;
        let block_end_byte = logical_starts.get(n + 1).copied().unwrap_or(source.len());
        let content = source[block_start_byte..block_end_byte]
            .trim_end_matches('\n')
            .to_string();
        let start_line = lg.first_no;
        let end_line = start_line + content.matches('\n').count();
        // The signature is the (logical, continuation-joined) rule line.
        let signature = lg.joined.clone();

        for target in &targets {
            // Skip builtin/special directive targets like `.PHONY`, `.SUFFIXES`:
            // a leading-dot, all-uppercase name is a directive, not a node.
            if is_special_target(target) {
                continue;
            }

            out.chunks.push(RawChunk {
                chunk_type: "target".into(),
                name: target.clone(),
                fqn: Some(target.clone()),
                parent_fqn: None,
                start_line,
                end_line,
                start_byte: block_start_byte,
                end_byte: block_start_byte + content.len(),
                signature: Some(signature.clone()),
                content: content.clone(),
                doc: None,
                receiver: None,
                is_async: false,
                is_static: false,
                is_const: false,
                is_exported: false,
                visibility: Visibility::Public,
                metadata: ChunkMetadata::default(),
            });

            let target_source = EdgeEndpoint::Name {
                name: target.clone(),
                module_specifier: None,
            };
            for prereq in &prereqs {
                out.edges.push(RawEdge {
                    source: target_source.clone(),
                    target: EdgeEndpoint::Name {
                        name: prereq.clone(),
                        module_specifier: None,
                    },
                    kind: EdgeKind::Call,
                    provenance: Provenance::Static,
                    line: Some(lg.first_no),
                    metadata: relation_meta("prerequisite"),
                });
            }
        }
    }

    out
}

/// Whether a column-0 line ends with a continuation backslash. A trailing `\`
/// (after stripping trailing whitespace) marks continuation.
fn ends_with_continuation(text: &str) -> bool {
    text.trim_end().ends_with('\\')
}

/// Strip a single trailing continuation backslash (and trailing whitespace).
fn strip_continuation(text: &str) -> &str {
    let t = text.trim_end();
    t.strip_suffix('\\').unwrap_or(t)
}

/// Strip an unescaped `#` comment from a logical (non-recipe) line. `\#` is an
/// escaped literal hash and is NOT a comment start.
fn strip_comment(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            // Count preceding backslashes; an odd count escapes the `#`.
            let mut bs = 0;
            let mut k = i;
            while k > 0 && bytes[k - 1] == b'\\' {
                bs += 1;
                k -= 1;
            }
            if bs % 2 == 0 {
                return &text[..i];
            }
        }
        i += 1;
    }
    text
}

/// If `joined` is an `include` / `-include` / `sinclude` directive, return the
/// list of included file references (space-separated). Else `None`.
fn parse_include(joined: &str) -> Option<Vec<String>> {
    let mut tokens = joined.split_whitespace();
    let kw = tokens.next()?;
    if kw == "include" || kw == "-include" || kw == "sinclude" {
        let files: Vec<String> = tokens.map(str::to_string).collect();
        if files.is_empty() {
            return None;
        }
        return Some(files);
    }
    None
}

/// Disambiguate a column-0 logical line: if it is a RULE, return
/// `(targets_part, prereqs_part)`; if it is an ASSIGNMENT or anything else,
/// return `None`.
///
/// Strategy: scan for the first occurrence of `:` or `=`. The line is an
/// assignment when an `=` is the first operator, OR a `:`/`::` is immediately
/// followed by `=` (`:=`, `::=`), OR the operator is `?=` / `+=` / `!=`. It is
/// a rule when a `:` (not followed by `=`) appears first.
fn split_rule(joined: &str) -> Option<(String, String)> {
    let bytes = joined.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'=' => {
                // `=` before any rule colon → plain assignment (`VAR = x`).
                return None;
            }
            b'?' | b'+' | b'!' if i + 1 < bytes.len() && bytes[i + 1] == b'=' => {
                // `?=`, `+=`, `!=` assignment.
                return None;
            }
            b':' => {
                // Could be `:=`, `::=`, `::` (double-colon rule), or `:` rule.
                // Skip a run of `:` (handles `::`).
                let mut j = i;
                while j < bytes.len() && bytes[j] == b':' {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'=' {
                    // `:=` or `::=` → assignment, not a rule.
                    return None;
                }
                // Rule separator. Targets = before the first `:`; prereqs =
                // after the colon run (so `target:: deps` works too).
                let targets_part = joined[..i].trim().to_string();
                let prereqs_part = joined[j..].trim().to_string();
                if targets_part.is_empty() {
                    return None;
                }
                // `target: VAR = val` / `:= ` / `+=` etc. is a GNU Make
                // target-specific variable, NOT a rule with prerequisites;
                // emit the target but drop the RHS so we don't create garbage
                // prerequisite edges to VAR / the operator / the value.
                if is_target_specific_var(&prereqs_part) {
                    return Some((targets_part, String::new()));
                }
                return Some((targets_part, prereqs_part));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether the right-hand side of a `target: RHS` is a GNU Make
/// target-specific *variable assignment* (`VAR = v`, `VAR := v`, `VAR ::= v`,
/// `VAR += v`, `VAR ?= v`, `VAR != v`) rather than a prerequisite list. Such a
/// line sets a variable scoped to the target; its RHS must NOT be parsed as
/// prerequisites (which produced edges to `VAR`, the operator and the value).
fn is_target_specific_var(rhs: &str) -> bool {
    let s = rhs.trim_start();
    // Leading Make variable name: [A-Za-z_][A-Za-z0-9_]*
    let name_len = s
        .char_indices()
        .take_while(|(idx, c)| {
            if *idx == 0 {
                c.is_ascii_alphabetic() || *c == '_'
            } else {
                c.is_ascii_alphanumeric() || *c == '_'
            }
        })
        .count();
    if name_len == 0 {
        return false;
    }
    let after = s[name_len..].trim_start();
    after.starts_with('=')
        || after.starts_with(":=")
        || after.starts_with("::=")
        || after.starts_with("+=")
        || after.starts_with("?=")
        || after.starts_with("!=")
}

/// Whether a target name is a builtin/special directive target (e.g. `.PHONY`,
/// `.SUFFIXES`, `.DEFAULT`): a leading `.` followed by an uppercase letter.
/// These are not buildable nodes, so they are not emitted as chunks.
fn is_special_target(target: &str) -> bool {
    let mut chars = target.chars();
    matches!(chars.next(), Some('.')) && matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
}

fn relation_meta(relation: &str) -> EdgeMetadata {
    let mut meta = EdgeMetadata::default();
    meta.fields.insert(
        "relation".to_string(),
        MetadataValue::String(relation.to_string()),
    );
    meta
}

/// The file stem (basename without extension), used as the edge source for
/// `include` directives.
fn file_stem(rel_path: &str) -> String {
    let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
    match base.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
        _ => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_target_specific_var, split_rule};

    #[test]
    fn target_specific_var_recognized() {
        assert!(is_target_specific_var("CFLAGS = -O2"));
        assert!(is_target_specific_var("CFLAGS += -Wall"));
        assert!(is_target_specific_var("CFLAGS := -g"));
        assert!(is_target_specific_var("X ?= y"));
        assert!(is_target_specific_var(" SPACED   =  v"));
        // Genuine prerequisite lists are not variable assignments.
        assert!(!is_target_specific_var("main.o utils.o"));
        assert!(!is_target_specific_var("build"));
        assert!(!is_target_specific_var(""));
    }

    #[test]
    fn split_rule_drops_target_specific_var_rhs() {
        // `target: VAR = val` keeps the target but emits NO prerequisites.
        assert_eq!(
            split_rule("build: CFLAGS = -O2"),
            Some(("build".to_string(), String::new()))
        );
        // A real rule still yields its prerequisites.
        assert_eq!(
            split_rule("build: main.o utils.o"),
            Some(("build".to_string(), "main.o utils.o".to_string()))
        );
        // A plain variable assignment (no rule colon) is not a rule.
        assert_eq!(split_rule("CFLAGS = -O2"), None);
    }
}
