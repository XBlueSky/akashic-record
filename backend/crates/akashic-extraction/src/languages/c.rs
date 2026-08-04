use crate::types::*;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_c::LANGUAGE.into()
}

static EMPTY_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &[],
    http_calls: &[],
    extra_chunks: &[],
};

/// C names are shaped differently per node kind:
///   - `function_definition`: name is buried in a declarator chain
///     (function_declarator → … → identifier).
///   - `struct_specifier` / `enum_specifier` / `union_specifier`: name is the
///     `name` field (a `type_identifier`), NOT a declarator.
///   - `type_definition` (typedef): the new type name is the `declarator` field,
///     which in tree-sitter-c is a `type_identifier` directly for named typedefs
///     (e.g. `typedef struct {...} Person;` → `declarator` = `type_identifier`
///     "Person").
///   - `preproc_def` / `preproc_function_def`: macro name is the `name` field
///     (an `identifier`), not a declarator chain.
fn c_resolve_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "struct_specifier" | "enum_specifier" | "union_specifier" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from),
        "type_definition" => {
            // In tree-sitter-c the `declarator` field of a typedef is a
            // `type_identifier` directly (e.g. `typedef ... Person;`).
            node.child_by_field_name("declarator")
                .and_then(|d| innermost_identifier(d, src))
        }
        "preproc_def" | "preproc_function_def" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from),
        _ => {
            // function_definition and friends: walk the declarator chain.
            node.child_by_field_name("declarator")
                .and_then(|d| innermost_identifier(d, src))
        }
    }
}

/// Find the defining identifier inside a (possibly nested) C declarator.
fn innermost_identifier(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "type_identifier" | "field_identifier" => {
            return node.utf8_text(src).ok().map(String::from);
        }
        _ => {}
    }
    if let Some(inner) = node.child_by_field_name("declarator")
        && let Some(s) = innermost_identifier(inner, src)
    {
        return Some(s);
    }
    // Fallback: scan named children for the first identifier-bearing declarator.
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i)
            && let Some(s) = innermost_identifier(c, src)
        {
            return Some(s);
        }
    }
    None
}

/// Check whether a `function_definition` node has a `static` storage-class
/// specifier.
///
/// In tree-sitter-c, `storage_class_specifier` is a named child of
/// `function_definition`, and its single unnamed child has `kind()` equal to
/// the keyword text (e.g. `"static"`, `"extern"`, `"inline"`, …).
fn c_is_static(node: Node) -> bool {
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i)
            && c.kind() == "storage_class_specifier"
        {
            // The storage class keyword is the first (unnamed) child.
            for j in 0..c.child_count() {
                if c.child(j).is_some_and(|kw| kw.kind() == "static") {
                    return true;
                }
            }
        }
    }
    false
}

/// Extract the include path from a `preproc_include` node.
///
/// Returns the path stripped of its surrounding `"..."` or `<...>` delimiters.
fn c_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    // The path child is either `string_literal` or `system_lib_string`.
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i)
            && matches!(c.kind(), "string_literal" | "system_lib_string")
        {
            return c.utf8_text(src).ok().map(|t| {
                t.trim_matches(|ch: char| matches!(ch, '"' | '<' | '>'))
                    .to_string()
            });
        }
    }
    None
}

/// Resolve an include path to a module path.
///
/// All includes — same-directory (e.g. `"local.h"`) and relative-path
/// (e.g. `"../headers/util.h"`) — are resolved relative to the current
/// file's directory (derived by dropping the file stem from
/// `current_module`).  This mirrors how the pipeline's IMPORTS_FROM
/// resolver (Stage 5) identifies modules: a bare stem `"local"` cannot
/// be resolved, but `"dir/local"` can.  System headers such as `<stdio.h>`
/// become `"dir/stdio"` and will remain unresolvable (harmless).
fn c_resolve_module_path(specifier: &str, current_module: &str) -> Option<String> {
    let mut segments: Vec<&str> = current_module
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    segments.pop(); // drop file stem → its containing directory
    for part in specifier.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other.trim_end_matches(".h")),
        }
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}

pub static C: LanguageConfig = LanguageConfig {
    name: "c",
    extensions: &[".c"],
    language_fn: language,

    function_kinds: &["function_definition"],
    class_kinds: &[],
    method_kinds: &[],
    interface_kinds: &[],
    struct_kinds: &["struct_specifier"],
    enum_kinds: &["enum_specifier"],
    enum_member_kinds: &["enumerator"],
    type_alias_kinds: &["type_definition"],
    variable_kinds: &[],
    constant_kinds: &["preproc_def"],
    macro_kinds: &["preproc_function_def"],
    namespace_kinds: &[],
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    import_kinds: &["preproc_include"],
    call_kinds: &["call_expression"],
    member_expr_kinds: &["field_expression"],
    type_ref_kinds: &["type_identifier"],

    comment_kinds: &["comment"],
    // Doxygen `/** ... */` / `///` (node kind `comment`); the walker's
    // `is_doc_comment` predicate excludes ordinary `//` and `/* */` comments.
    doc_comment_kinds: &["comment"],

    name_field: "declarator",
    body_field: "body",
    params_field: "parameters",
    return_field: "type",
    import_source_field: "path",
    call_function_field: "function",
    member_property_field: "field",
    receiver_field: "",

    queries: &EMPTY_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(c_resolve_name),
        is_static: Some(c_is_static),
        resolve_import_specifier: Some(c_resolve_import_specifier),
        resolve_module_path: Some(c_resolve_module_path),
        ..LanguageHooks::DEFAULT
    },
};
