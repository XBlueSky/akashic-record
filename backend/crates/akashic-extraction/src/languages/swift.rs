use crate::types::*;
use crate::walker_helpers::default_name_from_field;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_swift::LANGUAGE.into()
}

static EMPTY_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &[],
    http_calls: &[],
    extra_chunks: &[],
};

/// Recursion guard for the Swift walker.
///
/// GRAMMAR QUIRK — type-declaration overloading: tree-sitter-swift 0.7.2
/// represents `class`, `struct`, AND `enum` declarations with a SINGLE node
/// kind, `class_declaration`, disambiguated only by an unnamed
/// `declaration_kind` token child (`class` / `struct` / `enum`).  `protocol`
/// gets its own kind, `protocol_declaration`.  Their braced bodies differ:
///
/// * `class` / `struct` → `class_body`
/// * `enum`             → `enum_class_body`
/// * `protocol`         → `protocol_body`
///
/// We must descend the root and every type-container body so member
/// declarations (methods, init, nested cases) are reached and chunked.
///
/// CRITICAL — no nested-func leak: function / method / init bodies are
/// `function_body` nodes.  We deliberately do NOT list `function_body` here, so
/// a callable's interior is never walked for chunks; nested defs do not surface
/// as top-level chunks, and a callable's interior calls are instead reached via
/// the walker's call-pass over each leaf chunk.
fn swift_should_recurse(node: Node) -> bool {
    node.parent().is_none()
        || matches!(
            node.kind(),
            "class_declaration"
                | "protocol_declaration"
                | "class_body"
                | "enum_class_body"
                | "protocol_body"
        )
}

/// Refine the chunk category for a Swift `class_declaration`.
///
/// tree-sitter-swift overloads ONE node-kind, `class_declaration`, for `class`,
/// `struct`, `enum`, and `extension`, disambiguated by a `declaration_kind`
/// field whose own node-kind is the keyword (`class`/`struct`/`enum`/
/// `extension`).  Node-kind-string classification lands them all in `Class`;
/// this hook reads `declaration_kind` to emit the precise `chunk_type`.
/// `extension` has no dedicated category and stays `Class` (its closest analog).
/// `protocol_declaration` is a separate node-kind already classified as
/// `Interface`, so it never reaches here.
fn swift_refine_category(node: Node, _src: &[u8], category: ChunkCategory) -> ChunkCategory {
    if node.kind() != "class_declaration" {
        return category;
    }
    match node
        .child_by_field_name("declaration_kind")
        .map(|k| k.kind())
    {
        Some("struct") => ChunkCategory::Struct,
        Some("enum") => ChunkCategory::Enum,
        // "class", "extension", or anything unexpected → keep Class.
        _ => ChunkCategory::Class,
    }
}

/// Resolve the display name of a Swift declaration.
///
/// Most declarations expose their name via the `name` field (a `type_identifier`
/// for types, a `simple_identifier` for functions).  Two callable kinds do NOT
/// have a `name` field and need special handling:
///
/// * `init_declaration` — the constructor; its name is the unnamed `init`
///   keyword token.  We synthesize the name `"init"`.
/// * `deinit_declaration` — the destructor; synthesize `"deinit"`.
///
/// Without this hook those declarations would yield `None` from
/// `default_name_from_field` and be silently dropped (no chunk emitted).
fn swift_resolve_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "init_declaration" => Some("init".to_string()),
        "deinit_declaration" => Some("deinit".to_string()),
        _ => default_name_from_field(node, src, "name"),
    }
}

