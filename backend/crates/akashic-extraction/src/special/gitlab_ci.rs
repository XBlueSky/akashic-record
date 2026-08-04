//! Hand-parsed GitLab CI (`.gitlab-ci.yml`) [`SpecialExtractor`] (DSL/infra
//! track).
//!
//! `.gitlab-ci.yml` is YAML, but a full YAML parser is deliberately avoided:
//! every YAML tree-sitter / crate grammar pulls a runtime that conflicts with
//! this crate's tree-sitter 0.25, and gitlab-ci's structure is regular enough
//! (a flat top-level mapping of jobs + global keywords) to hand-parse. No new
//! Cargo dependency.
//!
//! ## Structure exploited
//! A top-level key is a line matching `^([A-Za-z_.][\w.-]*)\s*:` at COLUMN 0
//! (no leading whitespace). Its block runs from that line through just before
//! the next column-0 key (or EOF). Top-level keys are partitioned into:
//! - GLOBAL keywords (NOT jobs): `stages`, `variables`, `default`, `include`,
//!   `workflow`, `image`, `services`, `before_script`, `after_script`, `cache`,
//!   `types` (deprecated). These emit no job chunk; `include` emits Import
//!   edges (config composition).
//! - everything else = a JOB. Keys beginning with `.` (e.g. `.build-template`)
//!   are HIDDEN/template jobs — still emitted as job chunks (they are referenced
//!   via `extends`/anchors), tagged `metadata.fields["template"]="true"`.
//!
//! ## Chunk model — one chunk per JOB (incl. hidden `.` templates)
//! `chunk_type = "job"`; `name` = `fqn` = the job key (jobs are flat → no dotted
//! nesting, `parent_fqn = None`). `start_line`/`start_byte` at the key line;
//! `end_line`/`end_byte` to just before the next top-level key (or EOF).
//! `content` = the job block; `signature` = the key line; `visibility = Public`.
//! GLOBAL keyword blocks are NOT job chunks.
//!
//! ## Edge model (all [`Provenance::Static`]; `source` = the job's `Name`)
//! Within a job block the keys `needs`, `extends`, `dependencies` are parsed in
//! their YAML forms — flow list (`needs: [a, b]`), block list (`- a`), object
//! list (`- job: a`), and scalar (`extends: .base`):
//! - `needs` → [`EdgeKind::Call`], `metadata.fields["relation"]="needs"` (the CI
//!   DAG dependency). `- project: x` long-form cross-project needs are recorded
//!   but skipped as edges (external, unresolvable in-file).
//! - `extends` → [`EdgeKind::Call`], `relation="extends"` (template base).
//! - `dependencies` → [`EdgeKind::Call`], `relation="dependencies"` (artifact
//!   dependency).
//!
//! Top-level `include:` emits one [`EdgeKind::Import`] per included ref (the
//! local path, remote URL, template name, or project ref) with
//! `module_specifier = Some(ref)` and `source` = the file stem. Both the scalar
//! form (`include: 'path'`) and the block-list form (whose items carry a
//! `local:` / `template:` / `remote:` / `project:` sub-key) are handled.
//!
//! ## Documented YAML limitations (the regular 90% is handled; the rest noted)
//! - YAML anchors (`&x`) / aliases (`*x`) and merge keys (`<<:`) are NOT
//!   resolved — the alias name is treated as a literal token.
//! - Multi-line block scalars (`|` / `>`) are not interpreted; their indented
//!   bodies are simply part of the job block text (they do not contain
//!   column-0 keys, so chunk boundaries are unaffected).
//! - Deeply-nested flow collections beyond the common single-level list are not
//!   descended into.
//! - `trigger:` (child-pipeline) is intentionally NOT turned into an edge here;
//!   it is left in the job content (documented out-of-scope).
//!
//! ## Matching / scope
//! `filename_matches` covers ONLY the canonical names `.gitlab-ci.yml` /
//! `.gitlab-ci.yaml` (`registry::lookup_for_path` lowercases the basename, and
//! these are already lowercase). `extensions` is EMPTY on purpose — claiming
//! `.yml`/`.yaml` would hijack every YAML file in a repo. A CI file renamed away
//! from the canonical name will therefore NOT route here (accepted limitation).

