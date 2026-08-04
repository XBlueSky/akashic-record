//! walker::routes — carved from walker.rs (EXT god-file split). See `super` for the
//! module overview; cross-cluster fns are re-exported there for `use super::*`.
use super::*;

// ─────────────────────────────────────────────────────────────
// Framework route extraction — EXT-7-1
// ─────────────────────────────────────────────────────────────

/// Normalize a captured HTTP-method token to a canonical verb (or `ANY`).
///
/// The `@route.method` capture is a raw grammar token that varies by framework:
/// a bare verb (`get`, `POST`), a Spring annotation (`GetMapping`,
/// `RequestMapping`), or a generic registration call (`route`). We canonicalize
/// so route chunk names and edge metadata are framework-independent:
///
/// * plain verbs (`GET`/`POST`/`PUT`/`DELETE`/`PATCH`/`HEAD`/`OPTIONS`/`ALL`) →
///   pass through unchanged (after uppercasing).
/// * `*Mapping` annotations → strip the trailing `MAPPING`
///   (`GETMAPPING`→`GET`, `POSTMAPPING`→`POST`, …); `REQUESTMAPPING`→`REQUEST`
///   which then maps to `ANY` (Spring `@RequestMapping` with no method binds any
///   verb).
/// * `ROUTE` (Flask `@app.route`, with no `methods=` kwarg captured) → `ANY`.
///
/// Anything unrecognized passes through uppercased verbatim.
pub(crate) fn normalize_http_method(raw: &str) -> String {
    let upper = raw.to_uppercase();
    let stripped = upper.strip_suffix("MAPPING").unwrap_or(&upper);
    match stripped {
        "REQUEST" | "ROUTE" => "ANY".to_string(),
        other => other.to_string(),
    }
}

/// The last `::`- or `.`-delimited segment of a handler reference.
///
/// Axum handlers are module-qualified (`health::health`,
/// `graph::core::get_graph`); the RoutesTo edge must target the bare handler
/// name so it resolves against a defining chunk's short `name`. Applied ONLY to
/// the handler (never the path).
pub(crate) fn last_handler_segment(raw: &str) -> &str {
    raw.rsplit([':', '.']).next().unwrap_or(raw).trim()
}