/// Extract the callee name from a Swift `call_expression`.
///
/// A `call_expression`'s first named child is the callee, followed by a
/// `call_suffix` (the argument list).  The callee is either:
///
/// * a `simple_identifier` for a bare call — `foo()` → `"foo"`;
/// * a `navigation_expression` for a member call — `self.bump()` /
///   `obj.method()`.  The trailing method identifier lives in the
///   `navigation_suffix` child's `simple_identifier`.
///
/// We return the bare trailing callee identifier in both cases, matching the
/// convention used by the C# / Java / PHP resolvers.
fn swift_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
    let callee = node.named_child(0)?;
    match callee.kind() {
        "navigation_expression" => {
            // The trailing `.method` is a `navigation_suffix` child holding a
            // `simple_identifier`.
            let suffix = (0..callee.named_child_count())
                .filter_map(|i| callee.named_child(i))
                .find(|c| c.kind() == "navigation_suffix")?;
            (0..suffix.named_child_count())
                .filter_map(|i| suffix.named_child(i))
                .find(|c| c.kind() == "simple_identifier")
                .and_then(|n| n.utf8_text(src).ok())
                .map(String::from)
        }
        "simple_identifier" => callee.utf8_text(src).ok().map(String::from),
        _ => None,
    }
}

/// Extract visibility from a Swift declaration's `modifiers` child.
///
/// Swift access-level modifiers live in a `modifiers` named child, inside a
/// `visibility_modifier` node whose text is one of `open` / `public` /
/// `internal` / `fileprivate` / `private`.  Mapping to the shared `Visibility`
/// enum:
///
///   `open` / `public`       → Public
///   `internal`              → Internal
///   `fileprivate` / `private` → Private
///
/// Swift's DEFAULT access level when no modifier is present is `internal`, so
/// we return `Internal` for any declaration lacking a `visibility_modifier`.
fn swift_visibility(node: Node, src: &[u8]) -> Visibility {
    let mods = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == "modifiers");

    if let Some(mods) = mods {
        for i in 0..mods.named_child_count() {
            if let Some(child) = mods.named_child(i)
                && child.kind() == "visibility_modifier"
                && let Ok(text) = child.utf8_text(src)
            {
                return match text {
                    "open" | "public" => Visibility::Public,
                    "internal" => Visibility::Internal,
                    "fileprivate" | "private" => Visibility::Private,
                    _ => Visibility::Internal,
                };
            }
        }
    }
    // No explicit access modifier → internal (Swift's default access level).
    Visibility::Internal
}

/// Extract the imported module path from a Swift `import_declaration`.
///
/// Swift imports are of the form:
///   `import Foundation`
///   `import os.log`            (submodule path; dots retained)
///   `import func Foo.bar`      (kind-qualified import)
///
/// The grammar wraps the module path in an `identifier` child whose text is the
/// dotted path (e.g. `"os.log"`).  We read its full text verbatim — Swift module
/// paths are already `.`-separated, so no rewriting is needed.
fn swift_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == "identifier")
        .and_then(|c| c.utf8_text(src).ok())
        .map(String::from)
}

/// Swift module paths are self-contained dotted identifiers (`Foundation`,
/// `os.log`); pass them through without host-path rewriting.
fn swift_resolve_module_path(specifier: &str, _current_module: &str) -> Option<String> {
    Some(specifier.to_string())
}

/// Resolve the fully-qualified name for a Swift declaration.
///
/// FQN scheme: `.`-joined — `Counter.increment`, `Outer.Inner.method`.  This is
/// the internal identity key every language config and the walker's own
/// `parent_fqn` (`scope_path`) use, so a chunk's `parent_fqn` exactly equals its
/// parent chunk's `fqn` (no cross-field separator skew).
///
/// We consult the walker's scope stack first (it pushes a frame for every
/// `class_declaration` — class/struct/enum — and `protocol_declaration` it
/// descends, including the enum-scope-frame fix so enum methods nest under the
/// enum).  As a defensive fallback we climb the AST to the nearest enclosing
/// type container, mirroring the C# / Java / PHP resolvers.
fn swift_resolve_fqn(node: Node, src: &[u8], scope: &ScopeStack) -> Option<String> {
    let name = swift_resolve_name(node, src)?;

    if !scope.is_empty() {
        let prefix = scope
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        return Some(format!("{prefix}.{name}"));
    }

    // Scope stack empty: climb the AST to the nearest enclosing type container.
    let container_kinds = ["class_declaration", "protocol_declaration"];
    let mut parent = node.parent();
    while let Some(p) = parent {
        if container_kinds.contains(&p.kind())
            && let Some(cn) = default_name_from_field(p, src, "name")
        {
            return Some(format!("{cn}.{name}"));
        }
        parent = p.parent();
    }

    Some(name)
}

