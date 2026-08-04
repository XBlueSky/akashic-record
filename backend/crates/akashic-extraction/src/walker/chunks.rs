//! walker::chunks — carved from walker.rs (EXT god-file split). See `super` for the
//! module overview; cross-cluster fns are re-exported there for `use super::*`.
use super::*;
use crate::resolution::chunk_index::normalize_base_type;

pub fn extract(
    config: &'static LanguageConfig,
    source: &[u8],
    rel_path: &str,
) -> Result<ExtractionOutput, ExtractionError> {
    let tree = parser_pool::parse_with(config, source)?;
    let mut out = ExtractionOutput::default();
    let mut scope: ScopeStack = Vec::with_capacity(8);
    walk(
        tree.root_node(),
        config,
        source,
        rel_path,
        &mut out,
        &mut scope,
        false,
    )?;
    if !config.queries.framework_routes.is_empty() {
        run_framework_routes(&tree, config, source, &mut out)?;
    }
    if !config.queries.http_calls.is_empty() {
        run_http_calls(&tree, config, source, &mut out)?;
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────
// Classification helpers
// ─────────────────────────────────────────────────────────────
// `ChunkCategory` lives in `types.rs` (pub) so language configs can name it in
// their `refine_category` hook; it is in scope here via `use types::*`.

pub(crate) fn classify_chunk_kind(kind: &str, cfg: &LanguageConfig) -> Option<ChunkCategory> {
    if cfg.function_kinds.contains(&kind) {
        return Some(ChunkCategory::Function);
    }
    if cfg.class_kinds.contains(&kind) {
        return Some(ChunkCategory::Class);
    }
    if cfg.method_kinds.contains(&kind) {
        return Some(ChunkCategory::Method);
    }
    if cfg.interface_kinds.contains(&kind) {
        return Some(ChunkCategory::Interface);
    }
    if cfg.struct_kinds.contains(&kind) {
        return Some(ChunkCategory::Struct);
    }
    if cfg.enum_kinds.contains(&kind) {
        return Some(ChunkCategory::Enum);
    }
    if cfg.trait_kinds.contains(&kind) {
        return Some(ChunkCategory::Trait);
    }
    if cfg.type_alias_kinds.contains(&kind) {
        return Some(ChunkCategory::TypeAlias);
    }
    if cfg.variable_kinds.contains(&kind) {
        return Some(ChunkCategory::Variable);
    }
    if cfg.constant_kinds.contains(&kind) {
        return Some(ChunkCategory::Constant);
    }
    if cfg.macro_kinds.contains(&kind) {
        return Some(ChunkCategory::Macro);
    }
    if cfg.namespace_kinds.contains(&kind) {
        return Some(ChunkCategory::Namespace);
    }
    if cfg.module_kinds.contains(&kind) {
        return Some(ChunkCategory::Module);
    }
    None
}

pub(crate) fn is_scope_container(kind: &str, cfg: &LanguageConfig) -> bool {
    cfg.class_kinds.contains(&kind)
        || cfg.struct_kinds.contains(&kind)
        || cfg.interface_kinds.contains(&kind)
        || cfg.trait_kinds.contains(&kind)
        || cfg.namespace_kinds.contains(&kind)
        || cfg.module_kinds.contains(&kind)
        || cfg.impl_kinds.contains(&kind)
}

// ─────────────────────────────────────────────────────────────
// Recursive walker
// ─────────────────────────────────────────────────────────────

// Split the mutable state into separate parameters to avoid a single `&mut
// WalkContext` borrow that the borrow-checker would refuse to split across
// nested calls.  `out` and `scope` are borrowed mutably while `config`,
// `source`, and `rel_path` are shared references — no aliasing conflict.
pub(crate) fn walk(
    node: Node<'_>,
    config: &'static LanguageConfig,
    source: &[u8],
    rel_path: &str,
    out: &mut ExtractionOutput,
    scope: &mut ScopeStack,
    // EXT-5: true once we are walking INSIDE a function/method body (nested mode),
    // so the walk keeps descending to find nested defs but only chunks callables
    // there (not local variables/consts) and attributes their calls correctly.
    in_callable: bool,
) -> Result<(), ExtractionError> {
    let kind = node.kind();

    // Top-level imports (outside any chunk) are extracted here. Import
    // extraction does not depend on scope, so its position relative to the
    // scope push below is irrelevant; keep it first.
    if config.import_kinds.contains(&kind) {
        extract_import_edge(node, config, source, rel_path, out)?;
    }

    // Determine recursion up-front: it gates whether a leaf chunk needs its own
    // call-pass (see below).
    let nested = config.hooks.nested_extraction;
    let base_recurse = match config.hooks.should_recurse_into {
        Some(hook) => hook(node),
        // Default: enter scope containers and the root only; do NOT descend
        // into function/method bodies — preserves the pre-refactor
        // "top-level only" extraction behavior.
        None => is_scope_container(kind, config) || node.parent().is_none(),
    };
    // EXT-5: in nested-extraction mode, descend into callable nodes AND keep
    // descending once inside a callable body, so NAMED nested functions anywhere
    // in the body become their own chunks.
    let should_recurse =
        base_recurse || (nested && (is_callable_kind(kind, config) || in_callable));

    // Chunk-worthy? Emit it; for LEAF chunks whose body is NOT also recursed,
    // run a call-pass over the subtree to extract calls/imports from the chunk
    // interior (the main recursion does not descend into function bodies, so
    // that is where those calls would otherwise be missed).
    //
    // The `!should_recurse` guard matters for a node that is BOTH a leaf chunk
    // AND a recursion target — e.g. an enum whose body holds methods. Recursing
    // reaches its leaf members, each of which runs its own call_pass; also
    // call-passing the enum itself would double-count every call inside an enum
    // method. A leaf whose body is not recursed (a function/method body, or a
    // body-less enum) still needs the call_pass.
    //
    // This runs BEFORE pushing this node's own scope frame, so a container's
    // own FQN excludes itself (e.g. class `Counter` → fqn "Counter", not
    // "Counter.Counter", and parent_fqn None). Children still see the container
    // once its frame is pushed below.
    // EXT-5: inside a callable body (nested mode), only chunk nested callables —
    // skip local variables/constants/etc. that would otherwise become noise.
    let chunkable = !(nested && in_callable && !is_callable_kind(kind, config));
    // Tracks whether THIS node was emitted as an Enum chunk, so the enum-member
    // pass below can trigger off the refined category — covering both languages
    // that classify enums by a dedicated node kind and those (Kotlin, Swift)
    // that overload `class_declaration` and refine to Enum via `refine_category`.
    let mut emitted_enum = false;
    if let Some(category) = classify_chunk_kind(kind, config).filter(|_| chunkable) {
        // A grammar may overload one node-kind for several constructs (Swift's
        // class_declaration = class/struct/enum; Kotlin's = class/interface/
        // enum). Let the config refine the category by inspecting child tokens.
        // This only changes the emitted chunk_type — leaf-ness (call-pass) and
        // scope-frame pushing are unaffected (the refined categories are all
        // non-leaf, and scope pushing keys off node-kind, not category).
        let category = config
            .hooks
            .refine_category
            .map_or(category, |h| h(node, source, category));
        if let Some(chunk) = extract_chunk(node, config, source, scope, category)? {
            emitted_enum = category == ChunkCategory::Enum;
            out.chunks.push(chunk);
            // EXT-6b: every chunk (leaf or container) contributes its type
            // references; type_pass no-ops when type_ref_kinds is empty.
            type_pass(node, config, source, out);
            // EXT-6c: emit IMPLEMENTS/EXTENDS edges for explicit supertypes.
            supertype_pass(node, config, source, out);
            // Leaf chunks run a call-pass to collect their interior calls.
            // Normally only when the chunk's body is NOT otherwise recursed.
            // EXT-5: a callable chunk runs a BOUNDED call-pass (stops at nested
            // callables) even while recursing into its body to find nested defs.
            let run_call_pass = is_leaf_chunk(category)
                && (!should_recurse || (nested && is_callable_kind(kind, config)));
            if run_call_pass {
                // D1b: build this chunk's intra-chunk type environment for
                // value-method receiver resolution. The language hook contributes
                // the value bindings (let/ctor/param); `self` is injected here
                // from the nearest enclosing type scope frame (Impl/Struct/Trait/
                // Enum), which only the walker holds. OOP languages (e.g. Python
                // `class_definition` → `ScopeKind::Class`) get `{self, Self}`
                // injected too — `enclosing_type_name` is gated on scope kind, not
                // on a hook. Output stays byte-identical for those languages only
                // because the D1b READ side (`resolve_call_receiver_expr` hook) is
                // `None` for non-Rust, so the env is built but never consulted.
                // D3: build_type_env now also returns the set of binding names
                // whose declared type was a trait object / trait-bound generic
                // (`trait_bound_tokens`), threaded alongside `type_env` into the
                // call-pass so `extract_call_edge` can flag `recv_is_trait_bound`.
                let (mut type_env, trait_bound_tokens) = config
                    .hooks
                    .build_type_env
                    .map(|h| h(node, source))
                    .unwrap_or_default();
                if let Some(self_ty) = enclosing_type_name(scope).map(|t| normalize_base_type(&t)) {
                    // Both `self` and `Self` receivers resolve to the enclosing
                    // type. `Self::assoc()` is path-qualified (handled by
                    // resolve_call_receiver), but `Self`-typed value bindings and
                    // `self.method()` both key here. Normalize to the bare base
                    // type (same as `ChunkIndex::insert_method` and
                    // `rust_build_type_env`) so a generic (`impl<T> Holder<T>`) or
                    // path-qualified impl still matches the tier-0 index key.
                    // Clone for the `self` key, move into the `Self` key. `self`
                    // is a concrete type, never a trait_bound_token.
                    type_env.insert("self".to_string(), self_ty.clone());
                    type_env.insert("Self".to_string(), self_ty);
                }
                call_pass(
                    node,
                    config,
                    source,
                    rel_path,
                    &type_env,
                    &trait_bound_tokens,
                    out,
                )?;
            }
        }
    }

    // EXT-6c: impl_kinds nodes are scope containers but NOT chunkable (they have
    // no category in classify_chunk_kind). Run supertype_pass on them directly so
    // `impl Trait for Type` produces Implements edges even when the impl_item is
    // not itself chunked.
    if config.impl_kinds.contains(&kind) {
        supertype_pass(node, config, source, out);
    }

    // Now push this node's scope frame (if it's a container) so its CHILDREN
    // resolve FQNs relative to it.
    let pushed_scope = maybe_push_scope(node, config, source, scope);

    // Enum members: emit each variant/case/constant as its own chunk. This runs
    // AFTER the enum's scope frame is pushed, so members resolve fqn
    // "Enum.Member" and parent_fqn = the enum. `enum_member_kinds` is populated
    // per-language; `classify_chunk_kind` deliberately does NOT list those kinds,
    // so members are emitted ONLY here — never double-emitted via the general
    // recursion that some languages (e.g. Java) run into enum bodies for methods.
    // Triggers off the refined Enum category (not the raw node kind) so it also
    // covers Kotlin/Swift, which emit enums from an overloaded `class_declaration`.
    if pushed_scope && emitted_enum && !config.enum_member_kinds.is_empty() {
        emit_enum_members(node, config, source, scope, out)?;
    }

    if should_recurse {
        // EXT-5: children are "inside a callable" if we already are, or this node
        // is itself a callable whose body we're descending.
        let child_in_callable = in_callable || (nested && is_callable_kind(kind, config));
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            walk(
                child,
                config,
                source,
                rel_path,
                out,
                scope,
                child_in_callable,
            )?;
        }
    }

    if pushed_scope {
        scope.pop();
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Scope management
// ─────────────────────────────────────────────────────────────

pub(crate) fn maybe_push_scope(
    node: Node<'_>,
    config: &'static LanguageConfig,
    source: &[u8],
    scope: &mut ScopeStack,
) -> bool {
    let kind = node.kind();
    let scope_kind = if config.class_kinds.contains(&kind) {
        Some(ScopeKind::Class)
    } else if config.struct_kinds.contains(&kind) {
        Some(ScopeKind::Struct)
    } else if config.interface_kinds.contains(&kind) {
        Some(ScopeKind::Interface)
    } else if config.trait_kinds.contains(&kind) {
        Some(ScopeKind::Trait)
    } else if config.enum_kinds.contains(&kind) {
        // Enums are scope containers for their members/methods (e.g. a PHP or
        // Java enum method must nest under the enum in its FQN). Pushing the
        // frame here — but deliberately NOT in `is_scope_container` — means the
        // enum name participates in child FQNs whenever a language's
        // `should_recurse_into` hook descends the enum body, without changing
        // the default (hook-less) recursion behavior for any language.
        Some(ScopeKind::Enum)
    } else if config.namespace_kinds.contains(&kind) {
        Some(ScopeKind::Namespace)
    } else if config.module_kinds.contains(&kind) {
        Some(ScopeKind::Module)
    } else if config.impl_kinds.contains(&kind) {
        Some(ScopeKind::Impl)
    } else if config.hooks.nested_extraction && is_callable_kind(kind, config) {
        // EXT-5: a function/method is a scope for its NAMED nested functions, so
        // they get parent_fqn = the enclosing function (e.g. `outer.inner`). Only
        // in nested mode; the frame only affects children, which are only chunked
        // when the body is descended (also nested mode).
        Some(ScopeKind::Function)
    } else {
        None
    };

    let Some(scope_kind) = scope_kind else {
        return false;
    };

    let name = config
        .hooks
        .resolve_name
        .map_or_else(
            || default_name_from_field(node, source, config.name_field),
            |h| h(node, source),
        )
        .unwrap_or_else(|| "<anonymous>".to_string());

    scope.push(ScopeFrame {
        kind: scope_kind,
        name,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    });
    true
}

/// The nearest enclosing TYPE scope-frame name, i.e. the type `self`/`Self`
/// refer to inside a method. Scans the scope stack from innermost outward for
/// the first `Impl`/`Struct`/`Class`/`Trait`/`Enum` frame. D1b: seeds the
/// per-chunk `type_env` with `self`'s type (the same value the chunk's
/// `parent_fqn` is built from). `None` for a free function (no enclosing type).
pub(crate) fn enclosing_type_name(scope: &ScopeStack) -> Option<String> {
    scope
        .iter()
        .rev()
        .find(|f| {
            matches!(
                f.kind,
                ScopeKind::Impl
                    | ScopeKind::Struct
                    | ScopeKind::Class
                    | ScopeKind::Trait
                    | ScopeKind::Enum
            )
        })
        .map(|f| f.name.clone())
}

// ─────────────────────────────────────────────────────────────
// Chunk construction
// ─────────────────────────────────────────────────────────────

pub(crate) fn extract_chunk(
    node: Node<'_>,
    config: &'static LanguageConfig,
    source: &[u8],
    scope: &ScopeStack,
    category: ChunkCategory,
) -> Result<Option<RawChunk>, ExtractionError> {
    let name = config.hooks.resolve_name.map_or_else(
        || default_name_from_field(node, source, config.name_field),
        |h| h(node, source),
    );
    // Enum members are usually not handled by a language's `resolve_name` /
    // `name_field` (those target declarations, not variants), so fall back to
    // the member node's own `name` field or first identifier child. This lets
    // members resolve uniformly across grammars without per-language hooks.
    let name = name.or_else(|| {
        (category == ChunkCategory::EnumMember)
            .then(|| enum_member_name_fallback(node, source))
            .flatten()
    });
    let Some(name) = name else {
        return Ok(None);
    };

    let fqn = config
        .hooks
        .resolve_fqn
        .map_or_else(|| default_fqn(&name, scope), |h| h(node, source, scope));
    // Enum members: if a language's `resolve_fqn` hook doesn't cover the member
    // node (returns None, e.g. Kotlin's), fall back to the scope-derived
    // "Enum.Member" fqn so members are addressable consistently across grammars.
    let fqn = fqn.or_else(|| {
        (category == ChunkCategory::EnumMember)
            .then(|| default_fqn(&name, scope))
            .flatten()
    });

    let signature = config
        .hooks
        .get_signature
        .map_or_else(|| default_signature(node, source), |h| h(node, source));

    let is_async = config.hooks.is_async.is_some_and(|h| h(node));
    let is_static = config.hooks.is_static.is_some_and(|h| h(node));
    let is_const = config.hooks.is_const.is_some_and(|h| h(node));
    let is_exported = config.hooks.is_exported.is_some_and(|h| h(node));
    let visibility = config
        .hooks
        .get_visibility
        .map_or(Visibility::Unknown, |h| h(node, source));

    let receiver = if category == ChunkCategory::Method {
        config
            .hooks
            .resolve_method_receiver
            .and_then(|h| h(node, source))
    } else {
        None
    };

    let doc = extract_leading_doc(node, source, config.doc_comment_kinds);

    let content = utf8_text(node, source)
        .map_err(|_| ExtractionError::InvalidUtf8 {
            byte: node.start_byte(),
        })?
        .to_string();

    let parent_fqn = scope_path(scope);

    Ok(Some(RawChunk {
        chunk_type: category.as_str().into(),
        name,
        fqn,
        parent_fqn,
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        signature,
        content,
        doc,
        receiver,
        is_async,
        is_static,
        is_const,
        is_exported,
        visibility,
        metadata: ChunkMetadata::default(),
    }))
}

// ─────────────────────────────────────────────────────────────
// Enum members
// ─────────────────────────────────────────────────────────────

/// Emit one [`ChunkCategory::EnumMember`] chunk per enum member found anywhere
/// under `enum_node`. Called once per enum, AFTER the enum's scope frame is
/// pushed, so members get `fqn = "Enum.Member"` and `parent_fqn = the enum`.
///
/// We descend depth-first looking only for nodes whose kind is in
/// `enum_member_kinds`, rather than reusing the main walker's recursion, so
/// members are emitted uniformly regardless of each grammar's intermediate
/// enum-body node names (`enum_member_declaration_list`, `enum_body`,
/// `enum_variant_list`, `enum_class_body`, …). Descent stops at a member node
/// (its sub-nodes are not themselves members).
pub(crate) fn emit_enum_members(
    enum_node: Node<'_>,
    config: &'static LanguageConfig,
    source: &[u8],
    scope: &ScopeStack,
    out: &mut ExtractionOutput,
) -> Result<(), ExtractionError> {
    let mut cursor = enum_node.walk();
    for child in enum_node.named_children(&mut cursor) {
        collect_enum_members(child, config, source, scope, out)?;
    }
    Ok(())
}

/// Best-effort name for an enum member when the language's `resolve_name` /
/// `name_field` does not cover the member node kind: prefer an explicit `name`
/// field, else the first identifier-like named child.
pub(crate) fn enum_member_name_fallback(node: Node<'_>, src: &[u8]) -> Option<String> {
    if let Some(name) = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
    {
        return Some(name.to_string());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if matches!(
            child.kind(),
            "identifier"
                | "type_identifier"
                | "simple_identifier"
                | "property_identifier"
                | "field_identifier"
        ) && let Ok(text) = child.utf8_text(src)
        {
            return Some(text.to_string());
        }
    }
    None
}

pub(crate) fn collect_enum_members(
    node: Node<'_>,
    config: &'static LanguageConfig,
    source: &[u8],
    scope: &ScopeStack,
    out: &mut ExtractionOutput,
) -> Result<(), ExtractionError> {
    if config.enum_member_kinds.contains(&node.kind()) {
        if let Some(chunk) = extract_chunk(node, config, source, scope, ChunkCategory::EnumMember)?
        {
            out.chunks.push(chunk);
        }
        // A member's children are not themselves members — do not descend.
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_enum_members(child, config, source, scope, out)?;
    }
    Ok(())
}
