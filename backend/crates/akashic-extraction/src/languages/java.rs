use crate::types::*;
use crate::walker_helpers::default_name_from_field;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_java::LANGUAGE.into()
}

// ─── Spring MVC framework-route queries ───────────────────────────────────
//
// Matches `@GetMapping("/path")` / `@PostMapping` / `@RequestMapping` etc.
// on a `method_declaration`.
//
// AST shape (tree-sitter-java):
//
//   method_declaration                             ← @route.reg
//     modifiers
//       annotation
//         identifier  field=name  e.g. "GetMapping" ← @route.method
//         annotation_argument_list  field=arguments
//           string_literal                         ← @route.path
//     identifier  field=name  e.g. "listUsers"    ← @route.handler
//
// @route.method: annotation name (e.g. "GetMapping"). The runner upper-cases
// it → "GETMAPPING". A #match? predicate restricts to *Mapping annotations
// only, avoiding @Override / @Transactional etc.
// @route.path: string_literal including surrounding quotes; runner strips them.
static JAVA_FRAMEWORK_ROUTES: [QueryDef; 1] = [QueryDef {
    name: "spring_route",
    source: "(method_declaration \
  (modifiers \
    (annotation \
      name: (identifier) @route.method \
      (#match? @route.method \"Mapping$\") \
      arguments: (annotation_argument_list \
        (string_literal) @route.path))) \
  name: (identifier) @route.handler) @route.reg",
    compiled: std::sync::OnceLock::new(),
}];

static JAVA_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &JAVA_FRAMEWORK_ROUTES,
    http_calls: &[],
    extra_chunks: &[],
};

/// Recurse into the root plus all container nodes between a class/interface/enum
/// declaration and its member declarations:
///
/// * `class_declaration` / `interface_declaration` / `enum_declaration` —
///   the top-level containers.
/// * `class_body` / `interface_body` — the braced body of a class or interface;
///   this is the intermediate node the walker must descend through to reach
///   `method_declaration` / `constructor_declaration` children.
/// * `enum_body` / `enum_body_declarations` — `enum_body` wraps the enum
///   constants and an optional `enum_body_declarations` block; that block is
///   the container for enum methods.
///
/// We deliberately do NOT list `block` or `constructor_body` here: those are
/// method / constructor bodies, and recursing into them would surface nested
/// classes as top-level chunks (breaking the "top-level only" contract).
fn java_should_recurse(node: Node) -> bool {
    node.parent().is_none()
        || matches!(
            node.kind(),
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "class_body"
                | "interface_body"
                | "enum_body"
                | "enum_body_declarations"
        )
}

/// Extract the callee name from a Java call-kind node.
///
/// Two node kinds are registered in `call_kinds`:
///
/// * `method_invocation` — exposes the called method via a `name` field
///   (the bare method identifier) and an optional `object` field for the
///   receiver (`obj.method()`).  There is no wrapping `call_function_field`
///   child (unlike TypeScript / C++ which have a `function` field), so the
///   default `default_callee_name` cannot be used here.
///   `add(1, 2)` → `"add"`; `obj.method(3)` → `"method"`.
///
/// * `object_creation_expression` — a `new Foo()` constructor call. This node
///   has NO `name` field; the constructed class lives in the `type` field
///   (a `_simple_type`).  Reading `name` here returns `None`, which is why
///   every `new Foo()` was silently dropping its Call edge before this fix
///   (closes finding: object_creation_expression read non-existent `name`).
///   We read the `type` field and extract the base/trailing class name so a
///   Call edge to the constructed class is emitted:
///
/// ```text
/// new Foo()     -> type_identifier        -> "Foo"
/// new Foo<T>()  -> generic_type           -> "Foo" (T skipped)
/// new pkg.Bar() -> scoped_type_identifier -> "Bar" (trailing)
/// ```
fn java_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
    if node.kind() == "object_creation_expression" {
        let type_node = node.child_by_field_name("type")?;
        return java_base_type_name(type_node, src);
    }
    // method_invocation (and any other call kind that exposes a `name` field).
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .map(String::from)
}

/// Resolve the base / trailing simple type name from a `_simple_type` node as
/// it appears in the `type` field of an `object_creation_expression`.
///
/// Grammar shapes handled (tree-sitter-java 0.23.x, verified via node-types):
///
/// * `type_identifier`           — leaf; the name itself (`"Foo"`).
/// * `generic_type`              — children are the base type
///   (`type_identifier` or `scoped_type_identifier`) followed by a
///   `type_arguments` node.  The base type is the first non-`type_arguments`
///   child; the `type_arguments` subtree (the `<T>` params) is skipped so we
///   never return a generic argument by mistake.
/// * `scoped_type_identifier`    — `pkg.Bar`; children are the qualifier
///   (nested `scoped_type_identifier`) plus the trailing simple
///   `type_identifier`.  We return the trailing `type_identifier` (`"Bar"`).
///
/// Returns `None` for non-class constructed types (e.g. primitive/array forms
/// that never appear as `new` targets), which correctly suppresses the edge.
fn java_base_type_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" => node.utf8_text(src).ok().map(String::from),
        "generic_type" => {
            // First child that is a type name (skip the `type_arguments` node
            // holding `<...>` params); recurse so `pkg.Bar<T>` resolves to "Bar".
            let mut cursor = node.walk();
            let base = node
                .named_children(&mut cursor)
                .find(|c| matches!(c.kind(), "type_identifier" | "scoped_type_identifier"));
            base.and_then(|b| java_base_type_name(b, src))
        }
        "scoped_type_identifier" => {
            // `pkg.Bar` / `a.b.Bar`: the trailing simple name is the LAST
            // `type_identifier` child. `named_children` advances a shared cursor
            // forward, so collect in order and take the last (NOT `next_back`,
            // which would mis-drive the cursor).
            let mut cursor = node.walk();
            let last = node
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "type_identifier")
                .last();
            last.and_then(|leaf| leaf.utf8_text(src).ok())
                .map(String::from)
        }
        _ => None,
    }
}

