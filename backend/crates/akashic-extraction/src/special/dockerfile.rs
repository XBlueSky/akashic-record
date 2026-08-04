//! Hand-parsed Dockerfile / Containerfile [`SpecialExtractor`] (DSL/infra track).
//!
//! Dockerfiles are line-oriented, so this extractor hand-parses rather than
//! using tree-sitter: `tree-sitter-dockerfile` pulls a conflicting old
//! tree-sitter (0.20) incompatible with this crate's 0.25, and line-oriented
//! parsing is robust and dependency-free.
//!
//! ## Chunk model — one chunk per STAGE
//! A Dockerfile is a flat sequence of instructions split into STAGES by `FROM`
//! instructions (multi-stage builds). Each `FROM <image>[:tag|@digest] [AS
//! <alias>]` opens a stage, emitted as one `chunk_type = "stage"` chunk:
//! - `name`/`fqn` = the `AS <alias>` name if present, else the base image
//!   reference (e.g. `debian:bookworm`). Stages are flat so the fqn is just the
//!   stage name (no dotted nesting). `parent_fqn` is always `None`.
//! - The chunk spans from the `FROM` line to just before the next `FROM` (or
//!   EOF). `signature` = the (logical, continuation-joined) FROM line;
//!   `content` = the stage's full raw text; `visibility = Public` (stages have
//!   no access control).
//! - If the file has NO `FROM` (e.g. an include fragment), a single
//!   `chunk_type = "stage"` chunk spanning the whole file is emitted with
//!   `name` = the file stem, so nothing is lost.
//!
//! ## Edge model
//! - `FROM <base>`: if `<base>` is a PRIOR stage alias (intra-file stage
//!   reference) → [`EdgeKind::Call`] to that stage; else (external registry
//!   image) → [`EdgeKind::Import`] with `module_specifier = Some(<image>)`.
//! - `COPY --from=<ref>` / `RUN --mount=...,from=<ref>`: inter-stage dependency
//!   from the current stage to `<ref>` — [`EdgeKind::Call`] if `<ref>` is a
//!   known prior stage alias, else [`EdgeKind::Import`] (an external image).
//!
//! All edges are statically known → [`Provenance::Static`]. Edge `source` is a
//! `Name` of the current stage (matching the Vue extractor's convention).
//!
//! ## Matching / limitation
//! `filename_matches` covers the exact names `Dockerfile` / `Containerfile`;
//! `extensions` covers `.dockerfile` (e.g. `app.dockerfile`). The trait only
//! does exact-filename + extension-suffix matching, so the common
//! `Dockerfile.prod` form (suffix-on-the-stem) is NOT matched — name such files
//! `prod.dockerfile` to route here.

use crate::types::*;

pub static DOCKERFILE: DockerfileExtractor = DockerfileExtractor;

pub struct DockerfileExtractor;

impl SpecialExtractor for DockerfileExtractor {
    fn name(&self) -> &'static str {
        "dockerfile"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &[".dockerfile"]
    }

    fn filename_matches(&self) -> &'static [&'static str] {
        // `registry::lookup_for_path` lowercases the basename before matching,
        // so these MUST be lowercase. Canonical filenames are `Dockerfile` /
        // `Containerfile` (case-insensitive in practice).
        &["dockerfile", "containerfile"]
    }

    fn parse(&self, source: &str, rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        Ok(parse_dockerfile(source, rel_path))
    }
}

/// A logical instruction: a (possibly continuation-joined) line with its
/// keyword, the byte/line span of its FIRST physical line, and the joined text.
struct Instruction {
    keyword: String,
    /// Continuation-joined remainder after the keyword (logical args).
    args: String,
    /// 1-based line number of the instruction's first physical line.
    line: usize,
    /// Byte offset of the instruction's first physical line in `source`.
    start_byte: usize,
}

