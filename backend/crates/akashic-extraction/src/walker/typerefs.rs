//! walker::typerefs — carved from walker.rs (EXT god-file split). See `super` for the
//! module overview; cross-cluster fns are re-exported there for `use super::*`.
use super::*;

// ─────────────────────────────────────────────────────────────
// Edge extraction (Type References) — EXT-6b
// ─────────────────────────────────────────────────────────────

/// Default type-name extraction: the node's text, generics stripped, trailing
/// path segment kept (`Vec<Foo>` → "Vec", `a::B` → "B"). The leaf identifier is
/// what matches a defining chunk's short `name`.
pub(crate) fn default_type_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?;
    let head = text.split('<').next().unwrap_or(text).trim();
    let leaf = head.rsplit("::").next().unwrap_or(head).trim();
    if leaf.is_empty() {
        None
    } else {
        Some(leaf.to_string())
    }
}

/// Emit a `References{ref_kind:type}` edge for a type-reference node.
///
/// Returns `true` if the node was EXPANDED via the `expand_type_ref` hook (a
/// multi-ref node such as a PEP 604 union). The caller uses this to skip
/// recursing into the node's subtree — the expansion already owns it.
pub(crate) fn extract_type_ref_edge(
    node: Node<'_>,
    config: &'static LanguageConfig,
    source: &[u8],
    out: &mut ExtractionOutput,
) -> bool {
    // EXT-6b-4: a multi-ref node (e.g. a PEP 604 union `A | B`) expands into
    // several type references whose names/spans are the individual operands.
    // A non-empty result REPLACES the single-name emission below; an empty
    // result means "ordinary single-type node" → fall through.
    if let Some(hook) = config.hooks.expand_type_ref {
        let refs = hook(node, source);
        if !refs.is_empty() {
            for (name, start, end) in refs {
                if name.is_empty() {
                    continue;
                }
                push_type_ref_edge(node, start, end, name, out);
            }
            return true;
        }
    }

    let name = config
        .hooks
        .resolve_type_name
        .and_then(|h| h(node, source))
        .or_else(|| default_type_name(node, source));
    let Some(name) = name else { return false };
    if name.is_empty() {
        return false;
    }
    push_type_ref_edge(node, node.start_byte(), node.end_byte(), name, out);
    false
}

/// Push one `References{ref_kind:type}` edge. `node` provides the source line
/// (`start_position().row + 1`); `start`/`end` are the edge's `ByteRange` (the
/// node's own span for ordinary refs, or an operand's span for expanded refs).
pub(crate) fn push_type_ref_edge(
    node: Node<'_>,
    start: usize,
    end: usize,
    name: String,
    out: &mut ExtractionOutput,
) {
    let pos = node.start_position();
    let mut metadata = EdgeMetadata::default();
    metadata.fields.insert(
        "ref_kind".to_string(),
        MetadataValue::String("type".to_string()),
    );
    out.edges.push(RawEdge {
        source: EdgeEndpoint::ByteRange { start, end },
        target: EdgeEndpoint::Name {
            name,
            module_specifier: None,
        },
        kind: EdgeKind::References,
        provenance: Provenance::Static,
        line: Some(pos.row + 1),
        metadata,
    });
}

/// EXT-6c: emit IMPLEMENTS/EXTENDS edges for a chunk's explicit supertypes via
/// the language's resolve_supertypes hook. impl_kind metadata records
/// implements-vs-extends.
///
/// IMPLEMENTS/EXTENDS is a DECLARATION-LEVEL name→name edge (`Btn` implements
/// `Draw`), not a call/reference attributed to an enclosing chunk. The source is
/// therefore a `Name` endpoint carrying the SUBTYPE's own resolved name (for a
/// Rust impl_item that's the `type` field, e.g. "Btn"), resolved BY NAME in
/// Stage 6 — NOT a ByteRange, which would attribute to an enclosing chunk by
/// byte-span and drop here (no chunk encloses the impl header's start byte).
pub(crate) fn supertype_pass(
    chunk_node: Node<'_>,
    config: &'static LanguageConfig,
    source: &[u8],
    out: &mut ExtractionOutput,
) {
    let Some(hook) = config.hooks.resolve_supertypes else {
        return;
    };
    let supertypes = hook(chunk_node, source);
    if supertypes.is_empty() {
        return;
    }
    // The SUBTYPE is this node's own resolved name (same resolution the walker
    // uses to name a chunk / scope frame). If neither the resolve_name hook nor
    // the name_field yields a name, skip — correct for e.g. inherent impls.
    let subtype = config
        .hooks
        .resolve_name
        .and_then(|h| h(chunk_node, source))
        .or_else(|| {
            chunk_node
                .child_by_field_name(config.name_field)
                .and_then(|n| n.utf8_text(source).ok())
                .map(String::from)
        });
    let Some(subtype) = subtype else {
        return;
    };
    if subtype.is_empty() {
        return;
    }
    let pos = chunk_node.start_position();
    for (name, kind) in supertypes {
        if name.is_empty() {
            continue;
        }
        let mut metadata = EdgeMetadata::default();
        let impl_kind = if kind == EdgeKind::Extends {
            "extends"
        } else {
            "implements"
        };
        metadata.fields.insert(
            "impl_kind".to_string(),
            MetadataValue::String(impl_kind.to_string()),
        );
        out.edges.push(RawEdge {
            source: EdgeEndpoint::Name {
                name: subtype.clone(),
                module_specifier: None,
            },
            target: EdgeEndpoint::Name {
                name,
                module_specifier: None,
            },
            kind,
            provenance: Provenance::Static,
            line: Some(pos.row + 1),
            metadata,
        });
    }
}