/// Extract visibility from a Java declaration's `modifiers` child node.
///
/// Java modifier tokens are direct (unnamed) children of the `modifiers`
/// node — they are keyword nodes whose `kind()` is the token text
/// (e.g. `"public"`, `"private"`, `"protected"`).
///
/// When no access modifier is present the declaration is package-private
/// (Java's default visibility).
fn java_visibility(node: Node, _src: &[u8]) -> Visibility {
    // Find the `modifiers` named child of the declaration node.
    let mods = match (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == "modifiers")
    {
        Some(m) => m,
        None => return Visibility::PackagePrivate,
    };

    // Modifier keyword tokens are unnamed children of the `modifiers` node.
    for i in 0..mods.child_count() {
        if let Some(child) = mods.child(i) {
            match child.kind() {
                "public" => return Visibility::Public,
                "private" => return Visibility::Private,
                "protected" => return Visibility::Protected,
                _ => {}
            }
        }
    }
    // No access modifier found → package-private.
    Visibility::PackagePrivate
}

/// Extract the imported module path from an `import_declaration` node.
///
/// Java imports are of the form:
///   `import java.util.List;`
///   `import static java.util.Collections.singletonList;`
///   `import java.util.*;`
///
/// The grammar represents the imported name as the first named child —
/// a `scoped_identifier` whose text is the fully-qualified name
/// (e.g. `"java.util.List"` or `"java.util.Collections.singletonList"`).
/// For wildcards (`java.util.*`) the `scoped_identifier` is the package
/// prefix (`"java.util"`) and an `asterisk` child follows it.
///
/// We pass the scoped_identifier text through as-is (dots retained).
/// Wildcard imports get a `.*` suffix to signal the glob pattern.
fn java_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    let scoped = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == "scoped_identifier")?;

    let base = scoped.utf8_text(src).ok()?;
    // Detect wildcard: a sibling `asterisk` named child means `import pkg.*`
    let has_glob = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .any(|c| c.kind() == "asterisk");

    if has_glob {
        Some(format!("{base}.*"))
    } else {
        Some(base.to_string())
    }
}

/// Java imports are already fully-qualified (`java.util.List`); pass them
/// through without path rewriting.  Unlike TypeScript (which has relative
/// `./` imports resolved against the current module), Java package names are
/// self-contained and do not need host-path resolution.
fn java_resolve_module_path(specifier: &str, _current_module: &str) -> Option<String> {
    Some(specifier.to_string())
}

