//! Hand-parsed Drupal `*.routing.yml` [`SpecialExtractor`] (DSL/infra track).
//!
//! Drupal modules declare routes in a file named `<module>.routing.yml`. The
//! file is YAML, but a full YAML parser is deliberately avoided (same policy as
//! gitlab_ci.rs and nginx.rs — no runtime dependency conflict, and the
//! structure is regular enough to hand-parse).
//!
//! ## Structure exploited
//! The file is a flat top-level mapping:
//! ```yaml
//! mymodule.content:
//!   path: '/mycontent'
//!   defaults:
//!     _controller: '\Drupal\mymodule\Controller\MyController::content'
//!   requirements:
//!     _permission: 'access content'
//! ```
//! - A **route** starts at a line with NO leading whitespace, ending in `:`,
//!   that is not a comment.  The key is the route name.
//! - Within the indented block the extractor captures:
//!   - `path: <value>` — the URL path (quotes stripped).
//!   - `_controller: <value>` — `Class::method` handler.
//!   - `_form: <value>` — form class (no `::method`; use class basename).
//!   - `_entity_form: <value>` — entity-form class (same treatment as `_form`).
//!
//! ## Chunk model — one chunk per route
//! `chunk_type = "route"`;
//! `name` = `fqn` = the URL path (e.g. `/mycontent`);
//! `content` = `"<route_name>: <path> -> <handler>"` (human summary);
//! `start_line`/`start_byte` at the route's key line;
//! `end_line`/`end_byte` to just before the next top-level key (or EOF);
//! `visibility = Public`.
//!
//! ## Edge model
//! For each route where a handler was found: one [`EdgeKind::RoutesTo`] edge
//! where:
//! - `source` = `ByteRange { start, end }` inside the route chunk's span;
//! - `target` = `Name { name: <handler_short>, module_specifier: None }` where
//!   `<handler_short>` is the trailing `::method` segment for `_controller`
//!   (e.g. `content`), or the class basename for `_form`/`_entity_form`
//!   (e.g. `SettingsForm`);
//! - `metadata.fields["http_method"] = "ANY"`.
//!
//! ## Matching / scope
//! `extensions` and `filename_matches` are EMPTY; dispatch is via
//! `path_suffix_matches(&[".routing.yml"])`. This avoids claiming `.yml` (which
//! would hijack every YAML file) and is more flexible than exact filename match
//! (every Drupal module has a different prefix).

use crate::types::*;

pub static DRUPAL_ROUTING: DrupalRouting = DrupalRouting;

pub struct DrupalRouting;

impl SpecialExtractor for DrupalRouting {
    fn name(&self) -> &'static str {
        "drupal_routing"
    }

    fn extensions(&self) -> &'static [&'static str] {
        // Intentionally empty: `.routing.yml` is a compound suffix, NOT a
        // single extension. Claiming `.yml` would hijack all YAML files.
        &[]
    }

    fn filename_matches(&self) -> &'static [&'static str] {
        // No single canonical filename; every module has a unique prefix.
        &[]
    }

    fn path_suffix_matches(&self) -> &'static [&'static str] {
        &[".routing.yml"]
    }

    fn parse(&self, source: &str, rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        Ok(parse_drupal_routing(source, rel_path))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal parse logic
// ─────────────────────────────────────────────────────────────────────────────

/// A physical source line with its 1-based line number and byte offset.
struct Line<'a> {
    no: usize,
    byte: usize,
    text: &'a str,
}

/// A top-level route key and the half-open line-index range of its block.
struct RouteBlock {
    route_name: String,
    /// Index into `lines` of the key line itself.
    key_line_idx: usize,
    /// Half-open `[key_line_idx, block_end)` range in `lines`.
    block_end_idx: usize,
}