use crate::types::*;

pub static GITLAB_CI: GitlabCiExtractor = GitlabCiExtractor;

pub struct GitlabCiExtractor;

impl SpecialExtractor for GitlabCiExtractor {
    fn name(&self) -> &'static str {
        "gitlab-ci"
    }

    fn extensions(&self) -> &'static [&'static str] {
        // Intentionally empty: matching by the canonical filename only, so we
        // do NOT hijack arbitrary `.yml` / `.yaml` files.
        &[]
    }

    fn filename_matches(&self) -> &'static [&'static str] {
        // `registry::lookup_for_path` lowercases the basename before matching,
        // so these MUST be lowercase. These are the canonical CI filenames.
        &[".gitlab-ci.yml", ".gitlab-ci.yaml"]
    }

    fn parse(&self, source: &str, rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        Ok(parse_gitlab_ci(source, rel_path))
    }
}

/// GLOBAL top-level keywords that are NOT jobs.
const GLOBAL_KEYWORDS: &[&str] = &[
    "stages",
    "variables",
    "default",
    "include",
    "workflow",
    "image",
    "services",
    "before_script",
    "after_script",
    "cache",
    "types",
];

/// A physical line with its 1-based number and byte offset in `source`.
struct Line<'a> {
    no: usize,
    byte: usize,
    text: &'a str,
}

/// A top-level (column-0) key and the half-open line-index range of its block.
struct TopKey {
    key: String,
    /// Index into `lines` of the key line itself.
    line_idx: usize,
    /// Half-open `[start, end)` line-index range of the block (key line .. next
    /// top-level key / EOF).
    block_start: usize,
    block_end: usize,
}