/// Resolve the fully-qualified name for a Java declaration.
///
/// The walker's default FQN logic is scope-stack-based, but the scope stack
/// only records `class_kinds`, `interface_kinds`, `struct_kinds`, etc. — it
/// does NOT record `enum_declaration` as a scope frame (the walker's
/// `maybe_push_scope` function does not include enum kinds).  As a result,
/// methods inside an enum body would receive a bare name FQN (e.g. `"lower"`)
/// rather than the expected `"Color.lower"`.
///
/// This hook fixes the gap by walking up the AST from the declaration node
/// to find the nearest enclosing class / interface / enum declaration and
/// prepending its name, regardless of the scope stack.  The scope stack is
/// still consulted as a fallback for deeply-nested classes (though Java
/// rarely nests more than one level deep in practice).
///
/// Priority:
/// 1. If the scope stack already has frames (e.g. the node is inside a class
///    that the walker correctly tracked), use those — they are correct.
/// 2. Otherwise, climb the AST tree to find an enclosing named container.
fn java_resolve_fqn(node: Node, src: &[u8], scope: &ScopeStack) -> Option<String> {
    let name = default_name_from_field(node, src, "name")?;

    // If the walker already pushed a scope frame for this node's container
    // (i.e. it was inside a tracked class/interface), use those frames.
    if !scope.is_empty() {
        let prefix = scope
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        return Some(format!("{prefix}.{name}"));
    }

    // Scope stack is empty — the node may be inside an enum body (whose
    // enum_declaration the walker does not push onto the scope stack).
    // Walk up the AST to find the nearest enclosing named container.
    let container_kinds = [
        "class_declaration",
        "interface_declaration",
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

    // No enclosing container found (top-level declaration).
    Some(name)
}

/// EXT-6c: resolve a Java class or interface's explicit supertypes.
///
/// For `class_declaration`:
///   * `superclass` field → `type_identifier` → (name, Extends)  [0 or 1]
///   * `interfaces` field (`super_interfaces` node) → `type_list` →
///     each `type_identifier` → (name, Implements)
///
/// For `interface_declaration`:
///   * `extends_interfaces` named child (NOT a field) → `type_list` →
///     each `type_identifier` → (name, Extends) [interfaces extending interfaces]
///
/// Bare leaf names only; generics (`List<E>`) stripped at `<`.
fn java_resolve_supertypes(node: Node, src: &[u8]) -> Vec<(String, EdgeKind)> {
    let mut out = Vec::new();
    match node.kind() {
        "class_declaration" => {
            // Superclass: `extends Foo` → field "superclass" → type_identifier
            if let Some(superclass) = node.child_by_field_name("superclass") {
                collect_type_identifiers(superclass, src, EdgeKind::Extends, &mut out);
            }
            // Interfaces: `implements Foo, Bar` → field "interfaces" (super_interfaces)
            // → type_list → type_identifier*
            if let Some(interfaces) = node.child_by_field_name("interfaces") {
                // interfaces is a `super_interfaces` node; its type_list child holds
                // the comma-separated interface names.
                let mut cursor = interfaces.walk();
                for child in interfaces.named_children(&mut cursor) {
                    if child.kind() == "type_list" {
                        let mut tc = child.walk();
                        for ty in child.named_children(&mut tc) {
                            if ty.kind() == "type_identifier" {
                                push_bare_name(ty, src, EdgeKind::Implements, &mut out);
                            }
                        }
                    }
                }
            }
        }
        "interface_declaration" => {
            // `interface Foo extends Bar, Baz` → `extends_interfaces` named child
            // (NOT a field — found by kind).  Its `type_list` child holds the names.
            //
            // Grammar shape (empirically verified):
            //   interface_declaration
            //     extends_interfaces          ← named child (no field name)
            //       extends = "extends"
            //       type_list
            //         type_identifier = "Bar"
            //         type_identifier = "Baz"
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "extends_interfaces" {
                    collect_type_identifiers(child, src, EdgeKind::Extends, &mut out);
                }
            }
        }
        _ => {}
    }
    out
}

/// Collect all `type_identifier` leaves from a node subtree, emitting `edge_kind`.
fn collect_type_identifiers(
    node: Node,
    src: &[u8],
    edge_kind: EdgeKind,
    out: &mut Vec<(String, EdgeKind)>,
) {
    if node.kind() == "type_identifier" {
        push_bare_name(node, src, edge_kind, out);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_type_identifiers(child, src, edge_kind, out);
    }
}

/// Push the bare (generics-stripped) text of a `type_identifier` node.
fn push_bare_name(node: Node, src: &[u8], edge_kind: EdgeKind, out: &mut Vec<(String, EdgeKind)>) {
    if let Ok(text) = node.utf8_text(src) {
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

pub static JAVA: LanguageConfig = LanguageConfig {
    name: "java",
    extensions: &[".java"],
    language_fn: language,

    // Java has no free-standing functions; all code lives inside methods.
    function_kinds: &[],
    class_kinds: &["class_declaration"],
    // Both regular methods and constructors are method-level chunks.
    method_kinds: &["method_declaration", "constructor_declaration"],
    interface_kinds: &["interface_declaration"],
    struct_kinds: &[],
    enum_kinds: &["enum_declaration"],
    // Enum constants are enum_constant nodes (name field = identifier).
    enum_member_kinds: &["enum_constant"],
    type_alias_kinds: &[],
    variable_kinds: &[],
    constant_kinds: &[],
    macro_kinds: &[],
    namespace_kinds: &[],
    // `package_declaration` is NOT a chunk; it's metadata only.
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    import_kinds: &["import_declaration"],
    // Method calls + object_creation_expression (`new Foo()`).
    call_kinds: &["method_invocation", "object_creation_expression"],
    member_expr_kinds: &[],
    type_ref_kinds: &["type_identifier"],

    comment_kinds: &["line_comment", "block_comment"],
    // Javadoc `/** ... */` (block_comment); filtered from ordinary comments by
    // the walker's `is_doc_comment` marker predicate. See extract_leading_doc.
    doc_comment_kinds: &["block_comment"],

    // All Java declarations expose their name via the `name` field.
    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "type",
    import_source_field: "",
    // Java method_invocation has no `function` field (handled via hook).
    call_function_field: "",
    member_property_field: "",
    receiver_field: "",

    queries: &JAVA_QUERIES,
    hooks: LanguageHooks {
        resolve_fqn: Some(java_resolve_fqn),
        get_visibility: Some(java_visibility),
        should_recurse_into: Some(java_should_recurse),
        resolve_import_specifier: Some(java_resolve_import_specifier),
        resolve_call_name: Some(java_resolve_call_name),
        resolve_module_path: Some(java_resolve_module_path),
        resolve_supertypes: Some(java_resolve_supertypes),
        ..LanguageHooks::DEFAULT
    },
};
