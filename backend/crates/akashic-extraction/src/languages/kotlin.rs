use crate::types::*;
use crate::walker_helpers::default_name_from_field;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_kotlin_ng::LANGUAGE.into()
}

static EMPTY_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &[],
    http_calls: &[],
    extra_chunks: &[],
};

/// Recursion guard for the Kotlin walker.
///
/// GRAMMAR QUIRK — type-declaration overloading: tree-sitter-kotlin-ng 1.1.0
/// represents `class`, `interface`, AND `enum class` declarations with a
/// SINGLE node kind, `class_declaration`, disambiguated only by a keyword child
/// (`interface` token, or an `enum` `class_modifier` inside `modifiers`), not by
/// a distinct node kind.  Singletons (`object Foo { ... }`) are
/// `object_declaration`; `companion object { ... }` is `companion_object`.
///
/// Container nodes the walker must descend to reach member declarations:
///
/// * Root `source_file` (`parent().is_none()`).
/// * `class_declaration` — class / interface / enum class.  Its braced body is
///   either `class_body` (class/interface/object) or `enum_class_body` (enum).
/// * `object_declaration` — a singleton; its body is a `class_body`.
/// * `companion_object` — `companion object { ... }`; its body is a `class_body`.
///   We descend it so its members chunk (and, lacking a name field of its own,
///   the companion is NOT itself a chunk/scope frame — its members nest directly
///   under the enclosing object/class, matching Kotlin's `Enclosing.member`
///   access semantics).
/// * `class_body` / `enum_class_body` — the braced bodies wrapping member
///   declarations (`function_declaration`, `property_declaration`,
///   `secondary_constructor`, nested `companion_object`, enum entries).
///
/// CRITICAL — no nested-func leak: function / method / secondary-constructor
/// bodies are `function_body` → `block` (or a bare `block`) nodes.  We
/// deliberately do NOT list `function_body` or `block` here, so a callable's
/// interior is never walked for chunks; nested defs do not surface as top-level
/// chunks, and a callable's interior calls are reached via the walker's
/// call-pass over each leaf chunk instead.
fn kotlin_should_recurse(node: Node) -> bool {
    node.parent().is_none()
        || matches!(
            node.kind(),
            "class_declaration"
                | "object_declaration"
                | "companion_object"
                | "class_body"
                | "enum_class_body"
        )
}

/// Refine the chunk category for a Kotlin `class_declaration`.
///
/// kotlin-ng overloads ONE node-kind, `class_declaration`, for `class`,
/// `interface`, and `enum class`.  They are distinguished by child structure:
/// an `interface` keyword token child marks an interface; an `enum_class_body`
/// body child marks an enum class (the `enum` modifier sits in a `modifiers`
/// child but the body kind is the unambiguous signal); otherwise it is a class
/// (incl. `data`/`sealed` classes).  Node-kind-string classification lands them
/// all in `Class`; this hook reads the children to emit the precise
/// `chunk_type`.  `object_declaration` is a separate node-kind (a singleton)
/// and is left as `Class` — there is no dedicated object category.
fn kotlin_refine_category(node: Node, _src: &[u8], category: ChunkCategory) -> ChunkCategory {
    if node.kind() != "class_declaration" {
        return category;
    }
    let mut cursor = node.walk();
    let mut is_interface = false;
    for child in node.children(&mut cursor) {
        match child.kind() {
            // Enum class: unambiguous via its body kind. Checked first because
            // an enum class also carries a `class` keyword token.
            "enum_class_body" => return ChunkCategory::Enum,
            "interface" => is_interface = true,
            _ => {}
        }
    }
    if is_interface {
        ChunkCategory::Interface
    } else {
        ChunkCategory::Class
    }
}

/// Resolve the display name of a Kotlin declaration.
///
/// Most declarations expose their name via the `name` field (an `identifier`).
/// `secondary_constructor` has NO `name` field — its name is the unnamed
/// `constructor` keyword token — so we synthesize `"constructor"`.  Without this
/// hook a secondary constructor would yield `None` from
/// `default_name_from_field` and be silently dropped (no chunk emitted).
fn kotlin_resolve_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "secondary_constructor" => Some("constructor".to_string()),
        _ => default_name_from_field(node, src, "name"),
    }
}

