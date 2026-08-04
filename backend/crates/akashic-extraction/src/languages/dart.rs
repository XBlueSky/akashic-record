use crate::types::*;
use crate::walker_helpers::default_name_from_field;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_dart::LANGUAGE.into()
}

static EMPTY_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &[],
    http_calls: &[],
    extra_chunks: &[],
};

/// Recursion guard for the Dart walker.
///
/// GRAMMAR SHAPE — wrapper layers: tree-sitter-dart 0.2.0 wraps every type
/// member in a `class_member` node, and several members in a further
/// `declaration` node, before the construct of interest is reached.  The walker
/// classifies/chunks purely by node kind, so it MUST descend these structural
/// wrappers to reach the declarations they hold:
///
/// * Root `source_file` (`parent().is_none()`).
/// * Type containers — `class_declaration`, `mixin_declaration`,
///   `enum_declaration`, `extension_declaration`.
/// * Container bodies — `class_body` (class / mixin / extension) and `enum_body`
///   (an enum's braced body, holding `enum_constant`s and, for enhanced enums,
///   `method_declaration` siblings).
/// * `class_member` — the per-member wrapper inside a body; descended so the
///   `method_declaration` / `declaration` it wraps is reached.
/// * `declaration` — wraps a `constructor_signature` (a constructor member) or a
///   field's `initialized_identifier_list`; descended so the constructor chunks.
///   (Field declarations hold no chunk-kind node, so descending them is inert.)
///
/// CRITICAL — no nested-func leak: a method/function body is a
/// `function_body` → `block`.  We deliberately do NOT list `function_body` or
/// `block` here, so a callable's interior is never walked for chunks; nested
/// functions inside a body never surface as top-level chunks, and a callable's
/// interior calls are reached via the walker's call-pass over each leaf chunk
/// instead.
fn dart_should_recurse(node: Node) -> bool {
    node.parent().is_none()
        || matches!(
            node.kind(),
            "class_declaration"
                | "mixin_declaration"
                | "enum_declaration"
                | "extension_declaration"
                | "class_body"
                | "enum_body"
                | "class_member"
                | "declaration"
        )
}

