use crate::types::*;
use crate::walker_helpers::default_name_from_field;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_c_sharp::LANGUAGE.into()
}

static EMPTY_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &[],
    http_calls: &[],
    extra_chunks: &[],
};

/// Recurse into container nodes between namespace / type declarations and
/// their member declarations.
///
/// C# grammar containers we must descend:
///
/// * Root (`parent().is_none()`) — the compilation unit.
/// * `namespace_declaration` — contains a `declaration_list` body.
/// * `file_scoped_namespace_declaration` — C# 10 file-scoped namespace;
///   its member declarations are direct children of this node (no `body`
///   field), so we must descend it.
/// * `class_declaration` / `interface_declaration` / `struct_declaration` /
///   `record_declaration` — the type containers whose `body` field is
///   a `declaration_list`.
/// * `declaration_list` — the braced body node that wraps member declarations
///   inside all of the above types.  This is the C# equivalent of Java's
///   `class_body`.
///
/// We deliberately exclude `block` and `arrow_expression_clause` (method
/// bodies) so that nested types are not surfaced as top-level chunks.
fn csharp_should_recurse(node: Node) -> bool {
    node.parent().is_none()
        || matches!(
            node.kind(),
            "namespace_declaration"
                | "file_scoped_namespace_declaration"
                | "class_declaration"
                | "interface_declaration"
                | "struct_declaration"
                | "record_declaration"
                | "declaration_list"
        )
}

/// Extract the callee name from a C# `invocation_expression` node.
///
/// The `invocation_expression` grammar node has a `function` field that is
/// either:
///
/// * An `identifier` for bare calls (`Foo()`) — text is the callee name.
/// * A `member_access_expression` for receiver calls (`obj.Method()`) — the
///   method name lives in its `name` field.
///
/// We delegate to this hook because the `member_access_expression.name` field
/// (not `property`) differs from the TypeScript / C++ pattern, so the
/// declarative `member_property_field = "name"` cannot be used alongside
/// `call_function_field = "function"` without a hook.
fn csharp_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
    let func = node.child_by_field_name("function")?;
    match func.kind() {
        "member_access_expression" => func
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from),
        _ => func.utf8_text(src).ok().map(String::from),
    }
}

/// Extract visibility from C# declaration `modifier` children.
///
/// C# modifiers are represented as `modifier` named-child nodes (not as
/// unnamed tokens like in Java).  We scan all named children of the
/// declaration node for `modifier` nodes, then read each modifier's text.
///
/// Access levels in C#:
///   `public` → Public
///   `private` → Private
///   `protected` → Protected
///   `internal` → Internal
///   `protected internal` / `private protected` → Protected (approximation)
///
/// Default visibility when no modifier is present varies by context:
///   – Class members default to `private`
///   – Top-level types default to `internal`
///
/// We return `Internal` as the default (safest approximation for top-level
/// types, the most common case where the modifier is absent).
fn csharp_visibility(node: Node, src: &[u8]) -> Visibility {
    // Collect all modifier texts from named children of kind "modifier".
    let mut modifiers: Vec<&str> = Vec::new();
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i)
            && child.kind() == "modifier"
            && let Ok(text) = child.utf8_text(src)
        {
            modifiers.push(text);
        }
    }

    // Check for combined access modifiers first.
    if modifiers.contains(&"public") {
        return Visibility::Public;
    }
    if modifiers.contains(&"private") && modifiers.contains(&"protected") {
        return Visibility::Protected;
    }
    if modifiers.contains(&"protected") {
        return Visibility::Protected;
    }
    if modifiers.contains(&"private") {
        return Visibility::Private;
    }
    if modifiers.contains(&"internal") {
        return Visibility::Internal;
    }

    // No access modifier → internal (C# default for top-level types)
    Visibility::Internal
}

