use crate::types::*;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_go::LANGUAGE.into()
}

// ─── Gin framework-route queries ──────────────────────────────────────────────
//
// Matches `r.METHOD(path, handler)` call patterns (Gin).
//
// AST shape (tree-sitter-go):
//
//   call_expression                                ← @route.reg
//     selector_expression
//       <receiver-expression>
//       field_identifier "GET"/"POST"/…            ← @route.method
//     argument_list
//       interpreted_string_literal                 ← @route.path  (includes quotes)
//       identifier                                 ← @route.handler
static GO_FRAMEWORK_ROUTES: [QueryDef; 1] = [QueryDef {
    name: "gin_route",
    source: "(call_expression \
  function: (selector_expression \
    field: (field_identifier) @route.method \
    (#match? @route.method \"^(GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS|Any|Handle)$\")) \
  arguments: (argument_list \
    (interpreted_string_literal) @route.path \
    (identifier) @route.handler)) @route.reg",
    compiled: std::sync::OnceLock::new(),
}];

static GO_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &GO_FRAMEWORK_ROUTES,
    http_calls: &[],
    extra_chunks: &[],
};

/// `type_declaration` has no `name` field — the name lives on its child
/// `type_spec` (or `type_alias`) in its `name` field.  Everything else uses
/// the standard `name` field.
fn go_resolve_name(node: Node, src: &[u8]) -> Option<String> {
    if node.kind() == "type_declaration" {
        for i in 0..node.named_child_count() {
            if let Some(spec) = node.named_child(i)
                && (spec.kind() == "type_spec" || spec.kind() == "type_alias")
            {
                return spec
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(src).ok())
                    .map(String::from);
            }
        }
        return None;
    }
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .map(String::from)
}

/// Extract the method receiver from a `method_declaration`.
///
/// The receiver is a `parameter_list` (the `receiver` field) containing one
/// `parameter_declaration` whose `type` field is the receiver type.
/// A leading `*` marks a pointer receiver; otherwise it is a value receiver.
fn go_resolve_method_receiver(node: Node, src: &[u8]) -> Option<MethodReceiver> {
    let recv = node.child_by_field_name("receiver")?;
    for i in 0..recv.named_child_count() {
        if let Some(param) = recv.named_child(i)
            && let Some(ty) = param.child_by_field_name("type")
        {
            let raw = ty.utf8_text(src).ok()?;
            let is_pointer = raw.trim_start().starts_with('*');
            let type_name = raw
                .trim_start_matches(|c: char| c == '*' || c.is_whitespace())
                .to_string();
            return Some(MethodReceiver {
                type_name,
                is_pointer,
                is_mutable: false,
            });
        }
    }
    None
}

/// Resolve the callee name for a `call_expression`.
///
/// - `selector_expression` function (`fmt.Println`): returns the `field` child
///   (the method/function name, e.g. `Println`).
/// - Otherwise: returns the full text of the `function` child.
fn go_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
    let func = node.child_by_field_name("function")?;
    if func.kind() == "selector_expression" {
        return func
            .child_by_field_name("field")
            .and_then(|f| f.utf8_text(src).ok())
            .map(String::from);
    }
    func.utf8_text(src).ok().map(String::from)
}

/// Extract the canonical import path from an `import_spec` node.
///
/// The `path` field is an `interpreted_string_literal` whose text includes the
/// surrounding double-quotes, e.g. `"fmt"`.  We strip those quotes.
fn go_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    let path = node.child_by_field_name("path")?;
    path.utf8_text(src)
        .ok()
        .map(|t| t.trim_matches('"').to_string())
}

/// Go import paths are canonical; pass through verbatim.
fn go_resolve_module_path(specifier: &str, _current: &str) -> Option<String> {
    Some(specifier.to_string())
}

/// Recursion guard: descend into import blocks so that grouped imports are
/// captured.
///
/// `import ( "fmt" "strings" )` parses as:
///   `import_declaration → import_spec_list → import_spec*`
/// Without descending `import_declaration` and `import_spec_list` the walker
/// would never reach the `import_spec` nodes.  Single-import forms
/// (`import "fmt"`) are `import_declaration → import_spec` directly, and also
/// need the parent traversal enabled.
fn go_should_recurse(node: Node) -> bool {
    node.parent().is_none() || matches!(node.kind(), "import_declaration" | "import_spec_list")
}

pub static GO: LanguageConfig = LanguageConfig {
    name: "go",
    extensions: &[".go"],
    language_fn: language,

    function_kinds: &["function_declaration"],
    class_kinds: &[],
    method_kinds: &["method_declaration"],
    interface_kinds: &[],
    struct_kinds: &[],
    enum_kinds: &[],
    enum_member_kinds: &[],
    // Both `type Foo struct {...}` and `type Foo interface {...}` parse as
    // `type_declaration` — the grammar does not use distinct node kinds.
    // We classify all of them as a single "type" chunk; struct/interface
    // sub-distinction is a later enrichment pass.
    type_alias_kinds: &["type_declaration"],
    variable_kinds: &[],
    constant_kinds: &["const_declaration"],
    macro_kinds: &[],
    namespace_kinds: &[],
    // `package_clause` is metadata, not a code unit — leave module_kinds empty.
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    import_kinds: &["import_spec"],
    call_kinds: &["call_expression"],
    member_expr_kinds: &["selector_expression"],
    type_ref_kinds: &["type_identifier"],

    comment_kinds: &["comment"],
    doc_comment_kinds: &[],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "result",
    import_source_field: "path",
    call_function_field: "function",
    member_property_field: "field",
    receiver_field: "receiver",

    queries: &GO_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(go_resolve_name),
        resolve_method_receiver: Some(go_resolve_method_receiver),
        resolve_call_name: Some(go_resolve_call_name),
        resolve_import_specifier: Some(go_resolve_import_specifier),
        resolve_module_path: Some(go_resolve_module_path),
        should_recurse_into: Some(go_should_recurse),
        ..LanguageHooks::DEFAULT
    },
};