/// Run each `framework_routes` QueryDef against the parsed tree, emitting one
/// `route` chunk ("METHOD /path") and one `RoutesTo` edge (route → handler)
/// per match. Invoked at the end of [`extract`] after the main walk.
///
/// Capture conventions (all optional; absent ⇒ sensible default):
/// * `@route.path`    — the path string literal (quotes stripped).
/// * `@route.method`  — the HTTP method identifier (e.g. `get`, `post`).
/// * `@route.handler` — the handler identifier.
/// * `@route.reg`     — the whole registration node; used as the route chunk's
///   span so the `RoutesTo` edge's `ByteRange` source falls inside it.
pub(crate) fn run_framework_routes(
    tree: &tree_sitter::Tree,
    config: &'static LanguageConfig,
    source: &[u8],
    out: &mut ExtractionOutput,
) -> Result<(), ExtractionError> {
    let lang = (config.language_fn)();
    for qd in config.queries.framework_routes {
        let query = qd.compile(&lang, config.name)?;

        // Capture name → index mapping (computed once per QueryDef).
        let idx_path = query.capture_index_for_name("route.path");
        let idx_method = query.capture_index_for_name("route.method");
        let idx_handler = query.capture_index_for_name("route.handler");
        let idx_reg = query.capture_index_for_name("route.reg");

        let mut cursor = QueryCursor::new();
        let mut it = cursor.matches(query, tree.root_node(), source);
        it.advance();
        while let Some(m) = it.get() {
            // Helper: find the first capture node for a given index.
            let capture_node = |idx: Option<u32>| -> Option<Node> {
                let idx = idx?;
                m.captures.iter().find(|c| c.index == idx).map(|c| c.node)
            };

            let path_node = capture_node(idx_path);
            let method_node = capture_node(idx_method);
            let handler_node = capture_node(idx_handler);
            let reg_node = capture_node(idx_reg);

            // We must at least have the path to emit a meaningful route chunk.
            let Some(path_n) = path_node else {
                it.advance();
                continue;
            };

            // Extract and clean the path string (strip surrounding quotes).
            let raw_path = path_n.utf8_text(source).unwrap_or("");
            // string_literal content may include the quote chars; strip them.
            let path = raw_path.trim_matches('"').trim_matches('\'').to_string();

            // HTTP method: normalize the captured token to a canonical verb (or
            // ANY for generic mappings/registrations); default "ANY".
            let method = method_node
                .and_then(|n| n.utf8_text(source).ok())
                .map_or_else(|| "ANY".to_string(), normalize_http_method);

            let route_name = format!("{method} {path}");

            // Span: prefer the @route.reg node (the full registration call),
            // then fall back to the path node. The RoutesTo edge's source
            // ByteRange is set to the handler's start byte — which falls
            // INSIDE the reg node's span — so Stage-6 attributes the edge to
            // this route chunk.
            let span_node = reg_node.unwrap_or(path_n);
            // BUG (chained builder): for a builder chain like
            // `Router::new().route(a).route(b)`, each link's `@route.reg`
            // `call_expression` has a `function: (field_expression value: <prior
            // chain> field: "route")` whose `value` is the ENTIRE preceding
            // chain. Its `start_byte` is therefore the chain ROOT (`Router`), so
            // every later route chunk's span swallows — and duplicates — every
            // earlier registration. Pin the START to THIS link's `.route` member
            // (the `field` child of the reg's `function` field_expression), which
            // begins right after the `.`; the reg's `end_byte` already bounds
            // this link correctly. The result spans only `route(a)` / `route(b)`
            // per chunk, non-overlapping, and the handler's start byte still
            // falls inside it (so the RoutesTo ByteRange attribution holds).
            // Frameworks whose @route.reg is NOT a method-call chain (e.g. Java
            // `method_declaration`, TS object literal, JSX element) lack this
            // `function/field` shape, so they keep the full reg span unchanged.
            let start_byte = span_node
                .child_by_field_name("function")
                .filter(|f| f.kind() == "field_expression")
                .and_then(|f| f.child_by_field_name("field"))
                .map_or_else(|| span_node.start_byte(), |field| field.start_byte());
            let end_byte = span_node.end_byte();
            // start_line/start_position must match the (possibly narrowed)
            // start_byte, so derive them from the byte offset rather than the
            // chain-root node. The narrowed start is on the same line as the
            // reg-call end in the common single-line chain link; for multi-line
            // chains the per-link `.route` start gives the correct line.
            let span_start_node = span_node
                .descendant_for_byte_range(start_byte, start_byte)
                .unwrap_or(span_node);
            let start_line = span_start_node.start_position().row + 1;
            let end_line = span_node.end_position().row + 1;

            // Build the route chunk content from its span.
            let content = std::str::from_utf8(&source[start_byte..end_byte])
                .unwrap_or("")
                .to_string();

            let mut chunk_meta = ChunkMetadata::default();
            chunk_meta.fields.insert(
                "http_method".to_string(),
                MetadataValue::String(method.clone()),
            );
            chunk_meta
                .fields
                .insert("http_path".to_string(), MetadataValue::String(path.clone()));

            // BUG 5(b): the human-facing `name` is "{METHOD} {path}", but two
            // registrations with the same method+path (or, pre-normalization,
            // the same display string) would collide in the chunk index's
            // last-write-wins `by_path_and_name` map, orphaning one RoutesTo
            // edge. Disambiguate the FQN with the PATH node's start byte — unique
            // per registration within a file (the @route.reg span is NOT: for a
            // chained builder like `.route(a).route(b)` the reg call_expression
            // of each link starts at the same chain-root byte). We keep `name`
            // unchanged for display. Downstream e2e assertions query by
            // name/http_path, not fqn.
            let route_fqn = format!("{route_name}@{}", path_n.start_byte());

            out.chunks.push(RawChunk {
                chunk_type: "route".to_string(),
                name: route_name.clone(),
                fqn: Some(route_fqn),
                parent_fqn: None,
                start_line,
                end_line,
                start_byte,
                end_byte,
                signature: None,
                content,
                doc: None,
                receiver: None,
                is_async: false,
                is_static: false,
                is_const: false,
                is_exported: true,
                visibility: Visibility::Public,
                metadata: chunk_meta,
            });

            // Emit RoutesTo edge if we have a handler capture.
            if let Some(handler_n) = handler_node {
                // BUG 1: handlers may be module-qualified (`health::health`,
                // `graph::core::get_graph`) or member paths; the edge target must
                // be the bare LAST segment so it resolves against a defining
                // chunk's short `name`. The path is NOT normalized this way.
                let raw_handler = handler_n.utf8_text(source).unwrap_or("");
                let handler_name = last_handler_segment(raw_handler).to_string();
                if !handler_name.is_empty() {
                    let handler_start = handler_n.start_byte();
                    let handler_end = handler_n.end_byte();
                    let handler_line = handler_n.start_position().row + 1;

                    let mut edge_meta = EdgeMetadata::default();
                    edge_meta.fields.insert(
                        "http_method".to_string(),
                        MetadataValue::String(method.clone()),
                    );
                    edge_meta
                        .fields
                        .insert("http_path".to_string(), MetadataValue::String(path.clone()));

                    out.edges.push(RawEdge {
                        // Source is a ByteRange inside the route chunk's span so
                        // Stage-6 attribute resolution maps this edge to the route chunk.
                        source: EdgeEndpoint::ByteRange {
                            start: handler_start,
                            end: handler_end,
                        },
                        target: EdgeEndpoint::Name {
                            name: handler_name,
                            module_specifier: None,
                        },
                        kind: EdgeKind::RoutesTo,
                        provenance: Provenance::Static,
                        line: Some(handler_line),
                        metadata: edge_meta,
                    });
                }
            }

            it.advance();
        }
    }
    Ok(())
}