/// Extract the callee name from a Kotlin `call_expression`.
///
/// A `call_expression`'s first named child is the callee, followed by
/// `value_arguments` (and optionally `type_arguments` / `annotated_lambda`).
/// The callee is either:
///
/// * an `identifier` for a bare call — `log()` → `"log"`;
/// * a `navigation_expression` for a member call — `items.add(s)` /
///   `obj.method()`.  A `navigation_expression` is `expression . identifier`;
///   the trailing `identifier` (its last named child) is the method name.
///
/// We return the bare trailing callee identifier in both cases, matching the
/// convention used by the Swift / C# / Java / PHP resolvers.
fn kotlin_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
    let callee = node.named_child(0)?;
    match callee.kind() {
        "navigation_expression" => {
            // The trailing `.identifier` is the last named child.
            (0..callee.named_child_count())
                .rev()
                .filter_map(|i| callee.named_child(i))
                .find(|c| c.kind() == "identifier")
                .and_then(|n| n.utf8_text(src).ok())
                .map(String::from)
        }
        "identifier" => callee.utf8_text(src).ok().map(String::from),
        _ => None,
    }
}

/// Extract visibility from a Kotlin declaration's `modifiers` child.
///
/// Kotlin access modifiers live in a `modifiers` named child, inside a
/// `visibility_modifier` node whose text is one of `public` / `private` /
/// `protected` / `internal`.  Mapping to the shared `Visibility` enum is direct.
///
/// Kotlin's DEFAULT visibility when no modifier is present is `public`, so we
/// return `Public` for any declaration lacking a `visibility_modifier`.
fn kotlin_visibility(node: Node, src: &[u8]) -> Visibility {
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
                    "public" => Visibility::Public,
                    "private" => Visibility::Private,
                    "protected" => Visibility::Protected,
                    "internal" => Visibility::Internal,
                    _ => Visibility::Public,
                };
            }
        }
    }
    // No explicit visibility modifier → public (Kotlin's default visibility).
    Visibility::Public
}

/// Extract the imported path from a Kotlin `import` node.
///
/// Kotlin imports are of the form:
///   `import kotlin.math.PI`
///   `import com.example.helpers.*`   (glob; a `.` `*` token pair follows)
///   `import com.example.Foo as Bar`  (aliased; `as <identifier>` follows)
///
/// The dotted path is a `qualified_identifier` child whose text is already
/// `.`-separated (e.g. `"kotlin.math.PI"`).  We read it verbatim; for a glob
/// import we append `.*` to signal the wildcard, mirroring the Java resolver.
/// The `as` alias is intentionally ignored — the import edge records the
/// imported path, not its local binding.
fn kotlin_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    let path = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == "qualified_identifier")
        .and_then(|c| c.utf8_text(src).ok())?;

    // A glob import (`import a.b.*`) has an unnamed `*` token child.
    let has_glob = (0..node.child_count())
        .filter_map(|i| node.child(i))
        .any(|c| c.kind() == "*");

    if has_glob {
        Some(format!("{path}.*"))
    } else {
        Some(path.to_string())
    }
}

/// Kotlin import paths are self-contained dotted identifiers (`kotlin.math.PI`);
/// pass them through without host-path rewriting.
fn kotlin_resolve_module_path(specifier: &str, _current_module: &str) -> Option<String> {
    Some(specifier.to_string())
}

