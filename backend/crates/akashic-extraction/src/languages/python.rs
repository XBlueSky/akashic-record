use crate::types::*;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}

// ─── FastAPI / Flask framework-route queries ───────────────────────────────
//
// Matches `@app.get("/path")` / `@router.post("/path")` decorator patterns.
//
// AST shape (tree-sitter-python):
//
//   decorated_definition                           ← @route.reg
//     decorator
//       call
//         attribute  field=function
//           identifier field=object  (e.g. "app")
//           identifier field=attribute (e.g. "get")  ← @route.method
//         argument_list field=arguments
//           string                                 ← @route.path
//     function_definition  field=definition
//       identifier  field=name                     ← @route.handler
//
// @route.method captures the HTTP verb attribute (e.g. "get", "post").
// The runner upper-cases it. Flask's @app.route(...) has method="ROUTE".
// We accept any attribute-style decorator with a string first argument.
// A #match? predicate filters to known HTTP verbs + "route" (Flask generic).
static PYTHON_FRAMEWORK_ROUTES: [QueryDef; 1] = [QueryDef {
    name: "fastapi_flask_route",
    source: "(decorated_definition \
  (decorator \
    (call \
      function: (attribute \
        attribute: (identifier) @route.method \
        (#match? @route.method \"^(get|post|put|delete|patch|head|options|route)$\")) \
      arguments: (argument_list \
        (string) @route.path))) \
  definition: (function_definition \
    name: (identifier) @route.handler)) @route.reg",
    compiled: std::sync::OnceLock::new(),
}];

static PYTHON_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &PYTHON_FRAMEWORK_ROUTES,
    http_calls: &[],
    extra_chunks: &[],
};

/// Return true when a `function_definition` is declared `async`.
///
/// In tree-sitter-python the `async` keyword is a direct unnamed child
/// (field=(none), kind="async") of `function_definition` at position 0.
/// The sexp omits unnamed nodes, so it doesn't appear in the s-expression
/// but is present in the concrete syntax tree.
fn py_is_async(node: Node) -> bool {
    (0..node.child_count()).any(|i| node.child(i).is_some_and(|c| c.kind() == "async"))
}

/// Extract the canonical module path from an import node.
///
/// - `import_statement`: reads the `name` field.  For `import os` that is a
///   `dotted_name` → returns "os".  For `import sys as system` the `name`
///   field is an `aliased_import` whose first named child is the `dotted_name`
///   → returns "sys".
/// - `import_from_statement`: reads the `module_name` field.  For
///   `from collections import foo` it is a `dotted_name` → "collections".
///   For `from .local_module import foo` it is a `relative_import` whose
///   full text (e.g. ".local_module") is returned as-is; the
///   `py_resolve_module_path` hook then resolves the leading dots.
fn py_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "import_statement" => {
            // `import x` / `import x as y`
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    match c.kind() {
                        "dotted_name" => return c.utf8_text(src).ok().map(String::from),
                        "aliased_import" => {
                            return c
                                .named_child(0)
                                .and_then(|n| n.utf8_text(src).ok())
                                .map(String::from);
                        }
                        _ => {}
                    }
                }
            }
            None
        }
        "import_from_statement" => {
            // `from MODULE import names` — module_name field may be a
            // dotted_name or a relative_import (".foo", "..bar").
            node.child_by_field_name("module_name")
                .and_then(|m| m.utf8_text(src).ok())
                .map(String::from)
        }
        _ => None,
    }
}

/// Resolve a Python import specifier to a file-system-style module path.
///
/// Absolute imports (`collections`, `typing.Optional`) are converted by
/// replacing `.` separators with `/`.
///
/// Relative imports (leading `.` or `..`) are resolved against
/// `current_module` (the file's own module path, e.g. "pkg/sub/mod"):
///   - count the leading dots (= number of parent hops)
///   - strip that many trailing path segments from `current_module`
///   - append the rest of the specifier (dots replaced by `/`)
fn py_resolve_module_path(specifier: &str, current_module: &str) -> Option<String> {
    if specifier.starts_with('.') {
        let dots = specifier.chars().take_while(|c| *c == '.').count();
        let rest = &specifier[dots..];
        let mut segments: Vec<&str> = current_module
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        for _ in 0..dots {
            segments.pop();
        }
        for part in rest.split('.') {
            if !part.is_empty() {
                segments.push(part);
            }
        }
        return Some(segments.join("/"));
    }
    Some(specifier.replace('.', "/"))
}

