//! Hand-parsed HCL / Terraform [`SpecialExtractor`] (DSL/infra track).
//!
//! HCL configs are brace-delimited (`TYPE [LABEL...] { body }`) with `key =
//! value` attributes, structurally very close to nginx, so this extractor
//! hand-parses rather than using tree-sitter: an HCL grammar would pull a
//! tree-sitter runtime conflicting with this crate's 0.25, and the block
//! dependency edges (a `module`'s `source`) are STRUCTURAL like nginx — not the
//! call-pass model the code-walker expects. Brace matching is robust and
//! dependency-free.
//!
//! ## Tokenizer — quote/comment/heredoc-aware
//! [`tokenize`] scans char-by-char, emitting one token per `{`, `}`, `=`, and
//! per whitespace-delimited word, tracking state so that:
//! - `#` and `//` start a line comment that runs to end of line (dropped);
//! - `/* ... */` is a block comment (dropped, may span lines);
//! - `"..."` double-quoted strings become ONE word token with the quotes
//!   preserved; any `{`, `}`, `=`, `#` INSIDE the quotes is literal. `${...}`
//!   interpolations are NOT brace-matched — the string content is kept opaque,
//!   so a `{` inside an interpolation never opens a block;
//! - heredocs (`<<EOF ... EOF` / `<<-EOF ... EOF`) are opaque from the `<<`
//!   marker until a line whose trimmed content equals the terminator. Heredoc
//!   bytes are consumed WITHOUT emitting tokens, so `{`/`}`/`#` inside a heredoc
//!   body never break block matching (proven by `03-comments-heredoc.tf`).
//!
//! Each token records its byte offset and 1-based line so chunk boundaries map
//! back to the source exactly. HCL uses no statement terminator (`;`), so unlike
//! nginx there is no semicolon token; attributes are `key = value` and are NOT
//! chunks — only blocks are.
//!
//! ## Block tree → chunks (one chunk per BLOCK)
//! A "block" is a `TYPE [LABEL...] { body }` construct (`resource`, `module`,
//! `variable`, `data`, `provider`, `output`, `terraform`, `locals`, plus nested
//! blocks like `lifecycle`, `ingress`, `dynamic`). The token stream is folded
//! into a tree by brace matching; `key = value` attributes are NOT blocks and
//! emit NO chunk. Each block becomes one [`RawChunk`]:
//! - `chunk_type` = the block TYPE keyword (`resource`, `module`, ...);
//! - `name` = the block's quoted LABELS joined with `.`, with the leading TYPE
//!   keyword DROPPED (it's already the `chunk_type`). So `resource "aws_instance"
//!   "web"` → name `aws_instance.web`; `module "vpc"` → `vpc`; `variable
//!   "region"` → `region`. A label-less block (`terraform {`, `locals {`) → name
//!   = the type (`terraform`, `locals`). Sibling blocks with an identical name
//!   get a ` #N` suffix so fqns stay unique;
//! - `fqn` = `.`-joined ancestor block names + this block's name (nested:
//!   `aws_instance.web.lifecycle`), mirroring nginx. `parent_fqn` is the ancestor
//!   path (None at top level);
//! - `start_line`/`start_byte` at the block TYPE keyword; `end_line`/`end_byte`
//!   at the closing `}`. `content` = the block's full text; `signature` = the
//!   block header line (`resource "aws_instance" "web" {`); `visibility =
//!   Public`. Top-level attributes (rare in HCL) are NOT chunks.
//!
//! ## Edge model (all [`Provenance::Static`]; `source` = the enclosing block's
//! `Name`)
//! - A `module` block's `source = "..."` attribute → [`EdgeKind::Import`] with
//!   target `Name { name: <source>, module_specifier: Some(<source>) }` —
//!   `metadata.fields["relation"] = "module_source"`. This is the KEY
//!   high-value edge: a local path (`./modules/vpc`) or registry ref
//!   (`terraform-aws-modules/vpc/aws`) the module depends on.
//!
//! ## Resource-reference edges (v2 — conservative Terraform dep graph)
//! Attribute VALUES inside a block can reference other resources/data/modules by
//! the HCL convention `TYPE.NAME[.attr...]` (`subnet_id = aws_subnet.main.id`).
//! Each such reference becomes a [`EdgeKind::Call`] edge from the ENCLOSING block
//! chunk to the referenced address, so the resource graph downstream can resolve
//! `aws_subnet.main` to that block's chunk. The scan is deliberately CONSERVATIVE
//! to avoid false positives:
//! - The block's OWN body text is scanned (child-block byte spans are blanked
//!   out, so a reference inside a nested block is attributed to that INNERMOST
//!   block chunk, not its parent). The scanner is quote/comment/heredoc aware,
//!   mirroring the tokenizer: line/block comments and heredoc bodies are skipped;
//!   inside double-quoted strings ONLY `${...}` interpolation regions are scanned
//!   (a plain string literal — e.g. a `module` `source = "./x"` path — is NEVER a
//!   reference). Bare expression context and `${...}` bodies are scanned for
//!   dotted identifier chains.
//! - A candidate chain is `head.second[.rest...]` where `head` matches
//!   `^[a-z][a-z0-9_]*$`. The chain is emitted as an edge unless:
//!     * `head` is a RESERVED language scope — `var`, `local`, `each`, `count`,
//!       `path`, `self`, `terraform`, `provider` (these are NOT resources);
//!     * the chain is immediately followed by `(` (a function call like
//!       `jsonencode(...)` / `file(...)`); or
//!     * it is a bare single identifier (no dot).
//! - `ref_kind` metadata + target naming:
//!     * `TYPE.NAME` (resource) → target `Name { "TYPE.NAME" }`, `ref_kind=resource`;
//!     * `data.TYPE.NAME` → strip `data.`, target `Name { "TYPE.NAME" }` (the
//!       `data "TYPE" "NAME"` block is named `TYPE.NAME` here), `ref_kind=data`;
//!     * `module.NAME.output` → target `Name { "NAME" }` (the `module "NAME"`
//!       block is named `NAME`), `ref_kind=module`.
//! - Dedup: per source block, identical `(target_name, ref_kind)` references emit
//!   ONE edge (first-seen line wins). The existing `module` source-Import edge and
//!   the chunk model are UNCHANGED.
//!
//! ## Matching / scope
//! `extensions` claims `.tf` (Terraform) and `.hcl` (generic HCL). `.tfvars` is
//! deliberately NOT claimed: tfvars files are pure `key = value` value
//! assignments with no blocks, so they would produce zero chunks anyway.
//! `filename_matches` is empty.