/// Resolve the fully-qualified name for a Kotlin declaration.
///
/// FQN scheme: `.`-joined — `Counter.increment`, `Outer.Inner.method`.  This is
/// the internal identity key every language config and the walker's own
/// `parent_fqn` (`scope_path`) use, so a chunk's `parent_fqn` exactly equals its
/// parent chunk's `fqn` (no cross-field separator skew).
///
/// We consult the walker's scope stack first (it pushes a frame for every
/// `class_declaration` — class/interface/enum — and `object_declaration` it
/// descends, via `class_kinds`).  As a defensive fallback we climb the AST to
/// the nearest enclosing type container, mirroring the Swift / C# / Java
/// resolvers.
///
/// PACKAGE-SCOPE DECISION: the `package_header` is NOT pushed as a scope frame
/// (it is not in any `*_kinds`), so FQNs are type-rooted (`Counter.increment`),
/// NOT package-qualified (`com.example.Counter.increment`).  This is consistent
/// with the Java config, whose enum FQNs are likewise bare (`Color.method`).
fn kotlin_resolve_fqn(node: Node, src: &[u8], scope: &ScopeStack) -> Option<String> {
    let name = kotlin_resolve_name(node, src)?;

    if !scope.is_empty() {
        let prefix = scope
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        return Some(format!("{prefix}.{name}"));
    }

    // Scope stack empty: climb the AST to the nearest enclosing type container.
    let container_kinds = ["class_declaration", "object_declaration"];
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

/// EXT-6c-2: resolve a Kotlin class's supertypes from its `delegation_specifiers`
/// child.
///
/// Grammar shape (tree-sitter-kotlin-ng 1.1.0):
///   class_declaration
///     delegation_specifiers               ← unnamed child (no field name)
///       delegation_specifier              ← named children
///         user_type                       → Implements (interface/trait)
///           type_identifier = "Shape"
///         constructor_invocation          → Extends (superclass with args)
///           user_type
///             type_identifier = "Animal"
///
/// Rules:
/// * `delegation_specifier` containing a direct `user_type` (no
///   `constructor_invocation`) → Implements (interface conformance).
/// * `delegation_specifier` containing a `constructor_invocation` → its
///   `user_type`'s leading `type_identifier` → Extends (superclass).
///
/// In Kotlin, a class may extend at most one class (constructor_invocation)
/// and implement multiple interfaces (user_type); both appear flat in the
/// `delegation_specifiers` list.
fn kotlin_resolve_supertypes(node: Node, src: &[u8]) -> Vec<(String, EdgeKind)> {
    let mut out = Vec::new();
    if !matches!(node.kind(), "class_declaration" | "object_declaration") {
        return out;
    }

    // Find the `delegation_specifiers` child (unnamed, so walk all children
    // by index to avoid cursor lifetime issues).
    let deleg_specs = (0..node.child_count())
        .filter_map(|i| node.child(i))
        .find(|c| c.kind() == "delegation_specifiers");
    let deleg_specs = match deleg_specs {
        Some(d) => d,
        None => return out,
    };

    // Walk each named `delegation_specifier` by index.
    for si in 0..deleg_specs.named_child_count() {
        let spec = match deleg_specs.named_child(si) {
            Some(s) => s,
            None => continue,
        };
        if spec.kind() != "delegation_specifier" {
            continue;
        }
        // Does it contain a `constructor_invocation`? → Extends.
        let ctor = (0..spec.named_child_count())
            .filter_map(|i| spec.named_child(i))
            .find(|c| c.kind() == "constructor_invocation");
        if let Some(ctor) = ctor {
            // constructor_invocation → user_type → type_identifier
            let ut = (0..ctor.named_child_count())
                .filter_map(|i| ctor.named_child(i))
                .find(|c| c.kind() == "user_type");
            if let Some(ut) = ut {
                push_user_type_name(ut, src, EdgeKind::Extends, &mut out);
            }
        } else {
            // Direct `user_type` child → Implements (interface).
            let ut = (0..spec.named_child_count())
                .filter_map(|i| spec.named_child(i))
                .find(|c| c.kind() == "user_type");
            if let Some(ut) = ut {
                push_user_type_name(ut, src, EdgeKind::Implements, &mut out);
            }
        }
    }

    out
}

/// Extract the leading type name from a `user_type` node.
///
/// In tree-sitter-kotlin-ng 1.1.0, `user_type` contains a direct `identifier`
/// named child (the bare type name).  Generic arguments, if any, are a
/// sibling `type_arguments` child (ignored here).
///
/// Examples:
///   `user_type → identifier = "Shape"`
///   `user_type → identifier = "Comparable", type_arguments = "<T>"`
fn push_user_type_name(
    user_type: Node,
    src: &[u8],
    edge_kind: EdgeKind,
    out: &mut Vec<(String, EdgeKind)>,
) {
    // In kotlin-ng the user_type's first named child is `identifier`.
    let name_node = (0..user_type.named_child_count())
        .filter_map(|i| user_type.named_child(i))
        .find(|c| matches!(c.kind(), "identifier" | "type_identifier"));
    if let Some(name_node) = name_node
        && let Ok(text) = name_node.utf8_text(src)
    {
        let bare = strip_generics(text);
        if !bare.is_empty() {
            out.push((bare.to_string(), edge_kind));
        }
    }
}

/// Strip a trailing `<…>` generic suffix from a type name.
fn strip_generics(name: &str) -> &str {
    if let Some(pos) = name.find('<') {
        name[..pos].trim()
    } else {
        name.trim()
    }
}

/// Kotlin `LanguageConfig`.
///
/// TYPE-DECLARATION CLASSIFICATION (documented limitation): tree-sitter-kotlin-ng
/// 1.1.0 overloads ONE node kind, `class_declaration`, for `class`, `interface`,
/// AND `enum class` (disambiguated only by an `interface` keyword token or an
/// `enum` `class_modifier`, not by a distinct node kind).  Because the walker
/// classifies chunks purely by node kind, all three are emitted with
/// `chunk_type = "class"`.  Singletons (`object Foo`) are `object_declaration`,
/// also classified as `class` (closest analog — a named type container).  This
/// v1 limitation is acceptable, exactly as Swift folds struct/enum into
/// `class`: chunking, `.`-joined FQNs, scope nesting, visibility, and call edges
/// are all correct; only the interface/enum/object `chunk_type` label is coarse.
/// Refining it would require a classification hook keyed on the keyword child,
/// deferred to a later task.
///
/// ENUM METHODS: because an `enum class` IS a `class_declaration`, it pushes a
/// `Class` scope frame like any other class, so enum methods nest correctly as
/// `EnumName.method` in both `fqn` and `parent_fqn` — no separate enum-kind
/// handling is needed (unlike Java/PHP, where enums are a distinct node kind).
///
/// COMPANION OBJECTS: `companion_object` has no `name` field; it is NOT chunked
/// or pushed as a scope frame, but IS recursed into so its members chunk and
/// nest under the enclosing class/object (matching `Enclosing.member` access).
///
/// FUNCTIONS: `function_declaration` covers both methods and top-level free
/// functions (Kotlin has real free functions, like PHP/Swift).  They are all
/// `method_kinds`, so a top-level `fun` is labelled `method`; this keeps the
/// leaf-chunk / call-pass behavior uniform (both Function and Method are leaf
/// chunks).  `secondary_constructor` is also a method (name `"constructor"`);
/// `primary_constructor` is inline in the class header (parameter list, not a
/// member declaration) and is NOT chunked.  Default visibility is `public`.
pub static KOTLIN: LanguageConfig = LanguageConfig {
    name: "kotlin",
    extensions: &[".kt", ".kts"],
    language_fn: language,

    // Kotlin free functions and methods share `function_declaration`; classified
    // as methods (method-centric). See struct doc comment.
    function_kinds: &[],
    // class / interface / enum class all parse as `class_declaration`; the
    // singleton `object_declaration` is also a named type container. See doc.
    class_kinds: &["class_declaration", "object_declaration"],
    method_kinds: &["function_declaration", "secondary_constructor"],
    interface_kinds: &[],
    struct_kinds: &[],
    enum_kinds: &[],
    enum_member_kinds: &["enum_entry"],
    type_alias_kinds: &["type_alias"],
    variable_kinds: &[],
    constant_kinds: &[],
    macro_kinds: &[],
    namespace_kinds: &[],
    // `package_header` is NOT a chunk and NOT a scope frame (FQNs are
    // type-rooted, consistent with Java). See kotlin_resolve_fqn.
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    import_kinds: &["import"],
    call_kinds: &["call_expression"],
    member_expr_kinds: &["navigation_expression"],
    type_ref_kinds: &["user_type"],

    comment_kinds: &["line_comment", "block_comment"],
    doc_comment_kinds: &[],

    name_field: "name",
    body_field: "body",
    params_field: "function_value_parameters",
    return_field: "",
    import_source_field: "",
    // Call callee names are resolved via kotlin_resolve_call_name (the callee is
    // a first-child identifier or navigation_expression, not a field).
    call_function_field: "",
    member_property_field: "",
    receiver_field: "",

    queries: &EMPTY_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(kotlin_resolve_name),
        resolve_fqn: Some(kotlin_resolve_fqn),
        get_visibility: Some(kotlin_visibility),
        should_recurse_into: Some(kotlin_should_recurse),
        refine_category: Some(kotlin_refine_category),
        resolve_import_specifier: Some(kotlin_resolve_import_specifier),
        resolve_call_name: Some(kotlin_resolve_call_name),
        resolve_module_path: Some(kotlin_resolve_module_path),
        nested_extraction: true,
        resolve_supertypes: Some(kotlin_resolve_supertypes),
        ..LanguageHooks::DEFAULT
    },
};