fn parse_dockerfile(source: &str, rel_path: &str) -> ExtractionOutput {
    let mut out = ExtractionOutput::default();
    let instructions = lex_instructions(source);

    // Indices into `instructions` where each stage begins (a FROM).
    let from_idxs: Vec<usize> = instructions
        .iter()
        .enumerate()
        .filter(|(_, i)| i.keyword.eq_ignore_ascii_case("FROM"))
        .map(|(idx, _)| idx)
        .collect();

    if from_idxs.is_empty() {
        // No FROM: emit a single whole-file stage chunk so nothing is lost.
        let stem = file_stem(rel_path);
        let total_lines = source.matches('\n').count() + 1;
        out.chunks.push(RawChunk {
            chunk_type: "stage".into(),
            name: stem.clone(),
            fqn: Some(stem),
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
            visibility: Visibility::Public,
            metadata: ChunkMetadata::default(),
        });
        return out;
    }

    // Prior stage aliases known so far (lowercased) → used to decide Call vs
    // Import. Docker treats stage names case-insensitively.
    let mut known_stages: Vec<String> = Vec::new();

    for (stage_n, &from_idx) in from_idxs.iter().enumerate() {
        let from_instr = &instructions[from_idx];
        let (base, alias) = parse_from_args(&from_instr.args);

        // Stage name: alias if present, else the base image reference.
        let stage_name = alias.clone().unwrap_or_else(|| base.clone());

        // Stage byte span: from this FROM's first line to just before the next
        // FROM's first line (or EOF for the last stage).
        let stage_start_byte = from_instr.start_byte;
        let stage_end_byte = from_idxs
            .get(stage_n + 1)
            .map_or(source.len(), |&next| instructions[next].start_byte);
        let content = source[stage_start_byte..stage_end_byte]
            .trim_end_matches('\n')
            .to_string();
        let start_line = from_instr.line;
        let end_line = start_line + content.matches('\n').count();

        out.chunks.push(RawChunk {
            chunk_type: "stage".into(),
            name: stage_name.clone(),
            fqn: Some(stage_name.clone()),
            parent_fqn: None,
            start_line,
            end_line,
            start_byte: stage_start_byte,
            end_byte: stage_start_byte + content.len(),
            signature: Some(format!("FROM {}", from_instr.args.trim())),
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

        let source_endpoint = EdgeEndpoint::Name {
            name: stage_name.clone(),
            module_specifier: None,
        };

        // FROM edge: intra-file stage reference (Call) vs external image (Import).
        if known_stages.iter().any(|s| s.eq_ignore_ascii_case(&base)) {
            out.edges.push(RawEdge {
                source: source_endpoint.clone(),
                target: EdgeEndpoint::Name {
                    name: base.clone(),
                    module_specifier: None,
                },
                kind: EdgeKind::Call,
                provenance: Provenance::Static,
                line: Some(from_instr.line),
                metadata: instruction_meta("FROM"),
            });
        } else {
            out.edges.push(RawEdge {
                source: source_endpoint.clone(),
                target: EdgeEndpoint::Name {
                    name: base.clone(),
                    module_specifier: Some(base.clone()),
                },
                kind: EdgeKind::Import,
                provenance: Provenance::Static,
                line: Some(from_instr.line),
                metadata: instruction_meta("FROM"),
            });
        }

        // Scan the instructions belonging to this stage for COPY/RUN --from refs.
        let stage_instr_end = from_idxs
            .get(stage_n + 1)
            .copied()
            .unwrap_or(instructions.len());
        for instr in &instructions[from_idx + 1..stage_instr_end] {
            let from_ref = if instr.keyword.eq_ignore_ascii_case("COPY") {
                copy_from_ref(&instr.args)
            } else if instr.keyword.eq_ignore_ascii_case("RUN") {
                run_mount_from_ref(&instr.args)
            } else {
                None
            };
            let Some(reference) = from_ref else { continue };

            let (kind, target) = if known_stages
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&reference))
            {
                (
                    EdgeKind::Call,
                    EdgeEndpoint::Name {
                        name: reference.clone(),
                        module_specifier: None,
                    },
                )
            } else {
                (
                    EdgeKind::Import,
                    EdgeEndpoint::Name {
                        name: reference.clone(),
                        module_specifier: Some(reference.clone()),
                    },
                )
            };
            let instr_label = if instr.keyword.eq_ignore_ascii_case("COPY") {
                "COPY --from"
            } else {
                "RUN --mount=from"
            };
            out.edges.push(RawEdge {
                source: source_endpoint.clone(),
                target,
                kind,
                provenance: Provenance::Static,
                line: Some(instr.line),
                metadata: instruction_meta(instr_label),
            });
        }

        // Register this stage's alias (if any) for later intra-file references.
        if let Some(a) = alias {
            known_stages.push(a);
        }
    }

    out
}