/// Extract the namespace / type name from a C# `using_directive` node.
///
/// C# using directives take several forms:
///   `using System;`
///   `using System.Collections.Generic;`
///   `using static System.Math;`
///   `using SB = System.Text.StringBuilder;`
///
/// The grammar (tree-sitter-c-sharp 0.23) shapes the aliased form as
/// `seq(field('name', $.identifier), '=', $.type)`: the `name` FIELD holds the
/// alias binding (`SB`), and the imported namespace / type is a *separate*
/// unnamed child (a `qualified_name` / `identifier` / `generic_name`, since the
/// `type` supertype is hidden in the concrete tree).  We want the imported
/// namespace, not the alias — so we must skip the `name`-field node.  Both the
/// alias `SB` and the import `System...` are `identifier`/`qualified_name`
/// kinds, and the alias appears first in named-child order, which is exactly
/// why the previous "first matching kind" loop returned the alias.
fn csharp_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    // The alias binding lives in the `name` field for aliased usings; skip it
    // so we resolve the imported namespace/type rather than the local alias.
    let alias_node = node.child_by_field_name("name");

    // We search named children for the namespace name, skipping the alias node
    // and keyword pseudo-nodes ("static", "global").
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            // Skip the alias (name field) for `using Alias = Ns.Type;`.
            if let Some(alias) = alias_node
                && child.id() == alias.id()
            {
                continue;
            }
            let kind = child.kind();
            // The actual namespace / type is a qualified_name or identifier.
            if (kind == "qualified_name"
                || kind == "identifier"
                || kind == "generic_name"
                || kind == "alias_qualified_name")
                && let Ok(text) = child.utf8_text(src)
            {
                return Some(text.to_string());
            }
        }
    }
    // Fallback: read the text of the first named child regardless of kind,
    // still skipping the alias node so we never return the local binding.
    (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| alias_node.is_none_or(|a| a.id() != c.id()))
        .and_then(|n| n.utf8_text(src).ok())
        .map(String::from)
}

/// C# using directives are fully-qualified namespace paths (`System.Linq`);
/// pass them through without rewriting.
fn csharp_resolve_module_path(specifier: &str, _current_module: &str) -> Option<String> {
    Some(specifier.to_string())
}

/// Resolve the FQN for a C# declaration.
///
/// Mirrors the Java FQN resolver: consults the walker's scope stack first,
/// then falls back to an AST climb to find the nearest enclosing type
/// container (useful for members of structs / records / enums whose
/// declarations may not always appear on the scope stack).
fn csharp_resolve_fqn(node: Node, src: &[u8], scope: &ScopeStack) -> Option<String> {
    let name = default_name_from_field(node, src, "name")?;

    if !scope.is_empty() {
        let prefix = scope
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        return Some(format!("{prefix}.{name}"));
    }

    // AST climb: find nearest enclosing type declaration.
    let container_kinds = [
        "class_declaration",
        "interface_declaration",
        "struct_declaration",
        "record_declaration",
        "enum_declaration",
    ];
    let mut parent = node.parent();
    while let Some(p) = parent {
        if container_kinds.contains(&p.kind())
            && let Some(container_name) = p.child_by_field_name("name")
            && let Ok(cn) = container_name.utf8_text(src)
        {
            return Some(format!("{cn}.{name}"));
        }
        parent = p.parent();
    }

    Some(name)
}

