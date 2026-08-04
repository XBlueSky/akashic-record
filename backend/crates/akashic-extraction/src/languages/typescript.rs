use crate::types::*;
use crate::walker_helpers::{default_name_from_field, resolve_esm_import_names};
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

fn language_tsx() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}

// ─── Express framework-route queries ──────────────────────────────────────────
//
// Matches `app.METHOD(path, handler)` call patterns (Express 4+).
//
// AST shape (tree-sitter-typescript, identical to tree-sitter-javascript):
//
//   call_expression                          ← @route.reg
//     member_expression
//       <receiver-expression>
//       property_identifier "get"/"post"/…   ← @route.method
//     arguments
//       string                               ← @route.path  (includes quotes)
//       identifier                           ← @route.handler
//
// ─── NestJS decorator-based route queries ─────────────────────────────────────
//
// Matches `@Get('/path') findAll() { ... }` on a `method_definition` inside a
// `class_body`. In tree-sitter-typescript (0.23) the decorator is a SIBLING of
// the method_definition — both are direct children of `class_body` — so the
// query matches a `decorator` IMMEDIATELY followed by a `method_definition`.
//
// The `.` anchor between the decorator and the method_definition is LOAD-BEARING:
// without it, tree-sitter matches every (decorator, method_definition) PAIR in
// the class body (an N×M cross-product), producing phantom routes that pair a
// decorator's method+path with an unrelated method's name. The anchor binds each
// decorator to the method it immediately precedes (its own method).
//
// AST shape (tree-sitter-typescript):
//
//   class_body
//     decorator                              (child of class_body)
//       call_expression
//         identifier  "Get"/"Post"/…         ← @route.method
//         arguments
//           string                           ← @route.path  (includes quotes)
//     method_definition                      ← @route.reg
//       property_identifier "findAll"        ← @route.handler
//
// @route.method: the decorator identifier (e.g. "Get"). Uppercased by runner
//   → "GET". A #match? predicate restricts to HTTP-verb decorators only.
// @route.path: string inside the decorator call arguments.
// @route.handler: the method name (property_identifier).
// @route.reg: the full method_definition node.
// ─── Vue Router config-array route queries ────────────────────────────────────
//
// Matches `{ path: '/users', component: Users }` route objects inside a Vue
// Router config array. No HTTP method → runner emits "ANY".
//
// AST shape (tree-sitter-typescript, identical to tree-sitter-javascript):
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
// of source order.
static TS_FRAMEWORK_ROUTES: [QueryDef; 3] = [
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
    // KNOWN LIMITATION: the `.` adjacency anchor binds the route decorator to
    // the method_definition IMMEDIATELY following it. When a NON-route decorator
    // is interleaved between the route decorator and its method
    // (`@Get('/a') @Injectable() ma()`), the route decorator is no longer
    // adjacent to `ma`, so the route is DROPPED. This is accepted by design: the
    // alternative — removing the anchor — re-introduces the decorator×method
    // cross-product (phantom routes pairing a decorator with an unrelated
    // method), which is strictly worse than a rare missed route.
    QueryDef {
        name: "nestjs_route",
        source: "(class_body \
  (decorator \
    (call_expression \
      function: (identifier) @route.method \
      (#match? @route.method \"^(Get|Post|Put|Delete|Patch|All|Options|Head)$\") \
      arguments: (arguments \
        (string) @route.path))) \
  . \
  (method_definition \
    name: (property_identifier) @route.handler) @route.reg)",
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

static TS_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &TS_FRAMEWORK_ROUTES,
    http_calls: &[],
    extra_chunks: &[],
};

/// Resolve a declaration's name. Most TS declarations expose a `name` field
/// directly, but `lexical_declaration` (const/let) does not — its name lives on
/// the first `variable_declarator` child. This mirrors the pre-refactor parser
/// (`named_child(0).child_by_field_name("name")`).
///
/// Destructuring patterns (`const { a, b } = x`, `const [a, b] = x`) yield the
/// raw pattern text as the chunk name (e.g. "{a,b}") — this mirrors the
/// pre-refactor parser; filtering such names is out of scope for EXT-1.
fn ts_resolve_name(node: Node, src: &[u8]) -> Option<String> {
    if node.kind() == "lexical_declaration" {
        return node
            .named_child(0)
            .and_then(|d| default_name_from_field(d, src, "name"));
    }
    default_name_from_field(node, src, "name")
}

fn ts_is_exported(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(p) = current {
        if p.kind() == "export_statement" {
            return true;
        }
        current = p.parent();
    }
    false
}

fn ts_is_async(node: Node) -> bool {
    (0..node.child_count()).any(|i| node.child(i).is_some_and(|c| c.kind() == "async"))
}

fn ts_is_static(node: Node) -> bool {
    (0..node.child_count()).any(|i| node.child(i).is_some_and(|c| c.kind() == "static"))
}

fn ts_visibility(node: Node, src: &[u8]) -> Visibility {
    for i in 0..node.child_count() {
        if let Some(c) = node.child(i)
            && c.kind() == "accessibility_modifier"
        {
            return match c.utf8_text(src).unwrap_or("") {
                "public" => Visibility::Public,
                "private" => Visibility::Private,
                "protected" => Visibility::Protected,
                _ => Visibility::Unknown,
            };
        }
    }
    Visibility::Unknown
}

fn ts_should_skip_call(node: Node) -> bool {
    // Skip computed-property dynamic dispatch: obj[key]()
    node.child_by_field_name("function")
        .is_some_and(|f| f.kind() == "subscript_expression")
}

/// Recurse through `export_statement` (transparent wrapper), class/namespace
/// containers, and `class_body`; descend into the root; do NOT descend into
/// function/method bodies (so chunk extraction stays top-level; call-pass
/// handles interiors).
///
/// `class_body` is the intermediate member-list node between a
/// `class_declaration` and its `method_definition` children; without descending
/// into it the walker would never reach the methods even though the class
/// itself is recursed into. We deliberately do NOT list `statement_block` here:
/// a function/method body is also a `statement_block`, and recursing into those
/// would extract nested functions as top-level chunks, breaking the
/// "top-level only" extraction contract.
fn ts_should_recurse(node: Node) -> bool {
    node.parent().is_none()
        || matches!(
            node.kind(),
            "export_statement"
                | "class_declaration"
                | "abstract_class_declaration"
                | "internal_module"
                | "module"
                | "class_body"
        )
}

fn ts_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("source")
        .and_then(|n| n.utf8_text(src).ok())
        .map(|s| {
            s.trim_matches(|c: char| matches!(c, '\'' | '"' | '`'))
                .to_string()
        })
}