/// Recursion guard for the Python walker.
///
/// Python uses `block` for both class bodies and function bodies — the node
/// kind alone is ambiguous.  The rules:
///
/// - `class_definition` → recurse (contains method definitions)
/// - `decorated_definition` → recurse (transparent wrapper; the inner
///   `function_definition` or `class_definition` is what we actually extract)
/// - `block` → recurse ONLY when its immediate parent is `class_definition`
///   (the class body).  A `block` under `function_definition` is a function
///   body — we do NOT enter it, so nested functions don't leak as chunks.
/// - root (no parent) → always recurse
/// - everything else → do NOT recurse (leaf or non-scope node)
fn python_should_recurse(node: Node) -> bool {
    if node.parent().is_none() {
        return true;
    }
    match node.kind() {
        "class_definition" => true,
        "decorated_definition" => true,
        "block" => node
            .parent()
            .is_some_and(|p| p.kind() == "class_definition"),
        _ => false,
    }
}

/// Per-symbol import binding resolver for Python.
///
/// Only `from MODULE import <symbol>[, <symbol>][ as <alias>]`
/// (`import_from_statement`) yields per-symbol names, because each imported
/// symbol is a real definition in MODULE that a call site references directly
/// (`helper()`), so it can resolve to the definition chunk (tier-1 1.0). The
/// LOCAL binding is what the call site uses: for `from x import a` it is `a`
/// (the `dotted_name` leaf), for `from x import a as c` it is the alias `c`.
/// All are emitted as [`ImportSpec::Named`].
///
/// The following forms return `[]` so the walker falls back to a single
/// module-level edge:
///
/// - `from x import *` (`wildcard_import`): no specific symbol is bound, so
///   there is nothing to resolve per-symbol (would be [`ImportSpec::Glob`]).
/// - `import x` / `import x.y` (`import_statement` → `dotted_name`): binds the
///   MODULE object `x`, and call sites are `x.foo()` — `x` is not itself a
///   callable definition, so emitting it as a symbol would never match a
///   definition chunk. Module-level edge is the honest representation.
/// - `import x as z` (`import_statement` → `aliased_import`): same — `z` is a
///   module-object alias, not a definition symbol.
///
/// So per-symbol precision applies to `from … import` only; plain `import`,
/// `import … as`, and wildcard stay module-level. Aliased from-imports
/// (`a as c`, local `c` != source `a`) emit the LOCAL name `c` with the SOURCE
/// name `a` (the `name` field of `aliased_import`); the walker records the
/// source so tier-1 resolution bridges `c()` to `a`'s definition chunk.
fn py_resolve_import_names(node: Node, src: &[u8]) -> Vec<(String, String, ImportSpec)> {
    if node.kind() != "import_from_statement" {
        // `import x` / `import x as y` bind module objects, not symbols.
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.children_by_field_name("name", &mut cursor) {
        match child.kind() {
            // `from x import a` / `from x import a, b` — each symbol is a
            // `dotted_name`; its text is the local binding (== source name).
            "dotted_name" => {
                if let Ok(name) = child.utf8_text(src) {
                    out.push((name.to_string(), name.to_string(), ImportSpec::Named));
                }
            }
            // `from x import a as c` — local name is the `alias` field, source
            // name is the `name` field (the symbol defined in MODULE).
            "aliased_import" => {
                if let Some(alias) = child
                    .child_by_field_name("alias")
                    .and_then(|n| n.utf8_text(src).ok())
                {
                    let source = child
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(src).ok())
                        .map_or_else(|| alias.to_string(), String::from);
                    out.push((alias.to_string(), source, ImportSpec::Named));
                }
            }
            _ => {}
        }
    }
    // `from x import *` has a `wildcard_import` child (not under the `name`
    // field), so `out` stays empty → module-level Glob edge.
    out
}