fn parse_gitlab_ci(source: &str, rel_path: &str) -> ExtractionOutput {
    let mut out = ExtractionOutput::default();
    let lines = split_lines(source);
    let top_keys = find_top_keys(&lines);

    for tk in &top_keys {
        let key = tk.key.as_str();
        // The block's byte span: from the key line's byte to just before the
        // next top-level key's byte (or EOF), with the trailing newline trimmed.
        let block_start_byte = lines[tk.block_start].byte;
        let block_end_byte = lines.get(tk.block_end).map_or(source.len(), |l| l.byte);
        let content = source[block_start_byte..block_end_byte]
            .trim_end_matches('\n')
            .to_string();
        let start_line = lines[tk.block_start].no;
        let end_line = start_line + content.matches('\n').count();
        let signature = lines[tk.line_idx].text.trim_end().to_string();

        if is_global_keyword(key) {
            // GLOBAL keyword: no job chunk. `include` emits Import edges.
            if key == "include" {
                emit_include_edges(&lines, tk, rel_path, &mut out);
            }
            continue;
        }

        // JOB chunk (incl. hidden `.` templates).
        let is_template = key.starts_with('.');
        let mut metadata = ChunkMetadata::default();
        if is_template {
            metadata.fields.insert(
                "template".to_string(),
                MetadataValue::String("true".to_string()),
            );
        }

        out.chunks.push(RawChunk {
            chunk_type: "job".into(),
            name: key.to_string(),
            fqn: Some(key.to_string()),
            parent_fqn: None,
            start_line,
            end_line,
            start_byte: block_start_byte,
            end_byte: block_start_byte + content.len(),
            signature: Some(signature),
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

        // Edges from this job's needs / extends / dependencies keys.
        let job_source = EdgeEndpoint::Name {
            name: key.to_string(),
            module_specifier: None,
        };
        emit_dag_edges(&lines, tk, &job_source, &mut out);
    }

    out
}

/// Split the source into physical lines with byte offsets, keeping blank lines.
fn split_lines(source: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let mut byte = 0usize;
    for (idx, raw) in source.split_inclusive('\n').enumerate() {
        let text = raw.strip_suffix('\n').unwrap_or(raw);
        lines.push(Line {
            no: idx + 1,
            byte,
            text,
        });
        byte += raw.len();
    }
    lines
}

/// Find every top-level (column-0) key and compute its block range.
fn find_top_keys(lines: &[Line<'_>]) -> Vec<TopKey> {
    // First, collect the line indices that begin a top-level key.
    let mut key_lines: Vec<(usize, String)> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if let Some(key) = top_level_key(line.text) {
            key_lines.push((idx, key));
        }
    }

    let mut keys = Vec::with_capacity(key_lines.len());
    for (n, (line_idx, key)) in key_lines.iter().enumerate() {
        let block_end = key_lines
            .get(n + 1)
            .map_or(lines.len(), |(next_idx, _)| *next_idx);
        keys.push(TopKey {
            key: key.clone(),
            line_idx: *line_idx,
            block_start: *line_idx,
            block_end,
        });
    }
    keys
}

/// If `text` is a column-0 mapping key (`^([A-Za-z_.][\w.-]*)\s*:`), return the
/// key; else `None`. Column-0 means NO leading whitespace. Comment lines,
/// list-item lines (`- ...`), and document markers (`---`) never match.
fn top_level_key(text: &str) -> Option<String> {
    // Column 0: a leading space/tab disqualifies it as a top-level key.
    if text.starts_with([' ', '\t']) || text.is_empty() {
        return None;
    }
    let trimmed = text.trim_start();
    if trimmed.starts_with('#') {
        return None;
    }
    let first = trimmed.chars().next()?;
    if !(first.is_ascii_alphabetic() || first == '_' || first == '.') {
        return None;
    }
    // Read the key up to the `:`; the key chars are `[\w.-]`.
    let mut key = String::new();
    let mut rest = trimmed.chars().peekable();
    while let Some(&c) = rest.peek() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
            key.push(c);
            rest.next();
        } else {
            break;
        }
    }
    // Allow optional whitespace, then a mandatory `:`.
    let mut saw_colon = false;
    for c in rest {
        if c == ':' {
            saw_colon = true;
            break;
        } else if c == ' ' || c == '\t' {
            continue;
        }
        break;
    }
    if saw_colon && !key.is_empty() {
        Some(key)
    } else {
        None
    }
}

fn is_global_keyword(key: &str) -> bool {
    GLOBAL_KEYWORDS.contains(&key)
}

/// Strip a trailing `#` comment that is NOT inside quotes, then trim. Used on
/// value text (the part after a `:` or a `- `).
fn strip_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            // A `#` only starts a comment if preceded by whitespace or at the
            // start (YAML rule). Keep it simple: treat any unquoted `#` at the
            // start or after whitespace as a comment.
            b'#' if !in_single
                && !in_double
                && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') =>
            {
                return &s[..i];
            }
            _ => {}
        }
    }
    s
}