/// Per-symbol import binding resolver. TypeScript supports type-only imports
/// (`import type { T }` and inline `import { type T }`), so `support_type_only`
/// is `true`. Delegates to the shared ESM resolver (see
/// [`resolve_esm_import_names`]); see that fn for the full clause-form table.
fn ts_resolve_import_names(node: Node, src: &[u8]) -> Vec<(String, String, ImportSpec)> {
    resolve_esm_import_names(node, src, true)
}

/// Resolve a relative import specifier to a module path. Bare specifiers
/// (npm packages) and asset imports return None.
fn ts_resolve_module_path(specifier: &str, current_module: &str) -> Option<String> {
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
                    .strip_suffix(".ts")
                    .or_else(|| other.strip_suffix(".tsx"))
                    .or_else(|| other.strip_suffix(".js"))
                    .or_else(|| other.strip_suffix(".jsx"))
                    .or_else(|| other.strip_suffix(".mjs"))
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

/// EXT-6c: resolve a TypeScript class or interface's explicit supertypes.
///
/// For `class_declaration` / `abstract_class_declaration`:
///   Walk unnamed children looking for a `class_heritage` node; inside it:
///   * `extends_clause`    → identifier/expression leaf → (name, Extends)
///   * `implements_clause` → each `type_identifier`     → (name, Implements)
///
/// For `interface_declaration`:
///   Look for `extends_type_clause` among named children →
///   each `type_identifier` leaf → (name, Extends).
///
/// Generic suffixes (`Foo<T>`) are stripped to the bare name.
fn ts_resolve_supertypes(node: Node, src: &[u8]) -> Vec<(String, EdgeKind)> {
    let mut out = Vec::new();
    match node.kind() {
        "class_declaration" | "abstract_class_declaration" => {
            // class_heritage is an unnamed child of class_declaration
            let heritage = (0..node.child_count())
                .filter_map(|i| node.child(i))
                .find(|c| c.kind() == "class_heritage");
            let Some(heritage) = heritage else {
                return out;
            };
            // Walk named children of class_heritage: extends_clause, implements_clause
            let mut cursor = heritage.walk();
            for clause in heritage.named_children(&mut cursor) {
                match clause.kind() {
                    "extends_clause" => {
                        // extends_clause has a `value` field or is a plain identifier/expr.
                        // Walk named children to find the type leaf.
                        if let Some(name) = extract_ts_type_name(clause, src) {
                            out.push((name, EdgeKind::Extends));
                        }
                    }
                    "implements_clause" => {
                        // implements_clause → each type_identifier (or generic_type)
                        let mut ic = clause.walk();
                        for ty in clause.named_children(&mut ic) {
                            if let Some(name) = ts_type_leaf_name(ty, src) {
                                out.push((name, EdgeKind::Implements));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        "interface_declaration" => {
            // extends_type_clause is a named child of interface_declaration
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "extends_type_clause" {
                    let mut ec = child.walk();
                    for ty in child.named_children(&mut ec) {
                        if let Some(name) = ts_type_leaf_name(ty, src) {
                            out.push((name, EdgeKind::Extends));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// Extract the first type name from an `extends_clause` node.
///
/// In tree-sitter-typescript the extends clause has a `value` field or its
/// first named child is an identifier / expression.
fn extract_ts_type_name(node: Node, src: &[u8]) -> Option<String> {
    // Try `value` field first (some grammar versions use this for extends_clause).
    if let Some(val) = node.child_by_field_name("value") {
        return ts_type_leaf_name(val, src);
    }
    // Fall back: first named child.
    let child = node.named_child(0)?;
    ts_type_leaf_name(child, src)
}

/// Extract the bare type name from a type node (identifier, type_identifier,
/// generic_type, member_expression, etc.).  Generic suffixes stripped.
fn ts_type_leaf_name(node: Node, src: &[u8]) -> Option<String> {
    let text = match node.kind() {
        "type_identifier" | "identifier" => node.utf8_text(src).ok()?.to_string(),
        // generic_type: first named child is the base name.
        "generic_type" => node
            .named_child(0)
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from)?,
        // member_expression: property field or full text before `<`.
        "member_expression" => node.utf8_text(src).ok()?.to_string(),
        _ => node.utf8_text(src).ok()?.to_string(),
    };
    let bare = if let Some(pos) = text.find('<') {
        text[..pos].trim().to_string()
    } else {
        text.trim().to_string()
    };
    if bare.is_empty() { None } else { Some(bare) }
}

pub static TYPESCRIPT: LanguageConfig = LanguageConfig {
    name: "typescript",
    extensions: &[".ts"],
    language_fn: language,

    function_kinds: &["function_declaration"],
    class_kinds: &["class_declaration", "abstract_class_declaration"],
    method_kinds: &["method_definition"],
    interface_kinds: &["interface_declaration"],
    struct_kinds: &[],
    enum_kinds: &["enum_declaration"],
    enum_member_kinds: &["property_identifier"],
    type_alias_kinds: &["type_alias_declaration"],
    variable_kinds: &[],
    constant_kinds: &["lexical_declaration"],
    macro_kinds: &[],
    namespace_kinds: &["internal_module"],
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    import_kinds: &["import_statement", "export_statement"],
    call_kinds: &["call_expression"],
    member_expr_kinds: &["member_expression"],
    type_ref_kinds: &["type_identifier"],

    comment_kinds: &["comment"],
    // JSDoc `/** ... */` (node kind `comment`); the walker's `is_doc_comment`
    // marker predicate excludes ordinary `//` and `/* */` comments.
    doc_comment_kinds: &["comment"],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "return_type",
    import_source_field: "source",
    call_function_field: "function",
    member_property_field: "property",
    receiver_field: "",

    queries: &TS_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(ts_resolve_name),
        is_exported: Some(ts_is_exported),
        is_async: Some(ts_is_async),
        is_static: Some(ts_is_static),
        get_visibility: Some(ts_visibility),
        should_skip_call: Some(ts_should_skip_call),
        should_recurse_into: Some(ts_should_recurse),
        resolve_import_specifier: Some(ts_resolve_import_specifier),
        resolve_import_names: Some(ts_resolve_import_names),
        resolve_module_path: Some(ts_resolve_module_path),
        nested_extraction: true,
        resolve_supertypes: Some(ts_resolve_supertypes),
        ..LanguageHooks::DEFAULT
    },
};

// ─── TSX (TypeScript + JSX) config ────────────────────────────────────────────
//
// Uses LANGUAGE_TSX (a superset of LANGUAGE_TYPESCRIPT that adds JSX grammar
// rules). All hooks, field names, and chunk kinds are identical to TYPESCRIPT.
// The only differences are:
//   * `language_fn` → LANGUAGE_TSX (enables JSX node types)
//   * `extensions`  → &[".tsx"] only (`.ts` still maps to TYPESCRIPT)
//   * `queries`     → TSX_QUERIES which extends TS_FRAMEWORK_ROUTES with the
//     React-Router JSX route QueryDef.
//
// React-Router JSX route query AST shape (LANGUAGE_TSX / LANGUAGE_JS):
//
//   jsx_self_closing_element                 ← @route.reg
//     [name] (identifier) "Route"            ← anchored via #eq?
//     [attribute] (jsx_attribute)
//       (property_identifier) "path"
//       (string) "/x"                        ← @route.path  (includes quotes)
//     [attribute] (jsx_attribute)
//       (property_identifier) "element"
//       (jsx_expression)
//         (jsx_self_closing_element)
//           [name] (identifier) "Users"      ← @route.handler
//
// No HTTP method in JSX routes → @route.method absent → runner emits "ANY".
static TSX_FRAMEWORK_ROUTES: [QueryDef; 4] = [
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
    // KNOWN LIMITATION: see the TS_FRAMEWORK_ROUTES nestjs_route note above — the
    // `.` adjacency anchor drops a route when a NON-route decorator is interleaved
    // between the route decorator and its method (`@Get('/a') @Injectable() ma()`).
    // Accepted by design to avoid re-introducing the decorator×method cross-product.
    QueryDef {
        name: "nestjs_route",
        source: "(class_body \
  (decorator \
    (call_expression \
      function: (identifier) @route.method \
      (#match? @route.method \"^(Get|Post|Put|Delete|Patch|All|Options|Head)$\") \
      arguments: (arguments \
        (string) @route.path))) \
  . \
  (method_definition \
    name: (property_identifier) @route.handler) @route.reg)",
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

static TSX_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &TSX_FRAMEWORK_ROUTES,
    http_calls: &[],
    extra_chunks: &[],
};

/// TypeScript + JSX extractor. Handles `.tsx` files using `LANGUAGE_TSX` so
/// that JSX node types (e.g. `jsx_self_closing_element`) are available for
/// React-Router route queries. All walker hooks are shared with TYPESCRIPT.
pub static TSX: LanguageConfig = LanguageConfig {
    name: "tsx",
    extensions: &[".tsx"],
    language_fn: language_tsx,

    function_kinds: &["function_declaration"],
    class_kinds: &["class_declaration", "abstract_class_declaration"],
    method_kinds: &["method_definition"],
    interface_kinds: &["interface_declaration"],
    struct_kinds: &[],
    enum_kinds: &["enum_declaration"],
    enum_member_kinds: &["property_identifier"],
    type_alias_kinds: &["type_alias_declaration"],
    variable_kinds: &[],
    constant_kinds: &["lexical_declaration"],
    macro_kinds: &[],
    namespace_kinds: &["internal_module"],
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    import_kinds: &["import_statement", "export_statement"],
    call_kinds: &["call_expression"],
    member_expr_kinds: &["member_expression"],
    type_ref_kinds: &["type_identifier"],

    comment_kinds: &["comment"],
    // JSDoc `/** ... */` (node kind `comment`); the walker's `is_doc_comment`
    // marker predicate excludes ordinary `//` and `/* */` comments.
    doc_comment_kinds: &["comment"],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "return_type",
    import_source_field: "source",
    call_function_field: "function",
    member_property_field: "property",
    receiver_field: "",

    queries: &TSX_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(ts_resolve_name),
        is_exported: Some(ts_is_exported),
        is_async: Some(ts_is_async),
        is_static: Some(ts_is_static),
        get_visibility: Some(ts_visibility),
        should_skip_call: Some(ts_should_skip_call),
        should_recurse_into: Some(ts_should_recurse),
        resolve_import_specifier: Some(ts_resolve_import_specifier),
        resolve_import_names: Some(ts_resolve_import_names),
        resolve_module_path: Some(ts_resolve_module_path),
        nested_extraction: true,
        resolve_supertypes: Some(ts_resolve_supertypes),
        ..LanguageHooks::DEFAULT
    },
};