/// Resolve the bare type name from a Python `type` annotation node.
///
/// Python annotation wrapper nodes (`type`) contain the full annotation text,
/// e.g. `Widget`, `List[Foo]`, `Optional[str]`, `pkg.Mod`. This hook:
///
/// 1. Strips a `[…]` generic suffix: `List[Foo]` → `List`.
/// 2. Drops any leading dotted module prefix: `pkg.Mod` → `Mod`.
///
/// Inner generic arguments (e.g. `Foo` in `List[Foo]`) appear as their own
/// nested `type` nodes inside the subscript child; `type_pass_walk` recurses
/// into them automatically, so each is resolved independently via this hook.
fn python_resolve_type_name(node: tree_sitter::Node, src: &[u8]) -> Option<String> {
    let t = node.utf8_text(src).ok()?;
    let head = t.split('[').next().unwrap_or(t).trim();
    let leaf = head.rsplit('.').next().unwrap_or(head).trim();
    // Belt-and-braces: a PEP 604 union annotation (`A | B`, `X | None`) reaches
    // here as the composite text "A | B". Its real operands are emitted by
    // `python_expand_type_ref`; never emit the multi-token composite as a single
    // garbage type name. Any residual whitespace or pipe ⇒ decline.
    if leaf.is_empty() || leaf.contains('|') || leaf.contains(char::is_whitespace) {
        None
    } else {
        Some(leaf.to_string())
    }
}

/// EXT-6b-4: expand a Python PEP 604 union annotation into its operand types.
///
/// Python parses a union annotation into one of TWO shapes depending on whether
/// the operands are bare or generic:
///
/// * Bare operands → `(type (binary_operator left: A right: B))`. The operands
///   are bare nodes (`identifier`, `attribute`, …) that the generic `type_pass`
///   recursion never emits (they are not nested `type` nodes), so this hook
///   flattens the `binary_operator` tree and emits each leaf operand directly.
///
/// * Any generic operand → `(type (union_type (type (generic_type …)) (type B)))`.
///   Here each operand is wrapped in its OWN nested `type` node (a `generic_type`
///   such as `List[A]` resolves to head + arg exactly like a standalone
///   `List[A]` annotation). We recurse each child `type` node through the same
///   per-operand expansion/resolution so `List[A]` yields `List` + `A`, `B`
///   yields `B`, and a `None`/`none` operand is dropped (matching `Optional[X]`).
///
/// In BOTH shapes the composite text (e.g. "List[A] | B") is NOT a real type
/// name. Returning a non-empty Vec REPLACES the single-name fall-through (so the
/// generic head is not double-emitted) AND signals the walker to NOT recurse
/// into the node's subtree (so the nested operand `type` nodes are not re-
/// emitted). Returns `[]` for any non-union `type` node so the walker falls back
/// to the single-name `python_resolve_type_name` path (generics/dotted/nested
/// unchanged).
fn python_expand_type_ref(node: tree_sitter::Node, src: &[u8]) -> Vec<(String, usize, usize)> {
    let Some(inner) = node.named_child(0) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match inner.kind() {
        // Bare-operand union: `A | B`.
        "binary_operator" => collect_union_operands(inner, src, &mut out),
        // Generic-bearing union: `List[A] | B`, `Optional[C] | None`. Operands
        // are nested `type` nodes; expand each as a standalone annotation.
        "union_type" => collect_union_type_operands(inner, src, &mut out),
        _ => {}
    }
    out
}

/// Recursively flatten a PEP 604 union `binary_operator` (`A | B | C`) into its
/// leaf operand type names + spans. Nested `binary_operator`s are descended;
/// `none` operands are dropped.
fn collect_union_operands(
    node: tree_sitter::Node,
    src: &[u8],
    out: &mut Vec<(String, usize, usize)>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "binary_operator" => collect_union_operands(child, src, out),
            // The `None` builtin operand — drop it (like dropping `Optional`'s None).
            "none" => {}
            // `identifier` (bare `A`), `attribute` (`pkg.Mod`), `subscript`
            // (`List[T]`) and similar all carry their text; reuse the single-name
            // resolver's stripping rules (generics + dotted prefix) for the leaf.
            _ => {
                if let Some(name) = python_resolve_type_name(child, src) {
                    out.push((name, child.start_byte(), child.end_byte()));
                }
            }
        }
    }
}