/// Strip surrounding single/double quotes from a value token, if matched.
fn unquote(s: &str) -> String {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Parse a flow list `[a, "b", c]` body (the text BETWEEN the brackets) into
/// cleaned items.
fn parse_flow_list(inner: &str) -> Vec<String> {
    inner
        .split(',')
        .map(|t| unquote(strip_comment(t).trim()))
        .filter(|t| !t.is_empty())
        .collect()
}

/// The indentation (count of leading spaces; tabs count as one) of a line.
fn indent_of(text: &str) -> usize {
    text.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// Emit needs / extends / dependencies → Call edges for one job block.
fn emit_dag_edges(
    lines: &[Line<'_>],
    tk: &TopKey,
    job_source: &EdgeEndpoint,
    out: &mut ExtractionOutput,
) {
    // Body lines are everything after the key line, up to block_end.
    let body = &lines[tk.line_idx + 1..tk.block_end];

    for (rel_keyword, relation) in [
        ("needs", "needs"),
        ("extends", "extends"),
        ("dependencies", "dependencies"),
    ] {
        for target in collect_key_values(rel_keyword, body) {
            out.edges.push(RawEdge {
                source: job_source.clone(),
                target: EdgeEndpoint::Name {
                    name: target.0.clone(),
                    module_specifier: None,
                },
                kind: EdgeKind::Call,
                provenance: Provenance::Static,
                line: Some(target.1),
                metadata: relation_meta(relation),
            });
        }
    }
}

/// The indentation of a job's DIRECT child keys: the indent of the first
/// non-blank, non-comment body line. In valid YAML every direct child of the
/// job mapping shares this indent, so it is the level at which `needs:` /
/// `extends:` / `dependencies:` are job-level keys (vs. nested deeper under
/// another mapping such as `variables:`). Returns `None` for an empty body.
fn direct_child_indent(body: &[Line<'_>]) -> Option<usize> {
    body.iter()
        .map(|l| l.text)
        .find(|t| {
            let trimmed = t.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .map(indent_of)
}

/// Collect the values of a job-level key (`needs`/`extends`/`dependencies`)
/// across all YAML forms. Returns `(value, line_no)` pairs.
fn collect_key_values(keyword: &str, body: &[Line<'_>]) -> Vec<(String, usize)> {
    let mut values = Vec::new();
    let prefix = format!("{keyword}:");
    // Only the job's DIRECT children are job-level keys. A `needs:` /
    // `extends:` / `dependencies:` line nested deeper (e.g. a variable named
    // `extends` under `variables:`, or a sub-mapping key) must NOT produce a
    // DAG edge, so we gate every match on this indent below.
    let job_child_indent = direct_child_indent(body);

    let mut i = 0;
    while i < body.len() {
        let line = &body[i];
        let trimmed = line.text.trim_start();
        if !trimmed.starts_with(&prefix) {
            i += 1;
            continue;
        }
        // Found `keyword:`. Determine its indentation; the block list (if any)
        // is the run of more-indented `-` lines that follow.
        let key_indent = indent_of(line.text);
        // Skip a same-named key that is NOT at the job's direct-child indent:
        // it belongs to a deeper, nested mapping and is not a real DAG edge.
        if Some(key_indent) != job_child_indent {
            i += 1;
            continue;
        }
        let after = strip_comment(&trimmed[prefix.len()..]).trim();

        if !after.is_empty() {
            // Inline value: either a flow list `[...]` or a scalar.
            if let Some(inner) = after.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                for v in parse_flow_list(inner) {
                    values.push((v, line.no));
                }
            } else if after.starts_with('[') {
                // Flow list possibly spanning (rare); take what's between
                // the first `[` and a `]` if present, else the remainder.
                let inner = after
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_string();
                for v in parse_flow_list(&inner) {
                    values.push((v, line.no));
                }
            } else {
                // Scalar (e.g. `extends: .base`).
                let v = unquote(after);
                if !v.is_empty() {
                    values.push((v, line.no));
                }
            }
            i += 1;
            continue;
        }

        // Block form: scan following deeper-indented `-` list items.
        let mut j = i + 1;
        while j < body.len() {
            let item = &body[j];
            if item.text.trim().is_empty() {
                j += 1;
                continue;
            }
            let item_indent = indent_of(item.text);
            if item_indent <= key_indent {
                break; // dedented back out of the block
            }
            let item_trimmed = item.text.trim_start();
            if let Some(rest) = item_trimmed.strip_prefix('-') {
                let rest = strip_comment(rest).trim();
                if let Some(v) = parse_list_item(rest) {
                    values.push((v, item.no));
                }
            }
            // Object-form sub-keys (e.g. the `project:`/`ref:` lines under a
            // `- job:` entry) sit deeper and are not `-`-prefixed; we only act
            // on the `job:` extracted from the `-` line above, so skip them.
            j += 1;
        }
        i = j;
    }

    values
}

/// Interpret one block-list item value (the text after `- `), handling the
/// object long-form (`job: build`, `project: x`) and the plain scalar.
/// Returns `None` for cross-project `project:` needs (recorded as external,
/// skipped as an edge) and for empty items.
fn parse_list_item(rest: &str) -> Option<String> {
    if rest.is_empty() {
        return None;
    }
    // Object long form `key: value` (e.g. `job: build`, `project: grp/proj`).
    if let Some(colon) = rest.find(':') {
        let sub_key = rest[..colon].trim();
        let sub_val = unquote(rest[colon + 1..].trim());
        match sub_key {
            "job" => {
                if sub_val.is_empty() {
                    return None;
                }
                return Some(sub_val);
            }
            // Cross-project / pipeline-id long forms: external, skip as edge.
            "project" | "pipeline" | "ref" | "artifacts" | "optional" => return None,
            _ => {
                // Unknown sub-key: fall through to treat the whole as a scalar
                // only if it looks like a bare name (no colon handling needed).
                return None;
            }
        }
    }
    // Plain scalar list item (`- build` / `- "build"`).
    Some(unquote(rest))
}

/// Emit Import edges for a top-level `include:` block.
fn emit_include_edges(lines: &[Line<'_>], tk: &TopKey, rel_path: &str, out: &mut ExtractionOutput) {
    let source_endpoint = EdgeEndpoint::Name {
        name: file_stem(rel_path),
        module_specifier: None,
    };
    let key_line = &lines[tk.line_idx];
    let after = strip_comment(key_line.text.trim_start().trim_start_matches("include:")).trim();

    let mut push_import = |reference: String, line: usize| {
        if reference.is_empty() {
            return;
        }
        out.edges.push(RawEdge {
            source: source_endpoint.clone(),
            target: EdgeEndpoint::Name {
                name: reference.clone(),
                module_specifier: Some(reference),
            },
            kind: EdgeKind::Import,
            provenance: Provenance::Static,
            line: Some(line),
            metadata: relation_meta("include"),
        });
    };

    // Scalar form: `include: 'some/path.yml'` or `include: [a, b]`.
    if !after.is_empty() {
        if let Some(inner) = after.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            for v in parse_flow_list(inner) {
                push_import(v, key_line.no);
            }
        } else {
            push_import(unquote(after), key_line.no);
        }
        return;
    }

    // Block form: iterate the indented body lines, picking out the meaningful
    // ref from each `local:` / `remote:` / `template:` / `project:` / `file:`
    // sub-key (whether on a `- local: x` line or under a `-` item).
    for line in &lines[tk.line_idx + 1..tk.block_end] {
        if line.text.trim().is_empty() {
            continue;
        }
        // Stop if a column-0 line sneaks in (defensive; block_end should guard).
        if indent_of(line.text) == 0 {
            break;
        }
        let t = strip_comment(line.text.trim_start().trim_start_matches('-').trim_start()).trim();
        if let Some(colon) = t.find(':') {
            let sub_key = t[..colon].trim();
            let sub_val = unquote(t[colon + 1..].trim());
            match sub_key {
                "local" | "remote" | "template" | "project" | "file" | "component" => {
                    push_import(sub_val, line.no);
                }
                _ => {}
            }
        } else {
            // A bare scalar list item under include (`- some/path.yml`).
            push_import(unquote(t), line.no);
        }
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

/// The file stem (basename without extension), used as the file-level edge
/// source for top-level `include`. For `.gitlab-ci.yml` this yields
/// `.gitlab-ci`.
fn file_stem(rel_path: &str) -> String {
    let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
    match base.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
        _ => base.to_string(),
    }
}