/// Reduce an HTTP URL/path string literal to its path component, or return
/// `None` when the literal is not URL-shaped (the URL-shape guard that excludes
/// map-style `.get("key")` calls).
///
/// * `http(s)://host[:port]/p` → `/p` (path from the first `/` after the
///   authority; a URL with no path → `/`).
/// * a bare path (`/api/x`) → kept verbatim.
/// * anything else → `None`.
pub(crate) fn http_url_to_path(literal: &str) -> Option<String> {
    if let Some(rest) = literal
        .strip_prefix("http://")
        .or_else(|| literal.strip_prefix("https://"))
    {
        return Some(match rest.find('/') {
            Some(i) => rest[i..].to_string(),
            None => "/".to_string(),
        });
    }
    if literal.starts_with('/') {
        return Some(literal.to_string());
    }
    None
}

/// Run each `http_calls` QueryDef, emitting one `http_call` chunk
/// ("METHOD /path") and one `MakesHttpCall` edge (caller → http_call chunk) per
/// LITERAL-path client call. Invoked from [`extract`] right after
/// [`run_framework_routes`]. Mirrors `run_framework_routes`.
///
/// Captures: `@call.method` (verb, #match?-restricted), `@call.url` (the URL
/// string literal, first arg), `@call.reg` (the whole call_expression — its span
/// is the http_call chunk's span AND the MakesHttpCall edge's source ByteRange).
pub(crate) fn run_http_calls(
    tree: &tree_sitter::Tree,
    config: &'static LanguageConfig,
    source: &[u8],
    out: &mut ExtractionOutput,
) -> Result<(), ExtractionError> {
    let lang = (config.language_fn)();
    for qd in config.queries.http_calls {
        let query = qd.compile(&lang, config.name)?;
        let idx_method = query.capture_index_for_name("call.method");
        let idx_url = query.capture_index_for_name("call.url");
        let idx_reg = query.capture_index_for_name("call.reg");

        let mut cursor = QueryCursor::new();
        let mut it = cursor.matches(query, tree.root_node(), source);
        it.advance();
        while let Some(m) = it.get() {
            let capture_node = |idx: Option<u32>| -> Option<Node> {
                let idx = idx?;
                m.captures.iter().find(|c| c.index == idx).map(|c| c.node)
            };
            let (Some(method_n), Some(url_n), Some(reg_n)) = (
                capture_node(idx_method),
                capture_node(idx_url),
                capture_node(idx_reg),
            ) else {
                it.advance();
                continue;
            };

            // URL literal: strip surrounding quotes (mirror route path cleaning).
            let raw_url = url_n.utf8_text(source).unwrap_or("");
            let url = raw_url.trim_matches('"').trim_matches('\'');

            // URL-shape guard + full-URL → path reduction.
            let Some(path) = http_url_to_path(url) else {
                it.advance();
                continue;
            };

            // Verb → canonical (uppercased); reuse the route method normalizer.
            let method = method_n
                .utf8_text(source)
                .map_or_else(|_| "ANY".to_string(), normalize_http_method);

            let call_name = format!("{method} {path}");
            let start_byte = reg_n.start_byte();
            let end_byte = reg_n.end_byte();
            let start_line = reg_n.start_position().row + 1;
            let end_line = reg_n.end_position().row + 1;
            let content = std::str::from_utf8(&source[start_byte..end_byte])
                .unwrap_or("")
                .to_string();

            let mut chunk_meta = ChunkMetadata::default();
            chunk_meta.fields.insert(
                "http_method".to_string(),
                MetadataValue::String(method.clone()),
            );
            chunk_meta
                .fields
                .insert("http_path".to_string(), MetadataValue::String(path.clone()));

            // fqn disambiguated by the call start byte (unique per call site),
            // mirroring the route chunk's `@start_byte` scheme.
            let call_fqn = format!("{call_name}@{start_byte}");

            out.chunks.push(RawChunk {
                chunk_type: "http_call".to_string(),
                name: call_name.clone(),
                fqn: Some(call_fqn),
                parent_fqn: None,
                start_line,
                end_line,
                start_byte,
                end_byte,
                signature: None,
                content,
                doc: None,
                receiver: None,
                is_async: false,
                is_static: false,
                is_const: false,
                is_exported: false,
                visibility: Visibility::Private,
                metadata: chunk_meta,
            });

            let mut edge_meta = EdgeMetadata::default();
            edge_meta.fields.insert(
                "http_method".to_string(),
                MetadataValue::String(method.clone()),
            );
            edge_meta
                .fields
                .insert("http_path".to_string(), MetadataValue::String(path.clone()));

            out.edges.push(RawEdge {
                // Source ByteRange = the call span → Stage-6 attributes it to the
                // enclosing FUNCTION (http_call chunks are excluded from Stage-6
                // source-span candidates so they never shadow the function).
                source: EdgeEndpoint::ByteRange {
                    start: start_byte,
                    end: end_byte,
                },
                // Target = the http_call chunk's NAME (`ChunkIndex` keys on name,
                // not fqn); resolves intra-repo via same-module (tier-2) lookup.
                target: EdgeEndpoint::Name {
                    name: call_name,
                    module_specifier: None,
                },
                kind: EdgeKind::MakesHttpCall,
                provenance: Provenance::Static,
                line: Some(start_line),
                metadata: edge_meta,
            });

            it.advance();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::http_url_to_path;

    #[test]
    fn url_to_path_reduction() {
        assert_eq!(http_url_to_path("/api/x"), Some("/api/x".to_string()));
        assert_eq!(
            http_url_to_path("https://svc/api/y"),
            Some("/api/y".to_string())
        );
        assert_eq!(http_url_to_path("http://h:8080/z"), Some("/z".to_string()));
        assert_eq!(http_url_to_path("https://svc"), Some("/".to_string()));
        assert_eq!(http_url_to_path("some_key"), None);
        assert_eq!(http_url_to_path(""), None);
    }
}