/// Flatten a `union_type` whose operands are each wrapped in a nested `type`
/// node (the shape Python produces when ANY operand is a generic, e.g.
/// `List[A] | B`). Each child `type` is expanded exactly as a standalone
/// annotation: a `generic_type` child yields head + each arg (so `List[A]` →
/// `List`, `A`), a bare child yields its single resolved name, and a `None` /
/// `none` operand is dropped. Nested `union_type`s are descended.
fn collect_union_type_operands(
    node: tree_sitter::Node,
    src: &[u8],
    out: &mut Vec<(String, usize, usize)>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "union_type" => collect_union_type_operands(child, src, out),
            // A `type` operand: drop a `None`/`none` literal; otherwise expand
            // its head + any generic args exactly like a standalone annotation.
            "type" => expand_type_operand(child, src, out),
            // Defensive: a bare `none` directly under a union_type → drop.
            "none" => {}
            // Any other direct child (rare) → resolve as a single name.
            _ => {
                if let Some(name) = python_resolve_type_name(child, src) {
                    out.push((name, child.start_byte(), child.end_byte()));
                }
            }
        }
    }
}

/// Expand a single `type` operand of a generic-bearing union into its refs,
/// mirroring what the walker would emit for that annotation standalone:
/// * `(type (none))`         → dropped (the `None` union operand).
/// * `(type (generic_type))` → head name + each generic argument's name.
/// * anything else           → its single resolved name.
fn expand_type_operand(
    type_node: tree_sitter::Node,
    src: &[u8],
    out: &mut Vec<(String, usize, usize)>,
) {
    let Some(inner) = type_node.named_child(0) else {
        return;
    };
    match inner.kind() {
        // The `None` operand of the union — drop it.
        "none" => {}
        "generic_type" => {
            // Emit the generic head (`List` of `List[A]`).
            if let Some(name) = python_resolve_type_name(type_node, src) {
                out.push((name, type_node.start_byte(), type_node.end_byte()));
            }
            // Emit each generic argument, which itself parses as a nested `type`
            // node (`[A]` → `(type_parameter (type A))`). Recurse so a generic
            // argument that is ALSO generic (`List[Dict[K, V]]`) expands fully.
            collect_generic_arg_refs(inner, src, out);
        }
        // Bare / dotted / subscript operand → single resolved name.
        _ => {
            if let Some(name) = python_resolve_type_name(type_node, src) {
                out.push((name, type_node.start_byte(), type_node.end_byte()));
            }
        }
    }
}

/// Recursively collect the type-ref names of every nested `type` node beneath a
/// `generic_type` (its arguments), expanding any nested `generic_type` heads in
/// turn. `none` arguments are dropped.
fn collect_generic_arg_refs(
    node: tree_sitter::Node,
    src: &[u8],
    out: &mut Vec<(String, usize, usize)>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "type" {
            expand_type_operand(child, src, out);
        } else {
            // Descend through `type_parameter` and similar wrapper nodes.
            collect_generic_arg_refs(child, src, out);
        }
    }
}

pub static PYTHON: LanguageConfig = LanguageConfig {
    name: "python",
    extensions: &[".py", ".pyi"],
    language_fn: language,

    function_kinds: &["function_definition"],
    class_kinds: &["class_definition"],
    method_kinds: &[],
    interface_kinds: &[],
    struct_kinds: &[],
    enum_kinds: &[],
    enum_member_kinds: &[],
    type_alias_kinds: &[],
    variable_kinds: &[],
    constant_kinds: &[],
    macro_kinds: &[],
    namespace_kinds: &[],
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    import_kinds: &["import_statement", "import_from_statement"],
    call_kinds: &["call"],
    member_expr_kinds: &["attribute"],
    type_ref_kinds: &["type"],

    comment_kinds: &["comment"],
    doc_comment_kinds: &[],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "return_type",
    import_source_field: "module_name",
    call_function_field: "function",
    member_property_field: "attribute",
    receiver_field: "",

    queries: &PYTHON_QUERIES,
    hooks: LanguageHooks {
        is_async: Some(py_is_async),
        resolve_import_specifier: Some(py_resolve_import_specifier),
        resolve_import_names: Some(py_resolve_import_names),
        resolve_module_path: Some(py_resolve_module_path),
        should_recurse_into: Some(python_should_recurse),
        resolve_type_name: Some(python_resolve_type_name),
        expand_type_ref: Some(python_expand_type_ref),
        // EXT-5: chunk NAMED nested `def`s as their own chunks (parent_fqn = the
        // enclosing function); calls attribute to the innermost function.
        nested_extraction: true,
        ..LanguageHooks::DEFAULT
    },
};