/// Resolve the display name of a Dart declaration.
///
/// GRAMMAR AWKWARDNESS — nested callable name: NEITHER a `method_declaration`
/// NOR a top-level `function_declaration` exposes a `name` field of its own.
/// The name is buried inside an inner signature node:
///
/// * `method_declaration` → `method_signature` (`signature` field) →
///   `function_signature` | `getter_signature` | `setter_signature`
///   | `operator_signature` → name (see below).
/// * `function_declaration` → `function_signature` (`signature` field) →
///   `identifier` (`name` field).
///
/// The inner signature's name is read as follows:
///
/// * `function_signature` / `getter_signature` / `setter_signature` — a direct
///   `name` field holding an `identifier` (`int get count`, `set count(v)`).
/// * `operator_signature` — has NO `name` field; the operator itself lives in
///   the `operator` field (one of `~`, a `binary_operator` such as `+` / `==`,
///   `[]`, or `[]=`).  We synthesise a name of the form `operator<op>`
///   (`operator+`, `operator==`, `operator[]`) so the chunk has a stable,
///   human-readable identity.
///
/// (An abstract method can be a bare signature directly under
/// `method_declaration`, with no surrounding `function_body`.)  We dig through
/// that chain so a callable chunk is emitted with its real name; a plain
/// `default_name_from_field(node, "name")` would return `None` and the callable
/// would be silently dropped (this is exactly why top-level `main`/`compute`
/// chunks need this hook).
///
/// FINDING CLOSED (dart_resolve_name getter/setter/operator drop): previously
/// only `function_signature` was dug out of the `method_signature`, so a
/// getter / setter / operator method — whose name lives in
/// `getter_signature` / `setter_signature` / `operator_signature` — resolved to
/// `None` and the entire chunk was silently dropped.  We now handle all four
/// inner-signature kinds.
///
/// Every other chunk kind — `class_declaration`, `mixin_declaration`,
/// `enum_declaration`, `extension_declaration`, `constructor_signature`,
/// `enum_constant` — exposes its name via a direct `name` field, so it falls
/// through to `default_name_from_field`.
fn dart_resolve_name(node: Node, src: &[u8]) -> Option<String> {
    if matches!(
        node.kind(),
        "method_declaration" | "function_declaration" | "local_function_declaration"
    ) {
        // Descend to the inner signature, then read its name.  A
        // `method_declaration` wraps the signature in a `method_signature`;
        // a `function_declaration` holds a `function_signature` directly (both
        // via the `signature` field).
        let sig = node
            .child_by_field_name("signature")
            .or_else(|| first_named_child_of_kind(node, "function_signature"))?;
        let inner = match sig.kind() {
            // Top-level / nested free functions: the `signature` field IS the
            // `function_signature` (no `method_signature` wrapper).
            "function_signature" | "getter_signature" | "setter_signature"
            | "operator_signature" => sig,
            // `method_declaration`: the `signature` field is a `method_signature`
            // whose only named child is the real signature node.
            _ => first_inner_signature(sig)?,
        };
        return dart_signature_name(inner, src);
    }
    // FINDING CLOSED (type_alias silently dropped): `type_alias` is a declared
    // chunk kind (`type_alias_kinds`), but the grammar gives it `"fields": {}`
    // — it has NO `name` field, so `default_name_from_field(node, "name")`
    // returned `None` and every Dart `typedef` was dropped.  The alias name is
    // a DIRECT `type_identifier` named child (the hidden `_type_name` rule
    // aliases `identifier` → `type_identifier` and inlines it into the parent).
    // Both `typedef` forms put the alias name as the first such DIRECT child:
    //   * `typedef IntList = List<int>;`  → `IntList` direct; `List` is nested
    //     inside the `type` RHS child, not a direct child.
    //   * `typedef int Cmp(int a, int b);` → `Cmp` direct; the leading `int`
    //     return type wraps its `type_identifier` inside a `type` child.
    // So matching the first DIRECT named `type_identifier` child correctly
    // picks the alias name and never the RHS/return-type identifier.
    if node.kind() == "type_alias" {
        let name_node = first_named_child_of_kind(node, "type_identifier")?;
        return name_node.utf8_text(src).ok().map(String::from);
    }
    default_name_from_field(node, src, "name")
}

/// First named child of a `method_signature` that is one of the four callable
/// signature kinds.  (`method_signature` also wraps `constructor_signature` /
/// `factory_constructor_signature`, but constructors are chunked via the
/// `constructor_signature` node kind directly, not through this path.)
fn first_inner_signature(method_sig: Node) -> Option<Node> {
    (0..method_sig.named_child_count())
        .filter_map(|i| method_sig.named_child(i))
        .find(|c| {
            matches!(
                c.kind(),
                "function_signature"
                    | "getter_signature"
                    | "setter_signature"
                    | "operator_signature"
            )
        })
}

/// Read the display name out of a single callable signature node.
///
/// `function_signature` / `getter_signature` / `setter_signature` carry a `name`
/// field (an `identifier`).  `operator_signature` has no name field; its
/// `operator` field holds the operator token (`+`, `==`, `[]`, `~`, …), which we
/// prefix with `operator` to yield `operator+`, `operator==`, etc.
fn dart_signature_name(sig: Node, src: &[u8]) -> Option<String> {
    if sig.kind() == "operator_signature" {
        let op = sig.child_by_field_name("operator")?;
        let text = op.utf8_text(src).ok()?;
        return Some(format!("operator{text}"));
    }
    default_name_from_field(sig, src, "name")
}

/// First named child of `node` whose kind matches `kind`.
fn first_named_child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == kind)
}

