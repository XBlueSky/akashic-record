//! walker::edges — carved from walker.rs (EXT god-file split). See `super` for the
//! module overview; cross-cluster fns are re-exported there for `use super::*`.
use super::*;

// ─────────────────────────────────────────────────────────────
// Edge extraction (Call + Import)
// ─────────────────────────────────────────────────────────────

pub(crate) fn extract_import_edge(
    node: Node<'_>,
    config: &'static LanguageConfig,
    source: &[u8],
    rel_path: &str,
    out: &mut ExtractionOutput,
) -> Result<(), ExtractionError> {
    let specifier = config
        .hooks
        .resolve_import_specifier
        .and_then(|h| h(node, source))
        .or_else(|| default_import_specifier(node, source, config.import_source_field));

    let Some(spec) = specifier else {
        return Ok(());
    };

    let current = current_module_of(rel_path);
    let target_module = config
        .hooks
        .resolve_module_path
        .and_then(|h| h(&spec, &current))
        .unwrap_or_else(|| spec.clone());

    let names = config
        .hooks
        .resolve_import_names
        .map(|h| h(node, source))
        .unwrap_or_default();

    let line = node.start_position().row + 1;

    if names.is_empty() {
        out.edges.push(RawEdge {
            source: EdgeEndpoint::Name {
                name: current.clone(),
                module_specifier: None,
            },
            target: EdgeEndpoint::Name {
                // name = raw import specifier; resolver uses module_specifier for canonical resolution
                name: spec.clone(),
                module_specifier: Some(target_module),
            },
            kind: EdgeKind::Import,
            provenance: Provenance::Static,
            line: Some(line),
            metadata: EdgeMetadata::default(),
        });
    } else {
        for (local, source, spec_kind) in names {
            let mut md = EdgeMetadata::default();
            md.fields.insert(
                "edge.import_spec".to_string(),
                MetadataValue::String(format!("{spec_kind:?}").to_lowercase()),
            );
            // Record the source (definition-module) name ONLY when it differs
            // from the local binding (i.e. an aliased import). For non-aliased
            // imports source == local, so no metadata is added and goldens are
            // unchanged. Tier-1 resolution bridges the alias to the source.
            if source != local {
                md.fields.insert(
                    "edge.import_source".to_string(),
                    MetadataValue::String(source),
                );
            }
            out.edges.push(RawEdge {
                source: EdgeEndpoint::Name {
                    name: current.clone(),
                    module_specifier: None,
                },
                target: EdgeEndpoint::Name {
                    name: local,
                    module_specifier: Some(target_module.clone()),
                },
                kind: EdgeKind::Import,
                provenance: Provenance::Static,
                line: Some(line),
                metadata: md,
            });
        }
    }
    Ok(())
}