/// Lex the source into logical instructions, joining line continuations (`\` at
/// end of line) and skipping blank lines, comments (`#...`), and parser
/// directives (`# syntax=`). Instruction keywords are case-insensitive.
fn lex_instructions(source: &str) -> Vec<Instruction> {
    let mut instructions = Vec::new();
    // Running byte offset of each physical line.
    let mut byte_off = 0usize;

    // We iterate physical lines, but must join continuations into one logical
    // instruction. Collect (line_no, byte_off, text) for physical lines.
    struct PhysLine<'a> {
        no: usize,
        byte: usize,
        text: &'a str,
    }
    let mut phys: Vec<PhysLine> = Vec::new();
    for (idx, raw) in source.split_inclusive('\n').enumerate() {
        let text = raw.strip_suffix('\n').unwrap_or(raw);
        phys.push(PhysLine {
            no: idx + 1, // 1-based line number
            byte: byte_off,
            text,
        });
        byte_off += raw.len();
    }

    let mut i = 0;
    while i < phys.len() {
        let line = &phys[i];
        let trimmed = line.text.trim();
        // Skip blanks and comments / parser directives.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }

        // Start of a logical instruction. Join continuation lines.
        let first_no = line.no;
        let first_byte = line.byte;
        let mut joined = strip_continuation(line.text).to_string();
        while ends_with_continuation(phys[i].text) && i + 1 < phys.len() {
            i += 1;
            joined.push(' ');
            joined.push_str(strip_continuation(phys[i].text).trim());
        }
        i += 1;

        // Split keyword + args. Keyword is the first whitespace-delimited token.
        let joined = joined.trim().to_string();
        let mut parts = joined.splitn(2, char::is_whitespace);
        let keyword = parts.next().unwrap_or("").to_string();
        let args = parts.next().unwrap_or("").trim().to_string();
        if keyword.is_empty() {
            continue;
        }
        instructions.push(Instruction {
            keyword,
            args,
            line: first_no,
            start_byte: first_byte,
        });
    }

    instructions
}

fn ends_with_continuation(text: &str) -> bool {
    // A line continues if, after stripping trailing whitespace, it ends with `\`
    // (and that backslash is not within a trailing comment). Keep it simple:
    // strip a trailing inline comment is uncommon in Dockerfiles; treat the
    // physical line's trailing `\` as continuation.
    text.trim_end().ends_with('\\')
}

fn strip_continuation(text: &str) -> &str {
    let t = text.trim_end();
    t.strip_suffix('\\').unwrap_or(t)
}

/// Parse the args of a FROM instruction → (base_image_ref, optional_alias).
/// Handles `FROM [--platform=...] <image> [AS <alias>]`.
fn parse_from_args(args: &str) -> (String, Option<String>) {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut base = String::new();
    let mut alias = None;
    let mut idx = 0;
    // Skip leading flags like --platform=linux/amd64.
    while idx < tokens.len() && tokens[idx].starts_with("--") {
        idx += 1;
    }
    if idx < tokens.len() {
        base = tokens[idx].to_string();
        idx += 1;
    }
    // Look for `AS <alias>`.
    if idx + 1 < tokens.len() && tokens[idx].eq_ignore_ascii_case("AS") {
        alias = Some(tokens[idx + 1].to_string());
    }
    (base, alias)
}

/// Extract the `--from=<ref>` value from a COPY instruction's args, if present.
fn copy_from_ref(args: &str) -> Option<String> {
    for tok in args.split_whitespace() {
        if let Some(rest) = tok.strip_prefix("--from=")
            && !rest.is_empty()
        {
            return Some(rest.to_string());
        }
    }
    None
}

/// Extract the `from=<ref>` value from a RUN instruction's `--mount=...` flag(s),
/// if present (e.g. `RUN --mount=type=cache,from=builder,...`).
fn run_mount_from_ref(args: &str) -> Option<String> {
    for tok in args.split_whitespace() {
        if let Some(rest) = tok.strip_prefix("--mount=") {
            // The mount spec is comma-separated key=value pairs.
            for kv in rest.split(',') {
                if let Some(v) = kv.strip_prefix("from=")
                    && !v.is_empty()
                {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn instruction_meta(instruction: &str) -> EdgeMetadata {
    let mut meta = EdgeMetadata::default();
    meta.fields.insert(
        "instruction".to_string(),
        MetadataValue::String(instruction.to_string()),
    );
    meta
}

/// The file stem (basename without extension) for the no-FROM fallback chunk.
fn file_stem(rel_path: &str) -> String {
    let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
    match base.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
        _ => base.to_string(),
    }
}