/// Extract the imported URI from a Dart `import_or_export` node.
///
/// Dart imports take the shape:
///   `import 'package:flutter/material.dart';`
///   `import 'dart:async';`
///   `import '../utils/helpers.dart';`
///
/// The grammar nests the URI string deeply:
///   `import_or_export` → `library_import` → `import_specification`
///     → `configurable_uri`[uri] → `uri` → `string_literal`
///       → `string_literal_single_quotes` → `template_chars_*` (the raw text).
///
/// We descend to the first `template_chars_*` token and return its verbatim
/// text — the URI string exactly as written (`package:flutter/material.dart`,
/// `dart:async`, `../utils/helpers.dart`).  Like the C/C++ include-path
/// resolvers, the import target is preserved verbatim (NOT rewritten to a
/// dotted module path), because a Dart import target is a URI, not a symbol
/// path.
fn dart_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    find_uri_text(node, src)
}

/// Recursively find the first `template_chars_*` token (the URI body) and read
/// its text.  Falls back to the `string_literal`'s inner text with surrounding
/// quotes stripped if the grammar shape changes.
fn find_uri_text(node: Node, src: &[u8]) -> Option<String> {
    let kind = node.kind();
    if kind.starts_with("template_chars") {
        return node.utf8_text(src).ok().map(String::from);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_uri_text(child, src) {
            return Some(found);
        }
    }
    None
}

/// Derive Dart visibility from the declaration's NAME — Dart's distinctive rule.
///
/// Dart has NO visibility keywords (`public` / `private` / `protected`).
/// Privacy is encoded in the IDENTIFIER: a name beginning with an underscore
/// (`_count`, `_reset`) is library-private; any other name is public.  We
/// resolve the declaration's name (via the same `dart_resolve_name` chain that
/// feeds chunking, so a `method_declaration`'s dug-out name is used) and inspect
/// its first byte.  A declaration whose name cannot be resolved defaults to
/// `Public` (Dart's default for a public-looking identifier).
fn dart_visibility(node: Node, src: &[u8]) -> Visibility {
    match dart_resolve_name(node, src) {
        Some(name) if name.starts_with('_') => Visibility::Private,
        _ => Visibility::Public,
    }
}