/// EXT-6c-2: resolve supertypes from a Swift `class_declaration` or
/// `protocol_declaration` via its `inheritance_specifier` children.
///
/// Grammar shape (tree-sitter-swift 0.7.x):
///   class_declaration / protocol_declaration
///     type_inheritance_clause             ← unnamed or named child
///       inheritance_specifier*            ← each conformance
///         user_type                       ← the type name
///           type_identifier = "Shape"
///
/// Swift's type inheritance clause is flat — no `extends`/`with` distinction
/// at the grammar level.  A class's first inheritance_specifier might be a
/// class (Extends), but the grammar doesn't distinguish.  Per strategy: emit
/// ALL as `Implements` (flat clause, no distinction).
///
/// Strip generics: `Array<Element>` → `Array`.
fn swift_resolve_supertypes(node: Node, src: &[u8]) -> Vec<(String, EdgeKind)> {
    let mut out = Vec::new();
    if !matches!(node.kind(), "class_declaration" | "protocol_declaration") {
        return out;
    }

    // tree-sitter-swift 0.7.x grammar: `inheritance_specifier` nodes appear as
    // direct named children of `class_declaration` / `protocol_declaration`
    // (there is no intermediate `type_inheritance_clause` wrapper node in this
    // grammar version).  Each `inheritance_specifier` holds a `user_type` with
    // a `type_identifier` leaf.  We walk all named children by index.
    for ci in 0..node.named_child_count() {
        let child = match node.named_child(ci) {
            Some(c) => c,
            None => continue,
        };
        match child.kind() {
            "inheritance_specifier" => {
                // inheritance_specifier → user_type → type_identifier (index-based).
                let ut = (0..child.named_child_count())
                    .filter_map(|i| child.named_child(i))
                    .find(|c| c.kind() == "user_type");
                if let Some(ut) = ut {
                    let ti = (0..ut.named_child_count())
                        .filter_map(|i| ut.named_child(i))
                        .find(|c| c.kind() == "type_identifier");
                    if let Some(ti) = ti
                        && let Ok(text) = ti.utf8_text(src)
                    {
                        let bare = swift_strip_generics(text);
                        if !bare.is_empty() {
                            out.push((bare.to_string(), EdgeKind::Implements));
                        }
                    }
                } else {
                    // Fallback: direct type_identifier inside inheritance_specifier.
                    let ti = (0..child.named_child_count())
                        .filter_map(|i| child.named_child(i))
                        .find(|c| c.kind() == "type_identifier");
                    if let Some(ti) = ti
                        && let Ok(text) = ti.utf8_text(src)
                    {
                        let bare = swift_strip_generics(text);
                        if !bare.is_empty() {
                            out.push((bare.to_string(), EdgeKind::Implements));
                        }
                    }
                }
            }
            // Also handle type_inheritance_clause if present in some grammar versions.
            "type_inheritance_clause" => {
                for ii in 0..child.named_child_count() {
                    let ispec = match child.named_child(ii) {
                        Some(c) => c,
                        None => continue,
                    };
                    if ispec.kind() != "inheritance_specifier" {
                        continue;
                    }
                    let ti = (0..ispec.named_child_count())
                        .filter_map(|i| ispec.named_child(i))
                        .find(|c| c.kind() == "type_identifier");
                    if let Some(ti) = ti
                        && let Ok(text) = ti.utf8_text(src)
                    {
                        let bare = swift_strip_generics(text);
                        if !bare.is_empty() {
                            out.push((bare.to_string(), EdgeKind::Implements));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    out
}

/// Strip a trailing `<…>` generic suffix from a Swift type name.
fn swift_strip_generics(name: &str) -> &str {
    if let Some(pos) = name.find('<') {
        name[..pos].trim()
    } else {
        name.trim()
    }
}

/// Swift `LanguageConfig`.
///
/// TYPE-DECLARATION CLASSIFICATION (documented limitation): tree-sitter-swift
/// 0.7.2 overloads ONE node kind, `class_declaration`, for `class`, `struct`,
/// AND `enum` (disambiguated only by an unnamed `declaration_kind` token, not a
/// distinct node kind).  Because the walker classifies chunks purely by node
/// kind, all three are emitted with `chunk_type = "class"`.  `protocol` has its
/// own kind and is classified as `interface` (the closest analog).  `extension`
/// is also a `class_declaration` and would therefore classify as `class` — its
/// members nest under the extended type's name.  This v1 limitation is
/// acceptable: chunking, `.`-joined FQNs, scope nesting, visibility, and call
/// edges are all correct; only the struct/enum `chunk_type` label is coarse.
/// Refining it would require a classification hook keyed on `declaration_kind`,
/// deferred to a later task.
///
/// Functions: `function_declaration`, `init_declaration`, `deinit_declaration`,
/// and `protocol_function_declaration` are all `method_kinds`.  Swift's free
/// (top-level) functions are also `function_declaration`, so they are likewise
/// labelled `method`; this is acceptable for a method-centric OOP language and
/// keeps the leaf-chunk / call-pass behavior identical (both Function and Method
/// are leaf chunks).  Default access level is `internal` (Swift's default).
pub static SWIFT: LanguageConfig = LanguageConfig {
    name: "swift",
    extensions: &[".swift"],
    language_fn: language,

    // Swift free functions and methods share `function_declaration`; classified
    // as methods (method-centric language). See struct doc comment.
    function_kinds: &[],
    // class / struct / enum all parse as `class_declaration` (see doc comment).
    class_kinds: &["class_declaration"],
    method_kinds: &[
        "function_declaration",
        "init_declaration",
        "deinit_declaration",
        "protocol_function_declaration",
    ],
    // `protocol` is Swift's interface analog.
    interface_kinds: &["protocol_declaration"],
    struct_kinds: &[],
    enum_kinds: &[],
    enum_member_kinds: &["enum_entry"],
    type_alias_kinds: &["typealias_declaration"],
    variable_kinds: &[],
    constant_kinds: &[],
    macro_kinds: &[],
    namespace_kinds: &[],
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    import_kinds: &["import_declaration"],
    call_kinds: &["call_expression"],
    member_expr_kinds: &["navigation_expression"],
    type_ref_kinds: &["type_identifier"],

    comment_kinds: &["comment", "multiline_comment"],
    doc_comment_kinds: &[],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "return_type",
    import_source_field: "",
    // Call callee names are resolved via swift_resolve_call_name (the callee is a
    // first-child simple_identifier or navigation_expression, not a field).
    call_function_field: "",
    member_property_field: "",
    receiver_field: "",

    queries: &EMPTY_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(swift_resolve_name),
        resolve_fqn: Some(swift_resolve_fqn),
        get_visibility: Some(swift_visibility),
        should_recurse_into: Some(swift_should_recurse),
        refine_category: Some(swift_refine_category),
        resolve_import_specifier: Some(swift_resolve_import_specifier),
        resolve_call_name: Some(swift_resolve_call_name),
        resolve_module_path: Some(swift_resolve_module_path),
        nested_extraction: true,
        resolve_supertypes: Some(swift_resolve_supertypes),
        ..LanguageHooks::DEFAULT
    },
};