/// Emit one RawEdge of kind Call for the matched call expression. The source
/// endpoint is a ByteRange (the call node's own span); the Task-17 resolver
/// maps it to the enclosing chunk's UUID.
pub(crate) fn extract_call_edge(
    node: Node<'_>,
    config: &'static LanguageConfig,
    source: &[u8],
    type_env: &std::collections::HashMap<String, String>,
    trait_bound_tokens: &std::collections::HashSet<String>,
    out: &mut ExtractionOutput,
) -> Result<(), ExtractionError> {
    if let Some(skip) = config.hooks.should_skip_call
        && skip(node)
    {
        return Ok(());
    }

    let callee = config
        .hooks
        .resolve_call_name
        .and_then(|h| h(node, source))
        .or_else(|| {
            default_callee_name(
                node,
                source,
                config.member_expr_kinds,
                config.call_function_field,
                config.member_property_field,
            )
        });

    let Some(name) = callee else {
        return Ok(());
    };
    if name.is_empty() {
        return Ok(());
    }

    let mut metadata = EdgeMetadata::default();
    // D1-core: path-qualified receiver (`Foo::bar()`) — sets recv_type WITHOUT
    // recv_inferred (stays 0.9 "type_qualified" in the resolver).
    if let Some(recv_type) = config
        .hooks
        .resolve_call_receiver
        .and_then(|h| h(node, source))
    {
        metadata.fields.insert(
            "edge.recv_type".to_string(),
            MetadataValue::String(recv_type),
        );
    } else if let Some(token) = config
        .hooks
        .resolve_call_receiver_expr
        .and_then(|h| h(node, source))
        // D1b: value-method receiver (`x.m()` / `self.m()`) — resolve the leading
        // token through the per-chunk type_env to its base type, then flag it
        // inferred (→ 0.8 "receiver_type"). Unknown token / no type_env entry →
        // nothing (falls through to the bare-name cascade).
        && let Some(base) = type_env.get(&token)
    {
        metadata.fields.insert(
            "edge.recv_type".to_string(),
            MetadataValue::String(base.clone()),
        );
        metadata
            .fields
            .insert("edge.recv_inferred".to_string(), MetadataValue::Bool(true));
        // D3: the receiver's declared type was a trait object / trait-bound
        // generic param, so `base` is the TRAIT name — flag the edge so the
        // resolver's tier-0 `trait_default` branch labels it (0.5) instead of
        // `receiver_type` (0.8).
        if trait_bound_tokens.contains(&token) {
            metadata.fields.insert(
                "edge.recv_is_trait_bound".to_string(),
                MetadataValue::Bool(true),
            );
        }
    } else if let Some((chain_type, chain_method)) = config
        .hooks
        .resolve_call_receiver_chain
        .and_then(|h| h(node, source, type_env))
    {
        // D1c-1: outer link of a 2-hop method chain (`x.foo().bar()`) whose
        // INNER receiver is a simple typed token/self. Sets
        // chain_recv_type/chain_recv_method — the resolver's new tier looks
        // up by_return_type[(chain_recv_type, chain_recv_method)] to find
        // foo's return type, then by_type_and_name[(that, name)] for the
        // final hop.
        metadata.fields.insert(
            "edge.chain_recv_type".to_string(),
            MetadataValue::String(chain_type),
        );
        metadata.fields.insert(
            "edge.chain_recv_method".to_string(),
            MetadataValue::String(chain_method),
        );
    } else if let Some((field_type, field_name)) = config
        .hooks
        .resolve_call_receiver_field
        .and_then(|h| h(node, source, type_env))
    {
        // D1c-2: field-access receiver (`x.field.method()` /
        // `self.field.method()`) whose INNER receiver is a simple typed
        // token/self. Sets field_recv_type/field_name — the resolver's new
        // tier looks up by_field_type[(field_recv_type, field_name)] to find
        // the field's declared type, then by_type_and_name[(that, name)] for
        // the final hop.
        metadata.fields.insert(
            "edge.field_recv_type".to_string(),
            MetadataValue::String(field_type),
        );
        metadata.fields.insert(
            "edge.field_name".to_string(),
            MetadataValue::String(field_name),
        );
    }

    let pos = node.start_position();
    out.edges.push(RawEdge {
        source: EdgeEndpoint::ByteRange {
            start: node.start_byte(),
            end: node.end_byte(),
        },
        target: EdgeEndpoint::Name {
            name,
            module_specifier: None,
        },
        kind: EdgeKind::Call,
        provenance: Provenance::Static,
        line: Some(pos.row + 1),
        metadata,
    });
    Ok(())
}

/// A node-kind that classifies as a function or method (a callable chunk).
pub(crate) fn is_callable_kind(kind: &str, cfg: &LanguageConfig) -> bool {
    cfg.function_kinds.contains(&kind) || cfg.method_kinds.contains(&kind)
}

/// Walk a chunk's subtree extracting call + import edges, attributing them to
/// this chunk. Recurses the whole subtree by default. In EXT-5 nested-extraction
/// mode it STOPS at nested function/method definitions — each of those is its own
/// chunk and runs its own call-pass, so the enclosing chunk collects only its
/// OWN direct calls (correct innermost attribution, no duplicates).
pub(crate) fn call_pass(
    node: Node<'_>,
    config: &'static LanguageConfig,
    source: &[u8],
    rel_path: &str,
    type_env: &std::collections::HashMap<String, String>,
    trait_bound_tokens: &std::collections::HashSet<String>,
    out: &mut ExtractionOutput,
) -> Result<(), ExtractionError> {
    if config.call_kinds.contains(&node.kind()) {
        extract_call_edge(node, config, source, type_env, trait_bound_tokens, out)?;
    }
    if config.import_kinds.contains(&node.kind()) {
        extract_import_edge(node, config, source, rel_path, out)?;
    }
    let nested = config.hooks.nested_extraction;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        // EXT-5: a nested callable def is its own chunk + runs its own call-pass.
        if nested && is_callable_kind(child.kind(), config) {
            continue;
        }
        call_pass(
            child,
            config,
            source,
            rel_path,
            type_env,
            trait_bound_tokens,
            out,
        )?;
    }
    Ok(())
}