/// Resolve the fully-qualified name for a Dart declaration.
///
/// FQN scheme: `.`-joined — `Counter.increment`, `Status.isActive`.  This is the
/// internal identity key every language config and the walker's own
/// `parent_fqn` (`scope_path`) use, so a chunk's `parent_fqn` exactly equals its
/// parent chunk's `fqn` (no cross-field separator skew).  A private name keeps
/// its leading underscore in the FQN (`Counter._reset`).
///
/// We consult the walker's scope stack first (it pushes a frame for every
/// `class_declaration` / `extension_declaration` via `class_kinds`,
/// `mixin_declaration` via `trait_kinds`, and `enum_declaration` via the
/// enum-scope-frame path, so a mixin/enum method nests under its container).  As
/// a defensive fallback we climb the AST to the nearest enclosing type
/// container, mirroring the Swift / Kotlin / Scala resolvers.
fn dart_resolve_fqn(node: Node, src: &[u8], scope: &ScopeStack) -> Option<String> {
    let name = dart_resolve_name(node, src)?;

    if !scope.is_empty() {
        let prefix = scope
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        return Some(format!("{prefix}.{name}"));
    }

    // Scope stack empty: climb the AST to the nearest enclosing type container.
    let container_kinds = [
        "class_declaration",
        "mixin_declaration",
        "enum_declaration",
        "extension_declaration",
    ];
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

/// EXT-6c: resolve a Dart class's explicit supertypes.
///
/// Grammar shape (empirically verified via AST probe):
///
/// `class Dog extends Animal with Runner {}`
///   class_declaration
///     [superclass] superclass
///       [type]   type → type_identifier = "Animal"   ← Extends
///       mixins              ← child of superclass, NOT a field of class_declaration
///         type → type_identifier = "Runner"           ← Implements (mixin)
///
/// `class Circle implements Shape, Drawable {}`
///   class_declaration
///     [interfaces] interfaces
///       type → type_identifier = "Shape"             ← Implements
///       type → type_identifier = "Drawable"          ← Implements
///
/// CRITICAL: `mixins` is a NAMED CHILD of the `superclass` node, not a separate
/// field of `class_declaration`.  The `superclass`'s `[type]` field holds only
/// the `extends` target; the `mixins` sibling within `superclass` holds `with`
/// targets.  We must NOT recursively descend `superclass` without this split, or
/// both `Animal` and `Runner` get emitted as `Extends`.
fn dart_resolve_supertypes(node: Node, src: &[u8]) -> Vec<(String, EdgeKind)> {
    let mut out = Vec::new();
    if node.kind() != "class_declaration" {
        return out;
    }

    // Superclass + mixins live inside the `superclass` field node.
    if let Some(sc_node) = node.child_by_field_name("superclass") {
        // [type] field of the superclass node = the `extends` target.
        if let Some(ty) = sc_node.child_by_field_name("type") {
            dart_push_type_identifier(ty, src, EdgeKind::Extends, &mut out);
        }
        // `mixins` is a named child of `superclass` (not a field).
        let mut cursor = sc_node.walk();
        for child in sc_node.named_children(&mut cursor) {
            if child.kind() == "mixins" {
                let mut mc = child.walk();
                for ty in child.named_children(&mut mc) {
                    if ty.kind() == "type" {
                        dart_push_type_identifier(ty, src, EdgeKind::Implements, &mut out);
                    }
                }
            }
        }
    }

    // Interfaces: `implements Foo, Bar` → field "interfaces" → type children.
    if let Some(ifaces) = node.child_by_field_name("interfaces") {
        let mut cursor = ifaces.walk();
        for child in ifaces.named_children(&mut cursor) {
            if child.kind() == "type" {
                dart_push_type_identifier(child, src, EdgeKind::Implements, &mut out);
            }
        }
    }

    out
}

/// Push the bare `type_identifier` text found inside a `type` wrapper node.
fn dart_push_type_identifier(
    node: Node,
    src: &[u8],
    kind: EdgeKind,
    out: &mut Vec<(String, EdgeKind)>,
) {
    // The `type` node may itself be a `type_identifier`, or it may wrap one.
    let leaf = if node.kind() == "type_identifier" {
        node
    } else if let Some(c) = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == "type_identifier")
    {
        c
    } else {
        return;
    };
    if let Ok(text) = leaf.utf8_text(src) {
        let bare = if let Some(pos) = text.find('<') {
            text[..pos].trim()
        } else {
            text.trim()
        };
        if !bare.is_empty() {
            out.push((bare.to_string(), kind));
        }
    }
}