fn parse_drupal_routing(source: &str, _rel_path: &str) -> ExtractionOutput {
    let mut out = ExtractionOutput::default();
    let lines = split_lines(source);
    let routes = find_routes(&lines);

    for route in &routes {
        let block_start_byte = lines[route.key_line_idx].byte;
        let block_end_byte = lines
            .get(route.block_end_idx)
            .map_or(source.len(), |l| l.byte);

        // Trim trailing newline to stay consistent with gitlab_ci.rs.
        let content_raw = &source[block_start_byte..block_end_byte];
        let content_trimmed = content_raw.trim_end_matches('\n');
        let block_end_byte_trimmed = block_start_byte + content_trimmed.len();

        let start_line = lines[route.key_line_idx].no;
        let end_line = start_line + content_trimmed.matches('\n').count();

        // Extract `path:`, `_controller:`, `_form:`, `_entity_form:` from the
        // block body (lines after the route key line).
        let body = &lines[route.key_line_idx + 1..route.block_end_idx];
        let path = extract_path(body);
        let handler_raw = extract_handler(body);

        // A route without a `path:` key is malformed — emit nothing.
        let path = match path {
            Some(p) => p,
            None => continue,
        };

        let handler_short = handler_raw
            .as_deref()
            .map(extract_handler_short)
            .unwrap_or_default();
        let summary = if handler_short.is_empty() {
            format!("{}: {}", route.route_name, path)
        } else {
            format!("{}: {} -> {}", route.route_name, path, handler_short)
        };

        out.chunks.push(RawChunk {
            chunk_type: "route".into(),
            name: path.clone(),
            fqn: Some(path.clone()),
            parent_fqn: None,
            start_line,
            end_line,
            start_byte: block_start_byte,
            end_byte: block_end_byte_trimmed,
            signature: Some(format!("{}: {}", route.route_name, path)),
            content: summary,
            doc: None,
            receiver: None,
            is_async: false,
            is_static: false,
            is_const: false,
            is_exported: false,
            visibility: Visibility::Public,
            metadata: ChunkMetadata::default(),
        });

        // Emit a RoutesTo edge when a handler was resolved.
        if !handler_short.is_empty() {
            let mut edge_meta = EdgeMetadata::default();
            edge_meta.fields.insert(
                "http_method".to_string(),
                MetadataValue::String("ANY".to_string()),
            );
            out.edges.push(RawEdge {
                // Source: a ByteRange within the route chunk's span. We use the
                // first byte of the block (the route-name key line) as both
                // start and end — it is guaranteed to be inside [start_byte,
                // end_byte) and it's a stable reference point.
                source: EdgeEndpoint::ByteRange {
                    start: block_start_byte,
                    end: block_start_byte + lines[route.key_line_idx].text.len(),
                },
                target: EdgeEndpoint::Name {
                    name: handler_short,
                    module_specifier: None,
                },
                kind: EdgeKind::RoutesTo,
                provenance: Provenance::Static,
                line: Some(start_line),
                metadata: edge_meta,
            });
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Split `source` into physical lines with their byte offsets.
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

/// Find every top-level route key (NO leading whitespace, ends in `:`, not a
/// comment) and compute each block's line range.
fn find_routes(lines: &[Line<'_>]) -> Vec<RouteBlock> {
    let mut key_indices: Vec<(usize, String)> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if let Some(name) = top_level_route_key(line.text) {
            key_indices.push((idx, name));
        }
    }
    let mut routes = Vec::with_capacity(key_indices.len());
    for (n, (key_idx, route_name)) in key_indices.iter().enumerate() {
        let block_end_idx = key_indices
            .get(n + 1)
            .map_or(lines.len(), |(next_idx, _)| *next_idx);
        routes.push(RouteBlock {
            route_name: route_name.clone(),
            key_line_idx: *key_idx,
            block_end_idx,
        });
    }
    routes
}

/// If `text` is a top-level YAML mapping key (no leading whitespace, ends in
/// `:`, not a comment line), return the key name; else `None`.
fn top_level_route_key(text: &str) -> Option<String> {
    // Must start at column 0 — any leading whitespace disqualifies.
    if text.starts_with([' ', '\t']) || text.is_empty() {
        return None;
    }
    let trimmed = text.trim_start();
    if trimmed.starts_with('#') {
        return None;
    }
    // The key is everything before the first `:` (YAML mapping key).
    // Drupal route names look like `mymodule.content` — alphanumeric, `.`, `_`, `-`.
    let colon_pos = trimmed.find(':')?;
    let key = trimmed[..colon_pos].trim();
    if key.is_empty() {
        return None;
    }
    Some(key.to_string())
}

/// Strip surrounding single/double quotes from a YAML scalar value.
fn unquote(s: &str) -> String {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'\'' || b[0] == b'"') && b[b.len() - 1] == b[0] {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Strip a trailing `# comment` (YAML comment — only after whitespace).
fn strip_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
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

/// Extract the URL path from a route's body lines.
/// Handles `path: '/foo'`, `path: "/foo"`, and `path: /foo`.
fn extract_path(body: &[Line<'_>]) -> Option<String> {
    for line in body {
        let t = line.text.trim_start();
        if let Some(rest) = t.strip_prefix("path:") {
            let val = unquote(strip_comment(rest).trim());
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Possible handler key variants in `defaults:`.
const HANDLER_KEYS: &[&str] = &["_controller:", "_form:", "_entity_form:"];

/// Extract the raw handler value from a route's body lines, searching for
/// `_controller:`, `_form:`, or `_entity_form:` anywhere in the indented block.
fn extract_handler(body: &[Line<'_>]) -> Option<String> {
    for line in body {
        let t = line.text.trim_start();
        for &key in HANDLER_KEYS {
            if let Some(rest) = t.strip_prefix(key) {
                let val = unquote(strip_comment(rest).trim());
                if !val.is_empty() {
                    return Some(val);
                }
            }
        }
    }
    None
}

/// Derive the short handler name from a raw handler value.
///
/// - `_controller` values: `\Drupal\foo\Controller\MyController::content`
///   → trailing `::method` segment → `content`.
/// - `_form` / `_entity_form` values: `\Drupal\foo\Form\SettingsForm`
///   → class basename (last `\` segment) → `SettingsForm`.
/// - Fallback: return the full value as-is if no separator found.
fn extract_handler_short(raw: &str) -> String {
    // Prefer `::method` (controller case).
    if let Some(pos) = raw.rfind("::") {
        let method = raw[pos + 2..].trim();
        if !method.is_empty() {
            return method.to_string();
        }
    }
    // Form/entity-form: last `\` segment.
    if let Some(pos) = raw.rfind('\\') {
        let class = raw[pos + 1..].trim();
        if !class.is_empty() {
            return class.to_string();
        }
    }
    // Bare name or unrecognised format — return as-is.
    raw.trim().to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
mymodule.content:
  path: '/mycontent'
  defaults:
    _controller: '\\Drupal\\mymodule\\Controller\\MyController::content'
  requirements:
    _permission: 'access content'

mymodule.settings:
  path: '/admin/config/mycontent'
  defaults:
    _form: '\\Drupal\\mymodule\\Form\\SettingsForm'
  requirements:
    _permission: 'administer site configuration'
";

    #[test]
    fn drupal_routing_two_route_chunks() {
        let out = parse_drupal_routing(SAMPLE, "mymodule/mymodule.routing.yml");

        assert_eq!(out.chunks.len(), 2, "expected 2 route chunks");

        let c0 = &out.chunks[0];
        assert_eq!(c0.chunk_type, "route");
        assert_eq!(c0.name, "/mycontent");
        assert_eq!(c0.fqn, Some("/mycontent".into()));
        assert_eq!(c0.visibility, Visibility::Public);

        let c1 = &out.chunks[1];
        assert_eq!(c1.chunk_type, "route");
        assert_eq!(c1.name, "/admin/config/mycontent");
        assert_eq!(c1.fqn, Some("/admin/config/mycontent".into()));
    }

    #[test]
    fn drupal_routing_routes_to_controller_method() {
        let out = parse_drupal_routing(SAMPLE, "mymodule/mymodule.routing.yml");

        // Should have at least one RoutesTo edge (for the _controller route).
        let routes_to: Vec<_> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .collect();
        assert!(!routes_to.is_empty(), "expected at least one RoutesTo edge");

        // First edge targets the controller method name `content`.
        let first = &routes_to[0];
        match &first.target {
            EdgeEndpoint::Name { name, .. } => {
                assert_eq!(name, "content", "controller method should be 'content'");
            }
            other => panic!("expected Name endpoint, got {other:?}"),
        }

        // http_method metadata must be "ANY".
        let http_method = first.metadata.fields.get("http_method");
        assert_eq!(
            http_method,
            Some(&MetadataValue::String("ANY".into())),
            "http_method metadata must be 'ANY'"
        );
    }

    #[test]
    fn drupal_routing_routes_to_form_class() {
        let out = parse_drupal_routing(SAMPLE, "mymodule/mymodule.routing.yml");

        let routes_to: Vec<_> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .collect();

        // Second edge (for the _form route) targets class basename `SettingsForm`.
        assert_eq!(routes_to.len(), 2, "expected 2 RoutesTo edges");
        match &routes_to[1].target {
            EdgeEndpoint::Name { name, .. } => {
                assert_eq!(name, "SettingsForm");
            }
            other => panic!("expected Name endpoint, got {other:?}"),
        }
    }

    #[test]
    fn drupal_routing_source_byterange_inside_chunk() {
        let out = parse_drupal_routing(SAMPLE, "mymodule/mymodule.routing.yml");

        // Each RoutesTo edge's source ByteRange must fall within the span of
        // EXACTLY ONE route chunk. Match by CONTAINMENT (not positional zip):
        // a positional edge[i]↔chunk[i] alignment only holds while every route
        // has a handler (1:1). If a route without a handler is ever added the
        // arrays desync, but a containment match stays correct.
        let route_edges: Vec<_> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .collect();
        assert!(
            !route_edges.is_empty(),
            "expected at least one RoutesTo edge in the fixture"
        );
        for edge in route_edges {
            let (start, end) = match &edge.source {
                EdgeEndpoint::ByteRange { start, end } => (*start, *end),
                other => panic!("expected ByteRange source, got {other:?}"),
            };
            let containing = out
                .chunks
                .iter()
                .filter(|c| start >= c.start_byte && end <= c.end_byte)
                .count();
            assert_eq!(
                containing, 1,
                "RoutesTo edge source ByteRange [{start},{end}) must be contained \
                 in exactly one route chunk, found {containing}"
            );
        }
    }

    #[test]
    fn drupal_routing_no_path_skipped() {
        // A route block without a `path:` key must not produce a chunk.
        let src = "mymodule.broken:\n  defaults:\n    _controller: 'Foo::bar'\n";
        let out = parse_drupal_routing(src, "mymodule.routing.yml");
        assert_eq!(out.chunks.len(), 0);
    }

    #[test]
    fn extract_handler_short_controller() {
        assert_eq!(
            extract_handler_short(r"\Drupal\foo\Controller\MyController::content"),
            "content"
        );
    }

    #[test]
    fn extract_handler_short_form() {
        assert_eq!(
            extract_handler_short(r"\Drupal\foo\Form\SettingsForm"),
            "SettingsForm"
        );
    }
}