use crate::types::*;

pub static HCL: HclExtractor = HclExtractor;

pub struct HclExtractor;

impl SpecialExtractor for HclExtractor {
    fn name(&self) -> &'static str {
        "hcl"
    }

    fn extensions(&self) -> &'static [&'static str] {
        // `.tf` (Terraform) + `.hcl` (generic HCL). `.tfvars` is value-only
        // (no blocks) → not claimed.
        &[".tf", ".hcl"]
    }

    fn filename_matches(&self) -> &'static [&'static str] {
        &[]
    }

    fn parse(&self, source: &str, _rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        Ok(parse_hcl(source))
    }
}

/// A lexer token with its source position.
#[derive(Debug, Clone)]
struct Token {
    text: String,
    kind: TokKind,
    /// Byte offset of the token start in `source`.
    byte: usize,
    /// 1-based line of the token start.
    line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokKind {
    Word,
    OpenBrace,
    CloseBrace,
    Equals,
}

/// Quote/comment/heredoc-aware tokenizer. Emits `{`, `}`, `=`, and
/// whitespace-delimited words. `#` and `//` comments run to end of line; `/* */`
/// is a block comment; double-quoted strings become a single word whose inner
/// `{}=#` are literal (and whose `${...}` interpolations are kept opaque);
/// heredocs are consumed opaquely until their terminator line.
fn tokenize(source: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let bytes = source.as_bytes();
    let mut line = 1usize;
    let mut i = 0usize;
    let n = bytes.len();

    // Accumulator for the current word token (may include quoted spans).
    let mut word = String::new();
    let mut word_byte = 0usize;
    let mut word_line = 0usize;

    macro_rules! flush_word {
        () => {
            if !word.is_empty() {
                tokens.push(Token {
                    text: std::mem::take(&mut word),
                    kind: TokKind::Word,
                    byte: word_byte,
                    line: word_line,
                });
            }
        };
    }

    while i < n {
        // NOTE: structural bytes (`{ } = # / " < \\` and whitespace) are all
        // ASCII, so dispatching on `bytes[i] as char` is correct for them. The
        // only hazard is COPYING content bytes — a non-ASCII byte cast to `char`
        // corrupts UTF-8 (one garbage char per continuation byte). Non-ASCII
        // bytes (>= 0x80) never match an ASCII arm, so they fall to `_ =>`,
        // which decodes a whole char via `decode_char_at` (see below). The
        // quoted-string body loop does the same.
        let c = bytes[i] as char;
        match c {
            '\n' => {
                flush_word!();
                line += 1;
                i += 1;
            }
            ' ' | '\t' | '\r' => {
                flush_word!();
                i += 1;
            }
            '#' => {
                // `#` line comment to end of line.
                flush_word!();
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < n && bytes[i + 1] == b'/' => {
                // `//` line comment to end of line.
                flush_word!();
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < n && bytes[i + 1] == b'*' => {
                // `/* ... */` block comment (may span lines).
                flush_word!();
                i += 2;
                while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    if bytes[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                // Consume the closing `*/` (or run to EOF if unterminated).
                if i + 1 < n {
                    i += 2;
                } else {
                    i = n;
                }
            }
            '{' | '}' | '=' => {
                // `==`, `>=`, `<=`, `!=` inside an expression value are NOT block
                // structure; but a bare `=` between brace tokens IS an attribute
                // assignment. We only special-case `=` followed by `=` (an
                // equality operator) to avoid emitting a stray Equals — the
                // attribute parser keys off a single `=` after a Word.
                if c == '=' && i + 1 < n && bytes[i + 1] == b'=' {
                    // Equality operator inside an expression: treat as opaque
                    // word text so it doesn't masquerade as an assignment.
                    if word.is_empty() {
                        word_byte = i;
                        word_line = line;
                    }
                    word.push('=');
                    word.push('=');
                    i += 2;
                    continue;
                }
                flush_word!();
                let kind = match c {
                    '{' => TokKind::OpenBrace,
                    '}' => TokKind::CloseBrace,
                    _ => TokKind::Equals,
                };
                tokens.push(Token {
                    text: c.to_string(),
                    kind,
                    byte: i,
                    line,
                });
                i += 1;
            }
            '"' => {
                // Double-quoted string: braces/equals/comments inside are
                // literal. `${...}` interpolations are kept opaque (NOT
                // brace-matched) — we just copy bytes until the closing quote.
                if word.is_empty() {
                    word_byte = i;
                    word_line = line;
                }
                word.push('"');
                i += 1;
                while i < n {
                    if bytes[i] == b'\\' && i + 1 < n {
                        // Preserve escape sequences verbatim. The escaped byte
                        // may begin a multibyte UTF-8 char (e.g. `\é`), so
                        // decode a whole char rather than casting one byte.
                        word.push('\\');
                        let (ch, len) = decode_char_at(source, i + 1);
                        word.push(ch);
                        if ch == '\n' {
                            line += 1;
                        }
                        i += 1 + len;
                        continue;
                    }
                    // Decode a whole UTF-8 char so non-ASCII string content
                    // (e.g. `source = "./café/vpc"`) is preserved verbatim
                    // instead of being mangled byte-by-byte via `as char`.
                    let (qc, len) = decode_char_at(source, i);
                    word.push(qc);
                    if qc == '\n' {
                        line += 1;
                    }
                    i += len;
                    if qc == '"' {
                        break;
                    }
                }
            }
            '<' if i + 1 < n && bytes[i + 1] == b'<' => {
                // Heredoc: `<<EOF` or `<<-EOF`. Read the terminator word, then
                // consume opaquely until a line whose trimmed text == terminator.
                flush_word!();
                let mut j = i + 2;
                if j < n && bytes[j] == b'-' {
                    j += 1;
                }
                // The terminator is the identifier following `<<`/`<<-`.
                let term_start = j;
                while j < n && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                let terminator = source[term_start..j].to_string();
                if terminator.is_empty() {
                    // Not a real heredoc (e.g. `<<` used some other way); emit
                    // `<<` as opaque word text and move on.
                    word_byte = i;
                    word_line = line;
                    word.push_str("<<");
                    i += 2;
                    continue;
                }
                // Advance i past the marker; skip to end of the marker line.
                i = j;
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
                if i < n {
                    i += 1; // consume newline ending the marker line
                    line += 1;
                }
                // Consume body lines until a line trimming to the terminator.
                loop {
                    let line_start = i;
                    while i < n && bytes[i] != b'\n' {
                        i += 1;
                    }
                    let body_line = &source[line_start..i];
                    let is_term = body_line.trim() == terminator;
                    if i < n {
                        i += 1; // consume the newline
                        line += 1;
                    }
                    if is_term || i >= n {
                        break;
                    }
                }
            }
            _ => {
                // Ordinary word byte. A non-ASCII byte (>= 0x80) lands here
                // (it matches no ASCII arm), so decode a whole UTF-8 char to
                // keep non-ASCII identifiers/paths intact rather than pushing
                // `bytes[i] as char` and shredding the codepoint.
                if word.is_empty() {
                    word_byte = i;
                    word_line = line;
                }
                let (ch, len) = decode_char_at(source, i);
                word.push(ch);
                i += len;
            }
        }
    }
    flush_word!();
    tokens
}

/// Decode the single UTF-8 [`char`] starting at byte offset `i` of `source`,
/// returning `(char, byte_len)`. Pure helper so the tokenizer can advance one
/// whole codepoint at a time when copying word/string content, instead of
/// casting raw bytes (`bytes[i] as char`), which corrupts any non-ASCII run
/// (one garbage char per UTF-8 continuation byte). `i` MUST be a char boundary
/// in `source`; on the (unreachable for valid UTF-8) chance it is not, we fall
/// back to the replacement char advancing one byte so the loop still
/// terminates.
fn decode_char_at(source: &str, i: usize) -> (char, usize) {
    match source[i..].chars().next() {
        Some(ch) => (ch, ch.len_utf8()),
        None => ('\u{FFFD}', 1),
    }
}

/// A parsed block node in the HCL tree.
struct Block {
    /// The block TYPE keyword (e.g. `resource`, `module`).
    keyword: String,
    /// Labels-only name (`.`-joined, TYPE dropped). e.g. `aws_instance.web`.
    name: String,
    /// Byte offset of the block TYPE keyword.
    start_byte: usize,
    /// 1-based line of the block TYPE keyword.
    start_line: usize,
    /// Byte offset just AFTER the closing `}`.
    end_byte: usize,
    /// 1-based line of the closing `}`.
    end_line: usize,
    children: Vec<Block>,
    /// Attributes of interest (currently only `source`) directly in this block.
    attributes: Vec<Attribute>,
}

/// A `key = value` attribute of interest inside a block.
struct Attribute {
    key: String,
    /// The (unquoted) value of the attribute.
    value: String,
    line: usize,
}

/// Recursive-descent over the token stream, building the block tree. Returns the
/// blocks parsed at this level (top-level attributes are not collected — only a
/// block's own `source` attribute matters, handled when we enter the block).
fn parse_level(tokens: &[Token], pos: &mut usize, source: &str) -> Vec<Block> {
    let mut blocks = Vec::new();

    while *pos < tokens.len() {
        let tok = &tokens[*pos];
        match tok.kind {
            TokKind::CloseBrace => {
                // End of the enclosing block; caller consumes the `}`.
                return blocks;
            }
            TokKind::OpenBrace | TokKind::Equals => {
                // Brace/equals with no preceding header word — malformed or a
                // top-level stray; skip it.
                *pos += 1;
            }
            TokKind::Word => {
                // A statement starts with a Word. It's either:
                //   `key = value`        → attribute (look ahead for `=`)
                //   `TYPE [LABELS...] {`  → block (header words up to `{`)
                // Look ahead: if the NEXT token is `=`, it's an attribute.
                if tokens
                    .get(*pos + 1)
                    .is_some_and(|t| t.kind == TokKind::Equals)
                {
                    // Top-level / sibling attribute: not collected here (only a
                    // block's `source` is handled when entering the block). Skip
                    // the `key = value`; if the value is a `{ ... }` object (or
                    // an array containing objects), skip the balanced braces so
                    // they don't desync block matching.
                    *pos += 2; // skip key + `=`
                    skip_attribute_value(tokens, pos);
                    continue;
                }

                // Otherwise gather header words until `{` (a block) or a token
                // that can't be part of a header.
                let stmt_start = *pos;
                let mut j = *pos;
                while j < tokens.len() && tokens[j].kind == TokKind::Word {
                    j += 1;
                }
                if j >= tokens.len() || tokens[j].kind != TokKind::OpenBrace {
                    // Not a block header (e.g. a dangling word); skip the words.
                    *pos = j.max(*pos + 1);
                    continue;
                }
                // Block: words [stmt_start..j] form the header.
                let header: Vec<&str> = tokens[stmt_start..j]
                    .iter()
                    .map(|t| t.text.as_str())
                    .collect();
                let keyword = header.first().copied().unwrap_or("").to_string();
                // Labels = header words after the keyword, unquoted, `.`-joined.
                let labels: Vec<String> = header[1.min(header.len())..]
                    .iter()
                    .map(|w| unquote(w))
                    .collect();
                let name = if labels.is_empty() {
                    keyword.clone()
                } else {
                    labels.join(".")
                };
                let start_byte = tokens[stmt_start].byte;
                let start_line = tokens[stmt_start].line;
                // Recurse into the body (consume the `{`).
                *pos = j + 1;
                // Collect this block's direct `source` attribute while parsing
                // children: we scan the immediate token level for attributes
                // BEFORE recursing into nested blocks. parse_level handles nested
                // blocks; attributes at this level are gathered separately below.
                let (children, attributes) = parse_block_body(tokens, pos, source);
                let (end_byte, end_line) =
                    if *pos < tokens.len() && tokens[*pos].kind == TokKind::CloseBrace {
                        let close = &tokens[*pos];
                        *pos += 1; // consume `}`
                        (close.byte + 1, close.line)
                    } else {
                        (source.len(), source.matches('\n').count() + 1)
                    };
                blocks.push(Block {
                    keyword,
                    name,
                    start_byte,
                    start_line,
                    end_byte,
                    end_line,
                    children,
                    attributes,
                });
            }
        }
    }

    blocks
}

/// Parse the body of a block: gathers direct attributes (`key = value`) AND
/// recurses into nested blocks. Returns (nested blocks, direct attributes).
/// `*pos` is left pointing at the matching `}` (or EOF).
fn parse_block_body(
    tokens: &[Token],
    pos: &mut usize,
    source: &str,
) -> (Vec<Block>, Vec<Attribute>) {
    let mut blocks = Vec::new();
    let mut attributes = Vec::new();

    while *pos < tokens.len() {
        let tok = &tokens[*pos];
        match tok.kind {
            TokKind::CloseBrace => {
                return (blocks, attributes);
            }
            TokKind::OpenBrace | TokKind::Equals => {
                *pos += 1;
            }
            TokKind::Word => {
                if tokens
                    .get(*pos + 1)
                    .is_some_and(|t| t.kind == TokKind::Equals)
                {
                    // Attribute `key = value`.
                    let key = tokens[*pos].text.clone();
                    let line = tokens[*pos].line;
                    *pos += 2; // skip key + `=`
                    let value = if *pos < tokens.len() && tokens[*pos].kind == TokKind::Word {
                        unquote(&tokens[*pos].text)
                    } else {
                        String::new()
                    };
                    // Advance past the value, balancing any `{ ... }` object so
                    // its braces don't desync block matching.
                    skip_attribute_value(tokens, pos);
                    attributes.push(Attribute { key, value, line });
                    continue;
                }
                // A nested block header: collect words to `{`.
                let stmt_start = *pos;
                let mut j = *pos;
                while j < tokens.len() && tokens[j].kind == TokKind::Word {
                    j += 1;
                }
                if j >= tokens.len() || tokens[j].kind != TokKind::OpenBrace {
                    *pos = j.max(*pos + 1);
                    continue;
                }
                let header: Vec<&str> = tokens[stmt_start..j]
                    .iter()
                    .map(|t| t.text.as_str())
                    .collect();
                let keyword = header.first().copied().unwrap_or("").to_string();
                let labels: Vec<String> = header[1.min(header.len())..]
                    .iter()
                    .map(|w| unquote(w))
                    .collect();
                let name = if labels.is_empty() {
                    keyword.clone()
                } else {
                    labels.join(".")
                };
                let start_byte = tokens[stmt_start].byte;
                let start_line = tokens[stmt_start].line;
                *pos = j + 1;
                let (children, child_attrs) = parse_block_body(tokens, pos, source);
                let (end_byte, end_line) =
                    if *pos < tokens.len() && tokens[*pos].kind == TokKind::CloseBrace {
                        let close = &tokens[*pos];
                        *pos += 1;
                        (close.byte + 1, close.line)
                    } else {
                        (source.len(), source.matches('\n').count() + 1)
                    };
                blocks.push(Block {
                    keyword,
                    name,
                    start_byte,
                    start_line,
                    end_byte,
                    end_line,
                    children,
                    attributes: child_attrs,
                });
            }
        }
    }

    (blocks, attributes)
}

fn parse_hcl(source: &str) -> ExtractionOutput {
    let mut out = ExtractionOutput::default();
    let tokens = tokenize(source);
    let mut pos = 0usize;
    let blocks = parse_level(&tokens, &mut pos, source);

    emit_blocks(&blocks, None, source, &mut out);

    out
}

/// Emit one chunk per block, then recurse. Sibling blocks with identical names
/// get a ` #N` suffix so fqns stay unique (mirrors nginx).
fn emit_blocks(
    blocks: &[Block],
    parent_fqn: Option<&str>,
    source: &str,
    out: &mut ExtractionOutput,
) {
    let mut name_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for b in blocks {
        *name_counts.entry(b.name.as_str()).or_insert(0) += 1;
    }
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();

    for b in blocks {
        let display_name = if name_counts.get(b.name.as_str()).copied().unwrap_or(0) > 1 {
            let idx = seen.entry(b.name.as_str()).or_insert(0);
            *idx += 1;
            format!("{} #{}", b.name, idx)
        } else {
            b.name.clone()
        };

        let fqn = match parent_fqn {
            Some(p) => format!("{p}.{display_name}"),
            None => display_name.clone(),
        };

        let content = source[b.start_byte..b.end_byte.min(source.len())].to_string();
        let signature = source[b.start_byte..b.end_byte.min(source.len())]
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();

        out.chunks.push(RawChunk {
            chunk_type: b.keyword.clone(),
            name: display_name.clone(),
            fqn: Some(fqn.clone()),
            parent_fqn: parent_fqn.map(String::from),
            start_line: b.start_line,
            end_line: b.end_line,
            start_byte: b.start_byte,
            end_byte: b.end_byte,
            signature: Some(signature),
            content,
            doc: None,
            receiver: None,
            is_async: false,
            is_static: false,
            is_const: false,
            is_exported: false,
            visibility: Visibility::Public,
            metadata: ChunkMetadata::default(),
        });

        // Conservative resource-reference edges: scan THIS block's own body
        // (child-block spans blanked out) for `TYPE.NAME[.attr]` references and
        // emit one Call edge per distinct (target, ref_kind).
        let ref_source = EdgeEndpoint::Name {
            name: display_name.clone(),
            module_specifier: None,
        };
        for r in collect_references(b, source) {
            out.edges.push(RawEdge {
                source: ref_source.clone(),
                target: EdgeEndpoint::Name {
                    name: r.target,
                    module_specifier: None,
                },
                kind: EdgeKind::Call,
                provenance: Provenance::Static,
                line: Some(r.line),
                metadata: ref_kind_meta(r.ref_kind),
            });
        }

        // The KEY edge: a `module` block's `source = "..."` → Import.
        if b.keyword == "module" {
            let block_source = EdgeEndpoint::Name {
                name: display_name.clone(),
                module_specifier: None,
            };
            for a in &b.attributes {
                if a.key == "source" && !a.value.is_empty() {
                    out.edges.push(RawEdge {
                        source: block_source.clone(),
                        target: EdgeEndpoint::Name {
                            name: a.value.clone(),
                            module_specifier: Some(a.value.clone()),
                        },
                        kind: EdgeKind::Import,
                        provenance: Provenance::Static,
                        line: Some(a.line),
                        metadata: relation_meta("module_source"),
                    });
                }
            }
        }

        emit_blocks(&b.children, Some(&fqn), source, out);
    }
}

/// Advance `*pos` past an attribute's value. A scalar value is a single Word
/// token; an object/map value is a `{ ... }` whose braces must be balanced and
/// skipped so they don't desync the surrounding block tree (e.g. `tags = {
/// Name = "x" }`, or `default = { a = { b = 1 } }`). Arrays of objects
/// (`[{...}]`) are handled too: the leading `[` is a Word, then the brace
/// groups are balanced away.
fn skip_attribute_value(tokens: &[Token], pos: &mut usize) {
    // Consume a leading scalar / array-open Word, if any.
    if *pos < tokens.len() && tokens[*pos].kind == TokKind::Word {
        *pos += 1;
    }
    // Balance away any `{ ... }` object value(s) that follow.
    while *pos < tokens.len() && tokens[*pos].kind == TokKind::OpenBrace {
        let mut depth = 0usize;
        while *pos < tokens.len() {
            match tokens[*pos].kind {
                TokKind::OpenBrace => depth += 1,
                TokKind::CloseBrace => {
                    depth -= 1;
                    if depth == 0 {
                        *pos += 1;
                        break;
                    }
                }
                _ => {}
            }
            *pos += 1;
        }
    }
}

/// Remove surrounding matching double quotes from a token, if present.
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 2 && b[0] == b'"' && b[b.len() - 1] == b'"' {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn relation_meta(relation: &str) -> EdgeMetadata {
    let mut meta = EdgeMetadata::default();
    meta.fields.insert(
        "relation".to_string(),
        MetadataValue::String(relation.to_string()),
    );
    meta
}

fn ref_kind_meta(ref_kind: &str) -> EdgeMetadata {
    let mut meta = EdgeMetadata::default();
    meta.fields.insert(
        "ref_kind".to_string(),
        MetadataValue::String(ref_kind.to_string()),
    );
    meta
}

/// Reserved leading scopes that look like `TYPE.NAME` but are NOT resource
/// references (HCL language namespaces / built-ins).
const RESERVED_SCOPES: &[&str] = &[
    "var",
    "local",
    "each",
    "count",
    "path",
    "self",
    "terraform",
    "provider",
];

/// A detected reference and the edge fields it maps to.
struct Reference {
    /// Target chunk name (`TYPE.NAME` for resource/data, `NAME` for module).
    target: String,
    /// `"resource"` / `"data"` / `"module"`.
    ref_kind: &'static str,
    /// 1-based source line of the reference.
    line: usize,
}

/// Scan a block's OWN body (child-block byte spans blanked out so nested-block
/// references are attributed to the innermost block) for `TYPE.NAME[.attr]`
/// references, returning deduped `(target, ref_kind)` references (first-seen line
/// wins). The block header itself (`resource "TYPE" "NAME" {`) is excluded so the
/// quoted labels are never mistaken for references.
fn collect_references(block: &Block, source: &str) -> Vec<Reference> {
    let body_end = block.end_byte.min(source.len());
    if block.start_byte >= body_end {
        return Vec::new();
    }
    // Build a per-byte mask: bytes belonging to a child block are blanked to a
    // space so the scanner attributes their references to the child (innermost)
    // chunk, not this one. We also blank the block HEADER (everything up to and
    // including the opening `{`) so the quoted type/name labels aren't scanned.
    let region = &source[block.start_byte..body_end];
    let mut buf: Vec<u8> = region.as_bytes().to_vec();
    // Blank the header: up to the first `{` at this level. The first `{` in the
    // region opens this block's body (the tokenizer guarantees the header has no
    // stray braces — quoted labels keep their braces opaque, but labels here are
    // plain identifiers/strings without `{`).
    if let Some(open) = buf.iter().position(|&c| c == b'{') {
        for byte in buf.iter_mut().take(open + 1) {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    // Blank child-block spans.
    for child in &block.children {
        let cs = child.start_byte.saturating_sub(block.start_byte);
        let ce = child
            .end_byte
            .saturating_sub(block.start_byte)
            .min(buf.len());
        for byte in buf.iter_mut().take(ce).skip(cs) {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    let scan = String::from_utf8_lossy(&buf);
    scan_references(&scan, block.start_line)
}

/// Quote/comment/heredoc-aware scan of expression text for dotted-identifier
/// references. `base_line` is the 1-based line of the text's first byte.
fn scan_references(text: &str, base_line: usize) -> Vec<Reference> {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut i = 0usize;
    let mut line = base_line;
    let mut found: Vec<Reference> = Vec::new();
    let mut seen: std::collections::HashSet<(String, &'static str)> =
        std::collections::HashSet::new();

    while i < n {
        let c = bytes[i];
        match c {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'#' => {
                // Line comment to EOL.
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < n && bytes[i + 1] == b'/' => {
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < n && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    if bytes[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i = (i + 2).min(n);
            }
            b'"' => {
                // Inside a double-quoted string only `${...}` interpolation
                // regions hold references; plain literal text is skipped.
                i += 1;
                while i < n {
                    if bytes[i] == b'\\' && i + 1 < n {
                        if bytes[i + 1] == b'\n' {
                            line += 1;
                        }
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    if bytes[i] == b'$' && i + 1 < n && bytes[i + 1] == b'{' {
                        // Scan the interpolation body until the matching `}`.
                        i += 2;
                        let mut depth = 1usize;
                        while i < n && depth > 0 {
                            match bytes[i] {
                                b'{' => {
                                    depth += 1;
                                    i += 1;
                                }
                                b'}' => {
                                    depth -= 1;
                                    i += 1;
                                }
                                b'\n' => {
                                    line += 1;
                                    i += 1;
                                }
                                _ => {
                                    if try_reference(bytes, &mut i, line, &mut seen, &mut found) {
                                        // i advanced past the chain.
                                    } else {
                                        i += 1;
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    if bytes[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
            _ => {
                if try_reference(bytes, &mut i, line, &mut seen, &mut found) {
                    // i advanced past the chain.
                } else {
                    i += 1;
                }
            }
        }
    }

    found
}

/// At position `*i`, if a dotted-identifier reference chain starts here (and the
/// preceding byte is not an identifier char, so we begin at a chain boundary),
/// classify it and, when it's a real resource/data/module reference, record it.
/// Advances `*i` past the whole chain and returns `true` when a chain (ref or
/// skipped) was consumed; returns `false` (leaving `*i`) when no identifier
/// starts here.
fn try_reference(
    bytes: &[u8],
    i: &mut usize,
    line: usize,
    seen: &mut std::collections::HashSet<(String, &'static str)>,
    found: &mut Vec<Reference>,
) -> bool {
    let n = bytes.len();
    let start = *i;
    // Must start at an identifier boundary (the previous byte is not part of an
    // identifier) and the first char must begin an identifier.
    if start > 0 && is_ident_byte(bytes[start - 1]) {
        return false;
    }
    if !is_ident_start(bytes[start]) {
        return false;
    }
    // Read the full dotted chain `seg(.seg)*`.
    let mut j = start;
    let mut segments: Vec<(usize, usize)> = Vec::new();
    loop {
        let seg_start = j;
        while j < n && is_ident_byte(bytes[j]) {
            j += 1;
        }
        if j == seg_start {
            break;
        }
        segments.push((seg_start, j));
        if j < n && bytes[j] == b'.' && j + 1 < n && is_ident_start(bytes[j + 1]) {
            j += 1; // consume `.`
            continue;
        }
        break;
    }
    // Consume the whole chain regardless of classification.
    *i = j;

    // Function call: a chain immediately followed by `(` is a call, not a ref.
    if j < n && bytes[j] == b'(' {
        return true;
    }
    // Bare single identifier (no dot) → not a reference.
    if segments.len() < 2 {
        return true;
    }

    let seg =
        |idx: usize| std::str::from_utf8(&bytes[segments[idx].0..segments[idx].1]).unwrap_or("");
    let head = seg(0);

    // `head` must be a lowercase resource-type-like identifier.
    if !is_type_ident(head) {
        return true;
    }

    let (target, ref_kind) = if head == "data" {
        // `data.TYPE.NAME[...]` → target `TYPE.NAME`.
        if segments.len() < 3 {
            return true;
        }
        (format!("{}.{}", seg(1), seg(2)), "data")
    } else if head == "module" {
        // `module.NAME[.output]` → target `NAME`.
        (seg(1).to_string(), "module")
    } else if RESERVED_SCOPES.contains(&head) {
        // var/local/each/count/path/self/terraform/provider → not a resource.
        return true;
    } else {
        // `TYPE.NAME[.attr]` resource reference → target `TYPE.NAME`.
        (format!("{}.{}", seg(0), seg(1)), "resource")
    };

    if target.is_empty()
        || target.contains("..")
        || target.starts_with('.')
        || target.ends_with('.')
    {
        return true;
    }
    if seen.insert((target.clone(), ref_kind)) {
        found.push(Reference {
            target,
            ref_kind,
            line,
        });
    }
    true
}

/// `^[a-z][a-z0-9_]*$` — a resource-type-like leading identifier.
fn is_type_ident(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || !b[0].is_ascii_lowercase() {
        return false;
    }
    b.iter()
        .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `decode_char_at` returns the whole UTF-8 codepoint and its byte length,
    /// so the tokenizer advances one codepoint at a time instead of shredding
    /// multibyte runs the way `bytes[i] as char` did.
    #[test]
    fn decode_char_at_handles_multibyte() {
        // 1-byte (ASCII), 2-byte (é), 3-byte (€), 4-byte (😀) codepoints.
        assert_eq!(decode_char_at("a", 0), ('a', 1));
        assert_eq!(decode_char_at("é", 0), ('é', 2));
        assert_eq!(decode_char_at("€", 0), ('€', 3));
        assert_eq!(decode_char_at("😀", 0), ('😀', 4));
        // Decoding at a non-zero (valid) char boundary.
        let s = "x€"; // 'x' is 1 byte, '€' starts at byte 1.
        assert_eq!(decode_char_at(s, 1), ('€', 3));
    }

    /// A quoted string containing non-ASCII bytes must round-trip verbatim
    /// through the tokenizer into a single Word token, not get mangled into one
    /// garbage char per continuation byte.
    #[test]
    fn tokenize_preserves_non_ascii_quoted_string() {
        let toks = tokenize(r#"source = "./café/vpc""#);
        let words: Vec<&str> = toks
            .iter()
            .filter(|t| t.kind == TokKind::Word)
            .map(|t| t.text.as_str())
            .collect();
        // key word + the quoted value word (quotes preserved).
        assert_eq!(words, vec!["source", "\"./café/vpc\""]);
        assert_eq!(unquote(words[1]), "./café/vpc");
    }

    /// Bare (unquoted) non-ASCII word content is also preserved verbatim.
    #[test]
    fn tokenize_preserves_non_ascii_bare_word() {
        let toks = tokenize("café_ä = 1");
        let first = toks.iter().find(|t| t.kind == TokKind::Word).unwrap();
        assert_eq!(first.text, "café_ä");
    }

    /// End-to-end: a `module` source with non-ASCII path yields an Import edge
    /// whose target preserves the UTF-8 bytes (regression for the `as char`
    /// corruption that flowed into the Import target / ref edge targets).
    #[test]
    fn module_source_non_ascii_import_target_preserved() {
        let src = "module \"vpc\" {\n  source = \"./café/vpc\"\n}\n";
        let out = parse_hcl(src);
        let import = out
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::Import))
            .expect("module source Import edge");
        match &import.target {
            EdgeEndpoint::Name {
                name,
                module_specifier,
            } => {
                assert_eq!(name, "./café/vpc");
                assert_eq!(module_specifier.as_deref(), Some("./café/vpc"));
            }
            other => panic!("unexpected import target: {other:?}"),
        }
    }
}