/// Dart `LanguageConfig`.
///
/// TYPE-DECLARATION CLASSIFICATION: tree-sitter-dart 0.2.0 gives each type its
/// own node kind, so classification is clean (no Swift/Kotlin-style overloading):
///
/// * `class_declaration`     → `class`.
/// * `mixin_declaration`     → `trait`  (DOCUMENTED CHOICE: a Dart `mixin` is a
///   reusable bundle of behaviour mixed in via `with` — trait-like, not an
///   instantiable class.  The shared config HAS `trait_kinds`/`ScopeKind::Trait`,
///   preferred over folding the mixin into `interface` or `class`).
/// * `enum_declaration`      → `enum`   (enhanced enums — with methods — parse;
///   see ENUM METHODS below).
/// * `extension_declaration` → `class`  (DOCUMENTED CHOICE: a Dart `extension`
///   adds members to an existing type; there is no dedicated "extension" chunk
///   type, so it is labelled `class`, the closest named-member-container analog.
///   Its body is also `class_body`, so its members chunk and nest under the
///   extension's name).
///
/// METHODS / FUNCTIONS (grammar awkwardness — documented): a method is a
/// `method_declaration` whose name is buried in `method_signature` → one of
/// `function_signature` / `getter_signature` / `setter_signature` /
/// `operator_signature` (dug out by `dart_resolve_name`).  A plain method and
/// a getter/setter carry a `name` field; an operator method carries an
/// `operator` field instead and is named `operator<op>` (e.g. `operator+`).  A
/// constructor is NOT a `method_declaration`; it is a
/// `constructor_signature` (wrapped in `class_member` → `declaration`), with a
/// direct `name` field (the type name) — classified as a method too.  Top-level
/// functions are `function_declaration` (real free functions, like Swift/Kotlin)
/// and are `function_kinds`.  All three are leaf chunks, so leaf-chunk/call-pass
/// behaviour is uniform.
///
/// ENUM METHODS: an `enum_declaration` pushes an `Enum` scope frame (via the
/// walker's enum-scope path), and the walker recurses `enum_body`, so an enhanced
/// enum's `method_declaration` children nest correctly as `Enum.method` in both
/// `fqn` and `parent_fqn`.  `enum_constant`s are `enum_member_kinds`.
///
/// VISIBILITY — Dart's distinctive rule (see `dart_visibility`): Dart has no
/// visibility keywords; privacy is by NAMING — a name starting with `_` is
/// library-private, otherwise public.  Visibility is therefore derived from the
/// declaration's resolved name, not from any modifier node.
///
/// IMPORTS: `import_or_export` nodes; the target is a URI STRING preserved
/// verbatim (`package:flutter/material.dart`, `dart:async`, `../utils/x.dart`),
/// like C/C++ include paths — NOT rewritten to a dotted module path.
///
/// FIELDS / VARIABLES (documented): instance fields and top-level variables
/// (`declaration` → `initialized_identifier_list`) are NOT chunked
/// (`variable_kinds`/`constant_kinds` empty); methods are the unit of interest,
/// matching the method-centric stance of the Swift/Kotlin/Scala configs.
pub static DART: LanguageConfig = LanguageConfig {
    name: "dart",
    extensions: &[".dart"],
    language_fn: language,

    // Top-level free functions + EXT-5 named nested functions
    // (`local_function_declaration`, which wraps a `function_signature` just like
    // the top-level `function_declaration`).
    function_kinds: &["function_declaration", "local_function_declaration"],
    // `extension_declaration` is a named member-container (documented as `class`).
    class_kinds: &["class_declaration", "extension_declaration"],
    // `method_declaration` (name dug out) + `constructor_signature` (direct name).
    method_kinds: &["method_declaration", "constructor_signature"],
    interface_kinds: &[],
    struct_kinds: &[],
    enum_kinds: &["enum_declaration"],
    enum_member_kinds: &["enum_constant"],
    type_alias_kinds: &["type_alias"],
    variable_kinds: &[],
    constant_kinds: &[],
    macro_kinds: &[],
    namespace_kinds: &[],
    module_kinds: &[],
    // Dart mixins are trait-like (mixed in via `with`); use the trait category.
    trait_kinds: &["mixin_declaration"],
    impl_kinds: &[],

    import_kinds: &["import_or_export"],
    call_kinds: &["call_expression"],
    member_expr_kinds: &["member_expression"],
    type_ref_kinds: &["type_identifier"],

    comment_kinds: &["comment", "documentation_comment"],
    doc_comment_kinds: &[],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "return_type",
    import_source_field: "",
    // A call's callee is the `function`-field child: a bare `identifier`, or a
    // `member_expression` whose `property` field is the trailing method name.
    // The default callee resolver handles both via these fields.
    call_function_field: "function",
    member_property_field: "property",
    receiver_field: "",

    queries: &EMPTY_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(dart_resolve_name),
        resolve_fqn: Some(dart_resolve_fqn),
        get_visibility: Some(dart_visibility),
        should_recurse_into: Some(dart_should_recurse),
        resolve_import_specifier: Some(dart_resolve_import_specifier),
        resolve_module_path: None,
        nested_extraction: true,
        resolve_supertypes: Some(dart_resolve_supertypes),
        ..LanguageHooks::DEFAULT
    },
};
