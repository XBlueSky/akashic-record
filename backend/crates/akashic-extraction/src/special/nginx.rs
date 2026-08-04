//! Hand-parsed nginx config [`SpecialExtractor`] (DSL/infra track).
//!
//! nginx configs are brace-delimited (`key params { ... }`) and
//! semicolon-terminated (`key value;`), so this extractor hand-parses rather
//! than using tree-sitter: DSL grammars pull a conflicting old tree-sitter
//! incompatible with this crate's 0.25 runtime, and brace/semicolon parsing is
//! robust and dependency-free.
//!
//! ## Tokenizer — quote/comment-aware
//! [`tokenize`] scans the source character-by-character, emitting one token per
//! `{`, `}`, `;`, and per whitespace-delimited word. It tracks state so that:
//! - `#` starts a comment that runs to end of line (the `#` and the rest of the
//!   physical line are dropped entirely);
//! - `"..."` and `'...'` quoted strings are emitted as ONE word token with the
//!   quotes preserved, and any `{`, `}`, `;`, or `#` INSIDE the quotes is
//!   literal (NOT treated as structure) — this is what lets
//!   `04-nested.conf`'s `add_header X "a{b};#c"` parse correctly.
//!
//! Each token records its byte offset and 1-based line so chunk boundaries map
//! back to the source exactly.
//!
//! ## Block tree → chunks (one chunk per BLOCK)
//! A "block" is a `key params { ... }` construct (`http`, `server`, `location`,
//! `upstream`, `events`, `map`, `geo`, ...). The token stream is folded into a
//! tree by brace matching; bare `key value;` directives are NOT blocks and emit
//! NO chunk (only blocks do). Each block becomes one [`RawChunk`]:
//! - `chunk_type` = the block keyword (`http`, `server`, `location`, ...);
//! - `name` = keyword + trimmed params (e.g. `location /api`, `upstream
//!   backend`, bare `server`). Sibling blocks with an identical name are
//!   disambiguated with a ` #N` suffix (`server #2`) so fqns stay unique;
//! - `fqn` = `.`-joined ancestor block names + this block's name
//!   (`http.server.location /api`), mirroring the markdown section model;
//!   `parent_fqn` is the ancestor path (None at top level);
//! - `start_line`/`start_byte` at the block keyword; `end_line`/`end_byte` at
//!   the closing `}`. `content` = the block's full text; `signature` = the
//!   block header (`location /api {`); `visibility = Public`.
//!
//! ## Edge model (all [`Provenance::Static`])
//! Edge `source` is the enclosing block's `Name`, or a file-level `Name` (the
//! file stem) for top-level directives.
//! - `proxy_pass` / `fastcgi_pass` / `uwsgi_pass` / `grpc_pass <ref>;`: strip a
//!   `http://`/`https://` scheme and take the host. If the host matches a
//!   defined `upstream` block name → [`EdgeKind::RoutesTo`] to that upstream
//!   (intra-file dependency); else (an external host/IP) → still a
//!   [`EdgeKind::RoutesTo`] to the literal host string. A reference that is a bare
//!   nginx variable (starts with `$`, e.g. `proxy_pass $backend;`) is SKIPPED
//!   (unresolvable statically). The directive name is recorded in
//!   `metadata.fields["directive"]`.
//! - `include <path>;`: [`EdgeKind::Import`] with target `Name { name: <path>,
//!   module_specifier: Some(<path>) }`. Globs (`include sites-enabled/*.conf;`)
//!   are kept as the literal path.
//!
//! Only the proxy_pass-family (upstream refs) and `include` are modelled; other
//! directives are intentionally not turned into edges.
//!
//! ## Matching / scope
//! `filename_matches` covers the exact name `nginx.conf`; `extensions` claims
//! `.conf`. `.conf` is broad (many tools use it), so akashic treats ALL `.conf`
//! files as nginx-style config — an accepted scope choice for this track.

