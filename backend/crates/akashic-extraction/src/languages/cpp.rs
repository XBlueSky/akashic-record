use crate::types::*;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_cpp::LANGUAGE.into()
}

static EMPTY_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &[],
    http_calls: &[],
    extra_chunks: &[],
};

/// C++ names by node kind:
///   - `class_specifier` / `struct_specifier` / `enum_specifier` / `union_specifier`:
///     name is the `name` field (a `type_identifier`), NOT a declarator.
///   - `namespace_definition`: name is the `name` field (a `namespace_identifier`).
///   - `preproc_def` / `preproc_function_def`: macro name is the `name` field.
///   - `alias_declaration` (C++ `using T = ...`): name is the `name` field.
///   - everything else (function_definition, type_definition, …): walk the
///     declarator chain to the innermost identifier.
fn cpp_resolve_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "class_specifier"
        | "struct_specifier"
        | "enum_specifier"
        | "union_specifier"
        | "namespace_definition"
        | "preproc_def"
        | "preproc_function_def"
        | "alias_declaration" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from),
        _ => node
            .child_by_field_name("declarator")
            .and_then(|d| innermost_identifier(d, src)),
    }
}

/// Walk a (possibly nested) C++ declarator to find the innermost identifier.
///
/// Handles:
/// - Plain `identifier` / `type_identifier` / `field_identifier`
/// - C++ qualified names (`qualified_identifier`)
/// - Operator overloads (`operator_name`)
/// - Destructors (`destructor_name`)
fn innermost_identifier(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier"
        | "type_identifier"
        | "field_identifier"
        | "qualified_identifier"
        | "operator_name"
        | "destructor_name" => {
            return node.utf8_text(src).ok().map(String::from);
        }
        _ => {}
    }
    // Recurse through the declarator chain first.
    if let Some(inner) = node.child_by_field_name("declarator")
        && let Some(s) = innermost_identifier(inner, src)
    {
        return Some(s);
    }
    // Fallback: scan named children for the first identifier-bearing child.
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i)
            && let Some(s) = innermost_identifier(c, src)
        {
            return Some(s);
        }
    }
    None
}

/// Return true when a `function_definition` (or any node) has a `static`
/// storage-class specifier among its named children.
fn cpp_is_static(node: Node) -> bool {
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i)
            && c.kind() == "storage_class_specifier"
        {
            for j in 0..c.child_count() {
                if c.child(j).is_some_and(|kw| kw.kind() == "static") {
                    return true;
                }
            }
        }
    }
    false
}

/// Recursion guard: which node kinds the walker should descend into.
///
/// The default guard (`None`) enters scope containers and the root only.
/// C++ needs two extra kinds to be transparent:
///
/// * `template_declaration` — wraps `class_specifier` / `function_definition`
///   for template entities. Without this, `template <T> class Foo {}` would
///   not be traversed and `Foo` would be missed.
/// * `declaration_list` — the body wrapper of `namespace_definition` and the
///   body of `class_specifier` named `field_declaration_list`. Both appear as
///   the direct named child of their parent in tree-sitter-cpp; the walker
///   visits named children of the parent node, but does not descend into them
///   unless they are recognised scope containers. Adding them here lets the
///   walker reach the constructs inside without modifying the walker.
/// * `field_declaration_list` — the body node of class_specifier / struct_specifier.
fn cpp_should_recurse(node: Node) -> bool {
    node.parent().is_none()
        || matches!(
            node.kind(),
            "template_declaration"
                | "namespace_definition"
                | "class_specifier"
                | "struct_specifier"
                | "union_specifier"
                | "declaration_list"
                | "field_declaration_list"
        )
}

/// Extract the include path from a `preproc_include` node, stripping the
/// surrounding `"..."` or `<...>` delimiters.
fn cpp_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
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

/// Resolve a `#include` path to a canonical module path.
///
/// All includes — same-directory (e.g. `"util.h"`, `"vector"`) and
/// relative-path (e.g. `"../headers/util.hpp"`) — are resolved relative
/// to the current file's directory (derived by dropping the file stem
/// from `current_module`).  This mirrors how the pipeline's IMPORTS_FROM
/// resolver (Stage 5) identifies modules: a bare stem `"util"` cannot be
/// resolved, but `"dir/util"` can.  System headers such as `<vector>`
/// become `"dir/vector"` and will remain unresolvable (harmless).
fn cpp_resolve_module_path(specifier: &str, current_module: &str) -> Option<String> {
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
            other => segments.push(other.trim_end_matches(".hpp").trim_end_matches(".h")),
        }
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}

