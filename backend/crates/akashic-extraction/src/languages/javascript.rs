use crate::types::*;
use crate::walker_helpers::{default_name_from_field, resolve_esm_import_names};
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_javascript::LANGUAGE.into()
}

// ─── Express framework-route queries ──────────────────────────────────────────
//
// Matches `app.METHOD(path, handler)` call patterns (Express 4+).
//
// AST shape (tree-sitter-javascript):
//
//   call_expression                          ← @route.reg
//     member_expression
//       <receiver-expression>
//       property_identifier "get"/"post"/…   ← @route.method
//     arguments
//       string                               ← @route.path  (includes quotes)
//       identifier                           ← @route.handler
// ─── React-Router JSX route queries (JavaScript) ─────────────────────────────
//
// Same AST shape as the TypeScript variant (see typescript.rs). The
// tree-sitter-javascript grammar supports JSX natively, so the identical
// query works here. Appended after the Express QueryDef.
//
// ─── Vue Router config-array route queries ────────────────────────────────────
//
// Matches `{ path: '/users', component: Users }` route objects inside a Vue
// Router config array. No HTTP method → runner emits "ANY".
//
// AST shape (tree-sitter-javascript):
//
//   object                                   ← @route.reg
//     pair
//       key: property_identifier "path"
//       value: string                        ← @route.path
//     pair
//       key: property_identifier "component"
//       value: identifier                    ← @route.handler
//
// tree-sitter matches both pairs as named children of the object regardless
// of source order (no anchors), so path-before-component or
// component-before-path both match.
static JS_FRAMEWORK_ROUTES: [QueryDef; 3] = [
    QueryDef {
        name: "express_route",
        source: "(call_expression \
  function: (member_expression \
    property: (property_identifier) @route.method \
    (#match? @route.method \"^(get|post|put|delete|patch|options|head|all)$\")) \
  arguments: (arguments \
    (string) @route.path \
    (identifier) @route.handler)) @route.reg",
        compiled: std::sync::OnceLock::new(),
    },
    QueryDef {
        name: "react_router_route",
        source: "(jsx_self_closing_element \
  name: (identifier) @_n (#eq? @_n \"Route\") \
  (jsx_attribute \
    (property_identifier) @_pa (#eq? @_pa \"path\") \
    (string) @route.path) \
  (jsx_attribute \
    (property_identifier) @_ea (#eq? @_ea \"element\") \
    (jsx_expression \
      (jsx_self_closing_element \
        name: (identifier) @route.handler)))) @route.reg",
        compiled: std::sync::OnceLock::new(),
    },
    QueryDef {
        name: "vue_router_route",
        source: "(object \
  (pair key: (property_identifier) @_pk (#eq? @_pk \"path\") value: (string) @route.path) \
  (pair key: (property_identifier) @_ck (#eq? @_ck \"component\") value: (identifier) @route.handler)) @route.reg",
        compiled: std::sync::OnceLock::new(),
    },
];

static JS_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &JS_FRAMEWORK_ROUTES,
    http_calls: &[],
    extra_chunks: &[],
};

/// Resolve a declaration's name. Most JS declarations expose a `name` field
/// directly, but `lexical_declaration` (const/let) and `variable_declaration`
/// (var) do not — their name lives on the first `variable_declarator` child.
/// This covers `const f = () => {}` / `const f = function () {}` /
/// `var g = ...`, mirroring the TypeScript config's `ts_resolve_name`.
///
/// Destructuring patterns (`const { a, b } = x`, `const [a, b] = x`) yield the
/// raw pattern text as the chunk name; this mirrors the TS config.
fn js_resolve_name(node: Node, src: &[u8]) -> Option<String> {
    if matches!(node.kind(), "lexical_declaration" | "variable_declaration") {
        return node
            .named_child(0)
            .and_then(|d| default_name_from_field(d, src, "name"));
    }
    default_name_from_field(node, src, "name")
}

fn js_is_exported(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(p) = current {
        if p.kind() == "export_statement" {
            return true;
        }
        current = p.parent();
    }
    false
}

fn js_is_async(node: Node) -> bool {
    (0..node.child_count()).any(|i| node.child(i).is_some_and(|c| c.kind() == "async"))
}

fn js_is_static(node: Node) -> bool {
    (0..node.child_count()).any(|i| node.child(i).is_some_and(|c| c.kind() == "static"))
}