use crate::types::*;

pub static NGINX: NginxExtractor = NginxExtractor;

pub struct NginxExtractor;

impl SpecialExtractor for NginxExtractor {
    fn name(&self) -> &'static str {
        "nginx"
    }

    fn extensions(&self) -> &'static [&'static str] {
        // `.conf` is broad — akashic treats all `.conf` files as nginx-style.
        &[".conf"]
    }

    fn filename_matches(&self) -> &'static [&'static str] {
        // `registry::lookup_for_path` lowercases the basename before matching.
        &["nginx.conf"]
    }

    fn parse(&self, source: &str, rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        Ok(parse_nginx(source, rel_path))
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
    Semicolon,
}

/// Quote/comment-aware tokenizer. Emits `{`, `}`, `;`, and whitespace-delimited
/// words. `#` comments run to end of line; quoted strings (`"..."`, `'...'`)
/// become a single word whose inner `{}`;`#` are literal.
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
        // Only the ASCII structural bytes (`\n`, whitespace, `#`, `{}` `;`,
        // quotes) drive control flow; for those, `bytes[i] as char` is exact.
        // Word/quoted-string CONTENT is copied via `decode_char_at` instead, so
        // a non-ASCII codepoint (e.g. `münchen.example.com`) is preserved whole
        // rather than shredded into one garbage char per UTF-8 continuation byte.
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
                // Comment to end of line.
                flush_word!();
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '{' | '}' | ';' => {
                flush_word!();
                let kind = match c {
                    '{' => TokKind::OpenBrace,
                    '}' => TokKind::CloseBrace,
                    _ => TokKind::Semicolon,
                };
                tokens.push(Token {
                    text: c.to_string(),
                    kind,
                    byte: i,
                    line,
                });
                i += 1;
            }
            '"' | '\'' => {
                // Quoted string: braces/semicolons/comments inside are literal.
                if word.is_empty() {
                    word_byte = i;
                    word_line = line;
                }
                let quote = bytes[i];
                word.push(c);
                i += 1;
                while i < n {
                    if bytes[i] == b'\\' && i + 1 < n {
                        // Preserve escape sequences verbatim. The escaped char
                        // may be non-ASCII (e.g. `\ä`), so decode a whole char.
                        word.push('\\');
                        let (ech, elen) = decode_char_at(source, i + 1);
                        word.push(ech);
                        if bytes[i + 1] == b'\n' {
                            line += 1;
                        }
                        i += 1 + elen;
                        continue;
                    }
                    // Decode a whole UTF-8 char so multibyte content (and the
                    // closing quote) survive intact, not `bytes[i] as char`.
                    let (qc, qlen) = decode_char_at(source, i);
                    word.push(qc);
                    if bytes[i] == b'\n' {
                        line += 1;
                    }
                    let was_quote = bytes[i] == quote;
                    i += qlen;
                    if was_quote {
                        break;
                    }
                }
            }
            _ => {
                if word.is_empty() {
                    word_byte = i;
                    word_line = line;
                }
                // Ordinary word char. A non-ASCII byte (>= 0x80) lands here (it
                // matches no ASCII arm above), so decode a whole UTF-8 char to
                // keep non-ASCII identifiers/hostnames intact rather than
                // pushing `bytes[i] as char` and corrupting the codepoint.
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

/// A parsed block node in the config tree.
struct Block {
    /// The block keyword (e.g. `server`, `location`).
    keyword: String,
    /// keyword + params, trimmed (e.g. `location /api`).
    name: String,
    /// Byte offset of the block keyword.
    start_byte: usize,
    /// 1-based line of the block keyword.
    start_line: usize,
    /// Byte offset just AFTER the closing `}`.
    end_byte: usize,
    /// 1-based line of the closing `}`.
    end_line: usize,
    children: Vec<Block>,
    /// pass-family + include directives directly inside this block.
    directives: Vec<Directive>,
}

/// A directive of interest (proxy_pass-family or include) inside a block.
struct Directive {
    /// The directive keyword (`proxy_pass`, `include`, ...).
    keyword: String,
    /// The first argument (the reference / path).
    arg: String,
    line: usize,
}

/// Recursive-descent over the token stream, building the block tree. Returns the
/// blocks parsed and the loose directives at this level.
fn parse_level(tokens: &[Token], pos: &mut usize, source: &str) -> (Vec<Block>, Vec<Directive>) {
    let mut blocks = Vec::new();
    let mut directives = Vec::new();

    while *pos < tokens.len() {
        let tok = &tokens[*pos];
        match tok.kind {
            TokKind::CloseBrace => {
                // End of the enclosing block; caller consumes the `}`.
                return (blocks, directives);
            }
            TokKind::Semicolon => {
                // Stray semicolon (e.g. empty statement); skip.
                *pos += 1;
            }
            TokKind::OpenBrace => {
                // Brace with no preceding keyword — malformed; skip it.
                *pos += 1;
            }
            TokKind::Word => {
                // Gather words until `{` (a block) or `;` (a directive).
                let stmt_start = *pos;
                let mut j = *pos;
                while j < tokens.len()
                    && tokens[j].kind != TokKind::OpenBrace
                    && tokens[j].kind != TokKind::Semicolon
                    && tokens[j].kind != TokKind::CloseBrace
                {
                    j += 1;
                }
                if j >= tokens.len() {
                    // Trailing words with no terminator; stop.
                    *pos = j;
                    break;
                }
                match tokens[j].kind {
                    TokKind::OpenBrace => {
                        // Block: words [stmt_start..j] form the header.
                        let header: Vec<&str> = tokens[stmt_start..j]
                            .iter()
                            .map(|t| t.text.as_str())
                            .collect();
                        let keyword = header.first().copied().unwrap_or("").to_string();
                        let name = header.join(" ");
                        let start_byte = tokens[stmt_start].byte;
                        let start_line = tokens[stmt_start].line;
                        // Recurse into the body (consume the `{`).
                        *pos = j + 1;
                        let (children, child_directives) = parse_level(tokens, pos, source);
                        // `*pos` now points at the matching `}` (or EOF).
                        let (end_byte, end_line) =
                            if *pos < tokens.len() && tokens[*pos].kind == TokKind::CloseBrace {
                                let close = &tokens[*pos];
                                *pos += 1; // consume `}`
                                (close.byte + 1, close.line)
                            } else {
                                (
                                    source.len(),
                                    source[..source.len()].matches('\n').count() + 1,
                                )
                            };
                        blocks.push(Block {
                            keyword,
                            name,
                            start_byte,
                            start_line,
                            end_byte,
                            end_line,
                            children,
                            directives: child_directives,
                        });
                    }
                    TokKind::Semicolon => {
                        // Directive: words [stmt_start..j].
                        let keyword = tokens[stmt_start].text.clone();
                        let arg = tokens
                            .get(stmt_start + 1)
                            .map(|t| t.text.clone())
                            .unwrap_or_default();
                        directives.push(Directive {
                            keyword,
                            arg,
                            line: tokens[stmt_start].line,
                        });
                        *pos = j + 1; // consume `;`
                    }
                    TokKind::CloseBrace => {
                        // Directive without `;`, terminated by `}` (tolerate it).
                        let keyword = tokens[stmt_start].text.clone();
                        let arg = tokens
                            .get(stmt_start + 1)
                            .map(|t| t.text.clone())
                            .unwrap_or_default();
                        directives.push(Directive {
                            keyword,
                            arg,
                            line: tokens[stmt_start].line,
                        });
                        *pos = j;
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    (blocks, directives)
}

const PASS_DIRECTIVES: &[&str] = &["proxy_pass", "fastcgi_pass", "uwsgi_pass", "grpc_pass"];

fn parse_nginx(source: &str, rel_path: &str) -> ExtractionOutput {
    let mut out = ExtractionOutput::default();
    let tokens = tokenize(source);
    let mut pos = 0usize;
    let (blocks, top_directives) = parse_level(&tokens, &mut pos, source);

    // First pass: collect all `upstream <name>` block names (any nesting depth)
    // so pass-family edges can decide intra-file Call-to-upstream.
    let mut upstreams: Vec<String> = Vec::new();
    collect_upstreams(&blocks, &mut upstreams);

    // Emit block chunks + their edges, recursively.
    emit_blocks(&blocks, None, source, &upstreams, &mut out);

    // Top-level directives are NOT chunks, but `include` / pass directives still
    // emit edges with a file-level source endpoint.
    let file_source = EdgeEndpoint::Name {
        name: file_stem(rel_path),
        module_specifier: None,
    };
    for d in &top_directives {
        emit_directive_edge(d, &file_source, &upstreams, &mut out);
    }

    out
}

fn collect_upstreams(blocks: &[Block], acc: &mut Vec<String>) {
    for b in blocks {
        if b.keyword == "upstream" {
            // The upstream's identifier is its second header word, if any.
            let id = b.name.strip_prefix("upstream").map_or("", str::trim);
            if !id.is_empty() {
                acc.push(id.to_string());
            }
        }
        collect_upstreams(&b.children, acc);
    }
}

/// Emit one chunk per block, then recurse. Sibling blocks with identical names
/// get a ` #N` suffix so fqns stay unique.
fn emit_blocks(
    blocks: &[Block],
    parent_fqn: Option<&str>,
    source: &str,
    upstreams: &[String],
    out: &mut ExtractionOutput,
) {
    // Count duplicate names among siblings to decide disambiguation.
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

        // Edges from directives directly inside this block.
        let block_source = EdgeEndpoint::Name {
            name: display_name.clone(),
            module_specifier: None,
        };
        for d in &b.directives {
            emit_directive_edge(d, &block_source, upstreams, out);
        }

        emit_blocks(&b.children, Some(&fqn), source, upstreams, out);
    }
}

/// Emit a RoutesTo (pass-family → upstream/host) or Import (include) edge for one
/// directive, attributed to `source_endpoint`. No-op for other directives.
fn emit_directive_edge(
    d: &Directive,
    source_endpoint: &EdgeEndpoint,
    upstreams: &[String],
    out: &mut ExtractionOutput,
) {
    if d.keyword == "include" {
        let path = unquote(&d.arg);
        if path.is_empty() {
            return;
        }
        out.edges.push(RawEdge {
            source: source_endpoint.clone(),
            target: EdgeEndpoint::Name {
                name: path.clone(),
                module_specifier: Some(path),
            },
            kind: EdgeKind::Import,
            provenance: Provenance::Static,
            line: Some(d.line),
            metadata: directive_meta(&d.keyword),
        });
        return;
    }

    if PASS_DIRECTIVES.contains(&d.keyword.as_str()) {
        let host = pass_host(&d.arg);
        // Skip bare variable references (unresolvable statically).
        if host.is_empty() || host.starts_with('$') {
            return;
        }
        out.edges.push(RawEdge {
            source: source_endpoint.clone(),
            target: EdgeEndpoint::Name {
                name: host.clone(),
                // Resolved-to-upstream RoutesTo carries no module_specifier; an
                // external host is recorded the same way (the host string IS
                // the target). Whether it's an upstream is implicit in matching.
                module_specifier: None,
            },
            kind: EdgeKind::RoutesTo,
            provenance: Provenance::Static,
            line: Some(d.line),
            metadata: {
                let mut m = directive_meta(&d.keyword);
                let resolved = upstreams.iter().any(|u| u == &host);
                m.fields.insert(
                    "target_kind".to_string(),
                    MetadataValue::String(if resolved { "upstream" } else { "external" }.into()),
                );
                m
            },
        });
    }
}

/// Strip a leading `http://` / `https://` scheme and take the host portion
/// (up to the first `/`, `:`, or whitespace) of a pass directive's argument.
fn pass_host(arg: &str) -> String {
    let arg = unquote(arg);
    let without_scheme = arg
        .strip_prefix("http://")
        .or_else(|| arg.strip_prefix("https://"))
        .unwrap_or(&arg);
    // Variables like `$backend` are returned as-is (caller skips them).
    without_scheme
        .split(['/', ' '])
        .next()
        .unwrap_or(without_scheme)
        .to_string()
}

/// Remove surrounding matching single/double quotes from a token, if present.
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn directive_meta(directive: &str) -> EdgeMetadata {
    let mut meta = EdgeMetadata::default();
    meta.fields.insert(
        "directive".to_string(),
        MetadataValue::String(directive.to_string()),
    );
    meta
}

/// The file stem (basename without extension), used as the file-level edge
/// source for top-level directives.
fn file_stem(rel_path: &str) -> String {
    let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
    match base.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
        _ => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `decode_char_at` returns the whole UTF-8 codepoint and its byte length,
    /// so the tokenizer advances one codepoint at a time instead of shredding
    /// multibyte runs the way `bytes[i] as char` did.
    #[test]
    fn decode_char_at_handles_multibyte() {
        // 1-byte (ASCII), 2-byte (ü), 3-byte (€), 4-byte (😀) codepoints.
        assert_eq!(decode_char_at("a", 0), ('a', 1));
        assert_eq!(decode_char_at("ü", 0), ('ü', 2));
        assert_eq!(decode_char_at("€", 0), ('€', 3));
        assert_eq!(decode_char_at("😀", 0), ('😀', 4));
        // Decoding at a non-zero (valid) char boundary.
        let s = "x€"; // 'x' is 1 byte, '€' starts at byte 1.
        assert_eq!(decode_char_at(s, 1), ('€', 3));
    }

    /// A bare (unquoted) non-ASCII word — e.g. an IDN hostname in
    /// `server_name münchen.example.com;` — must survive verbatim, not get
    /// mangled into one garbage char per UTF-8 continuation byte.
    #[test]
    fn tokenize_preserves_non_ascii_bare_word() {
        let toks = tokenize("server_name münchen.example.com;");
        let words: Vec<&str> = toks
            .iter()
            .filter(|t| t.kind == TokKind::Word)
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(words, vec!["server_name", "münchen.example.com"]);
    }

    /// A quoted string containing non-ASCII bytes must round-trip verbatim
    /// through the tokenizer into a single Word token (quotes preserved).
    #[test]
    fn tokenize_preserves_non_ascii_quoted_string() {
        let toks = tokenize(r#"add_header X-City "münchen";"#);
        let words: Vec<&str> = toks
            .iter()
            .filter(|t| t.kind == TokKind::Word)
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(words, vec!["add_header", "X-City", "\"münchen\""]);
        assert_eq!(unquote(words[2]), "münchen");
    }

    /// End-to-end: a `proxy_pass` to a non-ASCII (IDN) host yields a RoutesTo
    /// edge whose target preserves the UTF-8 bytes — regression for the
    /// `as char` corruption that previously flowed into the edge target name.
    #[test]
    fn proxy_pass_non_ascii_host_target_preserved() {
        let src = "location / {\n    proxy_pass http://münchen.example.com;\n}\n";
        let out = parse_nginx(src, "test.conf");
        let edge = out
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::RoutesTo))
            .expect("proxy_pass RoutesTo edge");
        match &edge.target {
            EdgeEndpoint::Name { name, .. } => {
                assert_eq!(name, "münchen.example.com");
            }
            other => panic!("unexpected target endpoint: {other:?}"),
        }
    }
}