/// EXT-6c-2: resolve supertypes from a C# class, interface, or struct via its
/// `base_list` named field.
///
/// Grammar shape (tree-sitter-c-sharp):
///   class_declaration / interface_declaration / struct_declaration
///     base_list                           ← named field
///       ":"
///       identifier / qualified_name*      ← each base type
///
/// C# `base_list` position is type-only: the compiler uses the first entry as
/// the base class (if it IS a class), and the rest as interfaces.  However,
/// the grammar doesn't distinguish them, so we emit ALL as `Implements`
/// (consistent with the task strategy for flat clauses).
///
/// For `qualified_name` (`System.IDisposable`) we take only the trailing
/// segment (the simple name).
fn csharp_resolve_supertypes(node: Node, src: &[u8]) -> Vec<(String, EdgeKind)> {
    let mut out = Vec::new();
    if !matches!(
        node.kind(),
        "class_declaration" | "interface_declaration" | "struct_declaration" | "record_declaration"
    ) {
        return out;
    }

    // Find the base_list: prefer the `bases` field, fall back to named child by kind.
    // Use index-based search to avoid cursor lifetime issues.
    let base_list = node.child_by_field_name("bases").or_else(|| {
        (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "base_list")
    });
    let base_list = match base_list {
        Some(b) => b,
        None => return out,
    };

    for ci in 0..base_list.named_child_count() {
        let child = match base_list.named_child(ci) {
            Some(c) => c,
            None => continue,
        };
        match child.kind() {
            "identifier" => {
                if let Ok(text) = child.utf8_text(src) {
                    let bare = text.trim();
                    if !bare.is_empty() {
                        out.push((bare.to_string(), EdgeKind::Implements));
                    }
                }
            }
            "qualified_name" => {
                // Take the trailing simple name.
                if let Ok(text) = child.utf8_text(src) {
                    let bare = text.trim().rsplit('.').next().unwrap_or("").trim();
                    if !bare.is_empty() {
                        out.push((bare.to_string(), EdgeKind::Implements));
                    }
                }
            }
            "generic_name" => {
                // `List<T>` → `List`; the name field is an identifier.
                if let Some(name_node) = child.child_by_field_name("name")
                    && let Ok(text) = name_node.utf8_text(src)
                {
                    let bare = text.trim();
                    if !bare.is_empty() {
                        out.push((bare.to_string(), EdgeKind::Implements));
                    }
                }
            }
            _ => {}
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────
// EXT-6b-3: position-aware type-reference extraction for C#
// ─────────────────────────────────────────────────────────────

/// C# chunk boundary node kinds: descending into these would cross into a
/// nested chunk, so the type_refs walk stops at them (each nested chunk runs
/// its own type_pass).
const CSHARP_CHUNK_KINDS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "struct_declaration",
    "record_declaration",
    "method_declaration",
    "constructor_declaration",
    "enum_declaration",
];

/// Resolve the short name (and byte span) from a C# type node, recursing
/// generic arguments as needed. Appends results to `out`.
fn resolve_type_node(node: tree_sitter::Node, src: &[u8], out: &mut Vec<(String, usize, usize)>) {
    match node.kind() {
        "identifier" => {
            if let Ok(text) = node.utf8_text(src) {
                let s = text.trim();
                if !s.is_empty() {
                    out.push((s.to_string(), node.start_byte(), node.end_byte()));
                }
            }
        }
        "generic_name" => {
            // generic_name: first named child is the identifier (the base type name)
            if let Some(id_node) = node.named_child(0)
                && let Ok(text) = id_node.utf8_text(src)
            {
                let s = text.trim();
                if !s.is_empty() {
                    out.push((s.to_string(), id_node.start_byte(), id_node.end_byte()));
                }
            }
            // Recurse into type_argument_list for generic args
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i)
                    && child.kind() == "type_argument_list"
                {
                    let mut cursor = child.walk();
                    for arg in child.named_children(&mut cursor) {
                        resolve_type_node(arg, src, out);
                    }
                }
            }
        }
        "qualified_name" => {
            // Take the trailing simple name segment only
            if let Ok(text) = node.utf8_text(src) {
                let bare = text.trim().rsplit('.').next().unwrap_or("").trim();
                if !bare.is_empty() {
                    out.push((bare.to_string(), node.start_byte(), node.end_byte()));
                }
            }
        }
        "predefined_type" => {
            // e.g. `int`, `string`, `void` — include for completeness; resolution
            // will simply not find a matching chunk, which is harmless.
            if let Ok(text) = node.utf8_text(src) {
                let s = text.trim();
                if !s.is_empty() {
                    out.push((s.to_string(), node.start_byte(), node.end_byte()));
                }
            }
        }
        "nullable_type" => {
            // T? → recurse into element type (first named child)
            if let Some(inner) = node.named_child(0) {
                resolve_type_node(inner, src, out);
            }
        }
        "array_type" => {
            // T[] → recurse into element type (first named child)
            if let Some(inner) = node.named_child(0) {
                resolve_type_node(inner, src, out);
            }
        }
        _ => {}
    }
}