/// EXT-6b: emit type-reference edges for one chunk. Scans the chunk's subtree
/// for `type_ref_kinds` nodes, attributing each to THIS chunk, but stops at
/// nested chunk boundaries (each nested chunk runs its own type_pass) and skips
/// the chunk's own defining name node (no self-reference).
///
/// EXT-6b-3: if `config.hooks.resolve_type_refs` is `Some`, calls the
/// position-aware hook instead of the `type_ref_kinds` node-scan and returns.
/// This is required for languages like C# where type names are bare `identifier`
/// nodes that cannot be distinguished from value identifiers by kind alone.
pub(crate) fn type_pass(
    chunk_node: Node<'_>,
    config: &'static LanguageConfig,
    source: &[u8],
    out: &mut ExtractionOutput,
) {
    // EXT-6b-3: position-aware hook takes precedence over type_ref_kinds scan.
    if let Some(hook) = config.hooks.resolve_type_refs {
        for (name, start, end) in hook(chunk_node, source) {
            if name.is_empty() {
                continue;
            }
            let mut metadata = EdgeMetadata::default();
            metadata.fields.insert(
                "ref_kind".to_string(),
                MetadataValue::String("type".to_string()),
            );
            // Derive the source line from the captured byte span so these edges
            // carry the same `line` provenance as the `extract_type_ref_edge`
            // path (which reads `node.start_position().row + 1`). Locate the
            // tightest node spanning the capture and read its start row.
            let line = chunk_node
                .descendant_for_byte_range(start, end.saturating_sub(1).max(start))
                .map(|n| n.start_position().row + 1);
            out.edges.push(RawEdge {
                source: EdgeEndpoint::ByteRange { start, end },
                target: EdgeEndpoint::Name {
                    name,
                    module_specifier: None,
                },
                kind: EdgeKind::References,
                provenance: Provenance::Static,
                line,
                metadata,
            });
        }
        return;
    }
    // Existing type_ref_kinds node-scan path.
    if config.type_ref_kinds.is_empty() {
        return;
    }
    let self_name_id = chunk_node
        .child_by_field_name(config.name_field)
        .map(|n| n.id());
    type_pass_walk(chunk_node, config, source, out, self_name_id, true);
}

pub(crate) fn type_pass_walk(
    node: Node<'_>,
    config: &'static LanguageConfig,
    source: &[u8],
    out: &mut ExtractionOutput,
    self_name_id: Option<usize>,
    is_root: bool,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if Some(child.id()) == self_name_id {
            continue;
        }
        if !is_root && classify_chunk_kind(child.kind(), config).is_some() {
            continue;
        }
        // A type-ref node that EXPANDS (e.g. a PEP 604 union whose operands are
        // themselves nested `type` nodes, like `List[A] | None`) owns its whole
        // subtree: the expansion already emitted each operand once. Recursing
        // into it would re-emit those nested operand `type` nodes, double-
        // counting the generic head and leaking the dropped `None`. So skip the
        // descent when the node was expanded.
        let expanded = config.type_ref_kinds.contains(&child.kind())
            && extract_type_ref_edge(child, config, source, out);
        if !expanded {
            type_pass_walk(child, config, source, out, self_name_id, false);
        }
    }
}

/// Chunk categories that do not contain sub-chunks. Leaf chunks get a
/// call-pass to extract their interior edges; non-leaf containers
/// (class/struct/interface/trait/namespace/module) are instead recursed
/// into by the main walk, which reaches their inner leaf chunks.
pub(crate) fn is_leaf_chunk(category: ChunkCategory) -> bool {
    matches!(
        category,
        ChunkCategory::Function
            | ChunkCategory::Method
            | ChunkCategory::Macro
            | ChunkCategory::Variable
            | ChunkCategory::Constant
            | ChunkCategory::TypeAlias
            | ChunkCategory::Enum
    )
}