/// JavaScript has no access-modifier keywords (no `public`/`private`/
/// `protected`). The only visibility signal is the ES2022 `#private` class
/// member syntax: a `method_definition`/field whose name is a
/// `private_property_identifier` (`#foo`) is private. Everything else defaults
/// to Public (the walker's default for a missing hook is Unknown, but JS
/// declarations are conventionally public, so we surface that explicitly here).
fn js_visibility(node: Node, _src: &[u8]) -> Visibility {
    let is_private = node
        .child_by_field_name("name")
        .is_some_and(|n| n.kind() == "private_property_identifier");
    if is_private {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

fn js_should_skip_call(node: Node) -> bool {
    // Skip computed-property dynamic dispatch: obj[key]()
    node.child_by_field_name("function")
        .is_some_and(|f| f.kind() == "subscript_expression")
}

/// Recurse through `export_statement` (transparent wrapper — most ESM
/// declarations are exported, e.g. `export class Foo {}` / `export function
/// f(){}`, and without descending it the walker would never reach them),
/// `class_declaration`, and `class_body`; descend into the root; do NOT descend
/// into function/method bodies.
///
/// `class_body` is the intermediate member-list node between a
/// `class_declaration` and its `method_definition` children; without descending
/// into it the walker would never reach the methods even though the class
/// itself is recursed into. We deliberately do NOT list `statement_block` here:
/// a function/method body is also a `statement_block`, and recursing into those
/// would extract nested functions as top-level chunks, breaking the
/// "top-level only" extraction contract.
fn js_should_recurse(node: Node) -> bool {
    node.parent().is_none()
        || matches!(
            node.kind(),
            "export_statement" | "class_declaration" | "class_body"
        )
}

fn js_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("source")
        .and_then(|n| n.utf8_text(src).ok())
        .map(|s| {
            s.trim_matches(|c: char| matches!(c, '\'' | '"' | '`'))
                .to_string()
        })
}

/// Per-symbol import binding resolver. JavaScript has no type-only import
/// syntax, so `support_type_only` is `false` (an inline `type` token would be
/// a plain identifier in JS anyway). Delegates to the shared ESM resolver (see
/// [`resolve_esm_import_names`]) shared with the TypeScript config.
fn js_resolve_import_names(node: Node, src: &[u8]) -> Vec<(String, String, ImportSpec)> {
    resolve_esm_import_names(node, src, false)
}

/// Resolve a relative import specifier to a module path. Bare specifiers
/// (npm packages) and asset imports return None.
fn js_resolve_module_path(specifier: &str, current_module: &str) -> Option<String> {
    if !specifier.starts_with('.') {
        return None;
    }
    let skip = [".css", ".scss", ".less", ".svg", ".png", ".jpg", ".json"];
    if skip.iter().any(|e| specifier.ends_with(e)) {
        return None;
    }
    let mut segments: Vec<&str> = current_module
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    segments.pop(); // drop the file stem; relative paths resolve against the dir
    for part in specifier.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                segments.pop();
            }
            other => {
                let clean = other
                    .strip_suffix(".js")
                    .or_else(|| other.strip_suffix(".jsx"))
                    .or_else(|| other.strip_suffix(".mjs"))
                    .or_else(|| other.strip_suffix(".cjs"))
                    .unwrap_or(other);
                segments.push(clean);
            }
        }
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}

/// Dedicated JavaScript config. Split off the TypeScript grammar so `.js`/
/// `.jsx`/`.mjs` are parsed by the JavaScript grammar — correct JS/JSX
/// semantics rather than borrowing the (superset) TS grammar. The node-kinds
/// JS uses (`function_declaration`, `class_declaration`, `method_definition`,
/// `lexical_declaration`, `call_expression`, `member_expression`,
/// `import_statement`/`export_statement`, `statement_block`/`class_body`) are a
/// strict subset of the TS grammar's, so the resolution hooks mirror the TS
/// config verbatim — minus the TS-only constructs (`interface`, `type_alias`,
/// `enum`, namespaces, and `accessibility_modifier` visibility), which JS lacks.
/// FQNs are `.`-joined, matching TS.
pub static JAVASCRIPT: LanguageConfig = LanguageConfig {
    name: "javascript",
    extensions: &[".js", ".jsx", ".mjs"],
    language_fn: language,

    function_kinds: &["function_declaration", "generator_function_declaration"],
    class_kinds: &["class_declaration"],
    method_kinds: &["method_definition"],
    interface_kinds: &[],
    struct_kinds: &[],
    enum_kinds: &[],
    enum_member_kinds: &[],
    type_alias_kinds: &[],
    variable_kinds: &[],
    constant_kinds: &["lexical_declaration"],
    macro_kinds: &[],
    namespace_kinds: &[],
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    import_kinds: &["import_statement", "export_statement"],
    call_kinds: &["call_expression"],
    member_expr_kinds: &["member_expression"],
    type_ref_kinds: &[],

    comment_kinds: &["comment"],
    // JSDoc `/** ... */` (node kind `comment`); the walker's `is_doc_comment`
    // marker predicate excludes ordinary `//` and `/* */` comments.
    doc_comment_kinds: &["comment"],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "",
    import_source_field: "source",
    call_function_field: "function",
    member_property_field: "property",
    receiver_field: "",

    queries: &JS_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(js_resolve_name),
        is_exported: Some(js_is_exported),
        is_async: Some(js_is_async),
        is_static: Some(js_is_static),
        get_visibility: Some(js_visibility),
        should_skip_call: Some(js_should_skip_call),
        should_recurse_into: Some(js_should_recurse),
        resolve_import_specifier: Some(js_resolve_import_specifier),
        resolve_import_names: Some(js_resolve_import_names),
        resolve_module_path: Some(js_resolve_module_path),
        nested_extraction: true,
        ..LanguageHooks::DEFAULT
    },
};