/// Walk the subtree rooted at `node`, extracting type references from TYPE
/// positions only. Stops descent at nested chunk-kind boundaries. Called by
/// the walker's `type_pass` via `config.hooks.resolve_type_refs`.
fn csharp_collect_type_refs(
    node: tree_sitter::Node,
    src: &[u8],
    out: &mut Vec<(String, usize, usize)>,
    is_root: bool,
) {
    let kind = node.kind();

    // Stop at nested chunk boundaries (each nested chunk runs its own pass).
    // We still process the root node itself, but stop its CHILDREN if they
    // are a different chunk kind.
    if !is_root && CSHARP_CHUNK_KINDS.contains(&kind) {
        return;
    }

    match kind {
        "method_declaration" => {
            // .returns field = return type
            if let Some(ret) = node.child_by_field_name("returns") {
                resolve_type_node(ret, src, out);
            }
            // .parameters field = parameter_list; recurse into parameters
            if let Some(params) = node.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for param in params.named_children(&mut cursor) {
                    csharp_collect_type_refs(param, src, out, false);
                }
            }
            // body — recurse for object_creation_expression inside
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    csharp_collect_type_refs(child, src, out, false);
                }
            }
        }
        "constructor_declaration" => {
            // .parameters field only (no return type)
            if let Some(params) = node.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for param in params.named_children(&mut cursor) {
                    csharp_collect_type_refs(param, src, out, false);
                }
            }
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    csharp_collect_type_refs(child, src, out, false);
                }
            }
        }
        "parameter" => {
            // .type field only — NOT the .name field
            if let Some(type_node) = node.child_by_field_name("type") {
                resolve_type_node(type_node, src, out);
            }
        }
        "field_declaration" => {
            // field_declaration contains a variable_declaration; recurse into it
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                csharp_collect_type_refs(child, src, out, false);
            }
        }
        "variable_declaration" => {
            // .type field
            if let Some(type_node) = node.child_by_field_name("type") {
                // Skip `var` (implicit_type)
                if type_node.kind() != "implicit_type" {
                    resolve_type_node(type_node, src, out);
                }
            }
        }
        "property_declaration" => {
            // .type field
            if let Some(type_node) = node.child_by_field_name("type") {
                resolve_type_node(type_node, src, out);
            }
        }
        "object_creation_expression" => {
            // .type field
            if let Some(type_node) = node.child_by_field_name("type") {
                resolve_type_node(type_node, src, out);
            }
        }
        "base_list" => {
            // Each named child is a base type: identifier / qualified_name / generic_name
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                resolve_type_node(child, src, out);
            }
        }
        "class_declaration"
        | "interface_declaration"
        | "struct_declaration"
        | "record_declaration"
            if is_root =>
        {
            // For root chunk node: emit base_list types and recurse body members
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                csharp_collect_type_refs(child, src, out, false);
            }
        }
        _ => {
            // Generic descent: look for interesting nodes inside
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                csharp_collect_type_refs(child, src, out, false);
            }
        }
    }
}

/// EXT-6b-3 entry point: position-aware type-reference resolver for C#.
/// Called by the walker's `type_pass` when `resolve_type_refs` is `Some`.
fn csharp_resolve_type_refs(node: tree_sitter::Node, src: &[u8]) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    csharp_collect_type_refs(node, src, &mut out, true);
    out
}

pub static CSHARP: LanguageConfig = LanguageConfig {
    name: "csharp",
    extensions: &[".cs"],
    language_fn: language,

    // C# has no free-standing functions; all code lives inside type members.
    function_kinds: &[],
    class_kinds: &["class_declaration"],
    method_kinds: &["method_declaration", "constructor_declaration"],
    interface_kinds: &["interface_declaration"],
    struct_kinds: &["struct_declaration", "record_declaration"],
    enum_kinds: &["enum_declaration"],
    enum_member_kinds: &["enum_member_declaration"],
    type_alias_kinds: &[],
    variable_kinds: &[],
    constant_kinds: &[],
    macro_kinds: &[],
    // We don't emit namespace chunks; they are transparent scope containers.
    namespace_kinds: &[],
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    import_kinds: &["using_directive"],
    // invocation_expression covers both method calls and constructor calls.
    call_kinds: &["invocation_expression"],
    member_expr_kinds: &["member_access_expression"],
    type_ref_kinds: &[],

    comment_kinds: &["single_line_comment", "multiline_comment"],
    // XML doc `/// ...` — tree-sitter-c-sharp emits these as `comment` nodes
    // (NOT the single_line_comment/multiline_comment kinds used above); the
    // walker's `is_doc_comment` predicate keeps only the `///`/`/**` markers.
    doc_comment_kinds: &["comment"],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    // C# method_declaration uses `returns` for the return type.
    return_field: "returns",
    import_source_field: "",
    // invocation_expression has a `function` field — used by the
    // csharp_resolve_call_name hook (not the default_callee_name path).
    call_function_field: "function",
    // member_access_expression.name is the method identifier.
    member_property_field: "name",
    receiver_field: "",

    queries: &EMPTY_QUERIES,
    hooks: LanguageHooks {
        resolve_fqn: Some(csharp_resolve_fqn),
        get_visibility: Some(csharp_visibility),
        should_recurse_into: Some(csharp_should_recurse),
        resolve_import_specifier: Some(csharp_resolve_import_specifier),
        resolve_call_name: Some(csharp_resolve_call_name),
        resolve_module_path: Some(csharp_resolve_module_path),
        resolve_supertypes: Some(csharp_resolve_supertypes),
        resolve_type_refs: Some(csharp_resolve_type_refs),
        ..LanguageHooks::DEFAULT
    },
};