/// EXT-6c-2: resolve supertypes from a C++ `class_specifier` or
/// `struct_specifier` via its `base_class_clause` child.
///
/// Grammar shape (tree-sitter-cpp):
///   class_specifier / struct_specifier
///     base_class_clause                 ← named child (contains base specs)
///       ":"
///       access_specifier*               ← "public" / "private" / "protected"
///       type_identifier                 ← a plain base class name (`Base`)
///       qualified_identifier            ← a namespaced base (`ns::Base`)
///       template_type                   ← a templated base (`Base<T>`); its
///                                         `name` field is the `type_identifier`
///
/// Strategy: walk all named children of `base_class_clause`, skip
/// `access_specifier` nodes, emit each base name as Extends.
/// All C++ base classes are Extends (there is no interface distinction
/// in the grammar — `virtual` is a separate concept).
///
/// Strip generics: `Container<T>` → strip at `<`.
fn cpp_resolve_supertypes(node: Node, src: &[u8]) -> Vec<(String, EdgeKind)> {
    let mut out = Vec::new();
    if !matches!(node.kind(), "class_specifier" | "struct_specifier") {
        return out;
    }

    // Find the base_class_clause named child.
    let mut cursor = node.walk();
    let base_clause = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "base_class_clause");
    let clause = match base_clause {
        Some(c) => c,
        None => return out,
    };

    // Walk all children; skip access_specifier, emit type_identifier.
    let mut cc = clause.walk();
    for child in clause.children(&mut cc) {
        match child.kind() {
            "access_specifier" => {} // skip public/private/protected
            "type_identifier" => {
                if let Ok(text) = child.utf8_text(src) {
                    let bare = cpp_strip_generics(text);
                    if !bare.is_empty() {
                        out.push((bare.to_string(), EdgeKind::Extends));
                    }
                }
            }
            "qualified_identifier" => {
                // `ns::Base` — take the trailing simple name.
                if let Ok(text) = child.utf8_text(src) {
                    let bare = text.trim().rsplit("::").next().unwrap_or("").trim();
                    let bare = cpp_strip_generics(bare);
                    if !bare.is_empty() {
                        out.push((bare.to_string(), EdgeKind::Extends));
                    }
                }
            }
            // FIX: a TEMPLATED base (`class D : public Base<T>`) parses as a
            // `template_type` node, not a `type_identifier`, so it was
            // previously dropped. Its base name lives in the `name` field
            // (a `type_identifier`, e.g. `Base`); the `arguments` field holds
            // the `template_argument_list`. Emit the bare name as Extends.
            "template_type" => {
                if let Some(name_node) = child.child_by_field_name("name")
                    && let Ok(text) = name_node.utf8_text(src)
                {
                    // `name` is a plain type_identifier; cpp_strip_generics
                    // is a defensive no-op here.
                    let bare = cpp_strip_generics(text);
                    if !bare.is_empty() {
                        out.push((bare.to_string(), EdgeKind::Extends));
                    }
                }
            }
            _ => {}
        }
    }

    out
}

/// Strip a trailing `<…>` template suffix from a C++ type name.
fn cpp_strip_generics(name: &str) -> &str {
    if let Some(pos) = name.find('<') {
        name[..pos].trim()
    } else {
        name.trim()
    }
}

pub static CPP: LanguageConfig = LanguageConfig {
    name: "cpp",
    extensions: &[".cpp", ".cc", ".cxx", ".hpp"],
    language_fn: language,

    function_kinds: &["function_definition"],
    class_kinds: &["class_specifier"],
    method_kinds: &[],
    interface_kinds: &[],
    struct_kinds: &["struct_specifier"],
    enum_kinds: &["enum_specifier"],
    enum_member_kinds: &["enumerator"],
    type_alias_kinds: &["type_definition", "alias_declaration"],
    variable_kinds: &[],
    constant_kinds: &["preproc_def"],
    macro_kinds: &["preproc_function_def"],
    namespace_kinds: &["namespace_definition"],
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
        resolve_name: Some(cpp_resolve_name),
        is_static: Some(cpp_is_static),
        should_recurse_into: Some(cpp_should_recurse),
        resolve_import_specifier: Some(cpp_resolve_import_specifier),
        resolve_module_path: Some(cpp_resolve_module_path),
        resolve_supertypes: Some(cpp_resolve_supertypes),
        ..LanguageHooks::DEFAULT
    },
};
