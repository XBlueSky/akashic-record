use crate::types::*;
use crate::walker_helpers::default_name_from_field;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_scala::LANGUAGE.into()
}

static EMPTY_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &[],
    http_calls: &[],
    extra_chunks: &[],
};

/// Recursion guard for the Scala walker.
///
/// Unlike Kotlin/Swift (which overload one node kind), tree-sitter-scala 0.26.0
/// gives each type declaration its OWN node kind: `class_definition`,
/// `object_definition` (singleton), `trait_definition`, `enum_definition`
/// (Scala 3).  Their braced bodies differ:
///
/// * class / object / trait → `template_body`
/// * enum                   → `enum_body`, whose cases are wrapped one level
///   deeper in `enum_case_definitions` (the methods are direct `enum_body`
///   children).
///
/// Container nodes the walker must descend to reach member declarations:
///
/// * Root `compilation_unit` (`parent().is_none()`).
/// * `class_definition` / `object_definition` / `trait_definition` /
///   `enum_definition` — the type containers.
/// * `template_body` — the braced body of a class/object/trait.
/// * `enum_body` — the braced body of an enum (methods + `enum_case_definitions`).
/// * `enum_case_definitions` — the wrapper around `case A, B` / `case A extends ...`
///   enum-case members; descended so the cases chunk as enum members.
///
/// CRITICAL — no nested-def leak: a `def`'s body is a `block` (braced
/// `{ ... }`) or a bare `expression` (`def f = expr`).  We deliberately do NOT
/// list `block` (nor any expression kind) here, so a callable's interior is
/// never walked for chunks; nested `def`s inside a method body do not surface as
/// top-level chunks, and a callable's interior calls are reached via the
/// walker's call-pass over each leaf chunk instead.
fn scala_should_recurse(node: Node) -> bool {
    node.parent().is_none()
        || matches!(
            node.kind(),
            "class_definition"
                | "object_definition"
                | "trait_definition"
                | "enum_definition"
                | "template_body"
                | "enum_body"
                | "enum_case_definitions"
        )
}

/// Extract the callee name from a Scala `call_expression`.
///
/// A `call_expression` exposes its callee via the `function` field.  The two
/// common shapes are handled (matching the task's v1 scope):
///
/// * a bare application `f(x)` — `function` is an `identifier`; return `"f"`.
/// * a member application `obj.m(x)` / `pkg.Obj.m(x)` — `function` is a
///   `field_expression` whose `field` child is the trailing method identifier;
///   return `"m"`.
///
/// A chained call `f(x)(y)` nests `call_expression` in the `function` field; we
/// unwrap one level to reach the underlying identifier/field name.
///
/// INFIX CALLS SKIPPED (documented limitation): Scala operator/infix
/// applications `a + b`, `a max b` parse as `infix_expression`, NOT
/// `call_expression`, so they are NOT in `call_kinds` and produce no Call edge.
/// Modelling every operator as a call would flood the graph with arithmetic
/// noise; method-application calls are the unit for v1.  A bare field access
/// `obj.field` (no argument list) is a `field_expression`, also not a call.
fn scala_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
    let callee = node.child_by_field_name("function")?;
    callee_trailing_name(callee, src)
}

/// Resolve the trailing callee identifier of a `call_expression`'s `function`
/// child, unwrapping a chained `call_expression` once.
fn callee_trailing_name(callee: Node, src: &[u8]) -> Option<String> {
    match callee.kind() {
        "identifier" | "operator_identifier" => callee.utf8_text(src).ok().map(String::from),
        // Member application `obj.method` — the `field` child is the method name.
        "field_expression" => callee
            .child_by_field_name("field")
            .and_then(|f| f.utf8_text(src).ok())
            .map(String::from),
        // Chained application `f(x)(y)` / generic `f[T](x)`: the callee is itself
        // a call/generic node — unwrap its own `function` child once.
        "call_expression" | "generic_function" => callee
            .child_by_field_name("function")
            .and_then(|inner| callee_trailing_name(inner, src)),
        _ => None,
    }
}

/// Extract visibility from a Scala declaration's `access_modifier`.
///
/// Scala access modifiers live inside a `modifiers` named child, as an
/// `access_modifier` node whose leading token text is `private` or `protected`
/// (optionally qualified, e.g. `private[pkg]` — the qualifier is an
/// `access_qualifier` child we ignore; the access *level* is still private).
///
/// Scala's DEFAULT access level when no modifier is present is PUBLIC, so we
/// return `Public` for any declaration lacking an `access_modifier`.
fn scala_visibility(node: Node, src: &[u8]) -> Visibility {
    let mods = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == "modifiers");

    if let Some(mods) = mods {
        for i in 0..mods.named_child_count() {
            if let Some(child) = mods.named_child(i)
                && child.kind() == "access_modifier"
            {
                // The access level is the leading keyword token; the optional
                // `private[pkg]` qualifier does not change the level.
                if let Ok(text) = child.utf8_text(src) {
                    if text.trim_start().starts_with("protected") {
                        return Visibility::Protected;
                    }
                    if text.trim_start().starts_with("private") {
                        return Visibility::Private;
                    }
                }
            }
        }
    }
    // No explicit access modifier → public (Scala's default visibility).
    Visibility::Public
}

/// Extract the imported path from a Scala `import_declaration`.
///
/// Scala imports take several forms:
///   `import scala.collection.mutable`            (plain)
///   `import scala.collection.mutable.{Map, Set}` (grouped selectors)
///   `import java.util.*`                          (wildcard, Scala 3)
///   `import java.util._`                          (wildcard, Scala 2)
///   `import a.b.C as D`                            (renamed)
///
/// The grammar represents the dotted prefix as a sequence of `path`-field
/// `identifier` children (one per segment); we join them with `.`.  A grouped
/// import has a trailing `namespace_selectors` child (`{Map, Set}`) and a
/// wildcard import a `namespace_wildcard` child (`*` / `_`).
///
/// GROUPED/WILDCARD HANDLING (mirrors PHP/C#): for a grouped import we emit a
/// single representative specifier — the prefix joined with the FIRST selector
/// name (`scala.collection.mutable.Map`).  For a wildcard import we append `.*`
/// to the prefix.  Renames (`as`) are ignored — the edge records the imported
/// path, not its local binding.
fn scala_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    // Collect the dotted prefix from the `path`-field identifier children.
    let mut prefix_segments: Vec<String> = Vec::new();
    let mut cursor = node.walk();
    for (i, child) in node.children(&mut cursor).enumerate() {
        if node.field_name_for_child(i as u32) == Some("path")
            && child.is_named()
            && let Ok(t) = child.utf8_text(src)
        {
            prefix_segments.push(t.to_string());
        }
    }
    let prefix = prefix_segments.join(".");
    if prefix.is_empty() {
        return None;
    }

    // Wildcard import `a.b.*` / `a.b._`.
    let has_wildcard = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .any(|c| c.kind() == "namespace_wildcard");
    if has_wildcard {
        return Some(format!("{prefix}.*"));
    }

    // Grouped import `a.b.{C, D}` — use the first selector as representative.
    let selectors = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == "namespace_selectors");
    if let Some(sel) = selectors {
        let first = (0..sel.named_child_count())
            .filter_map(|i| sel.named_child(i))
            .find(|c| matches!(c.kind(), "identifier" | "type_identifier"))
            .and_then(|c| c.utf8_text(src).ok());
        if let Some(first) = first {
            return Some(format!("{prefix}.{first}"));
        }
    }

    Some(prefix)
}

/// Scala import paths are self-contained dotted identifiers
/// (`scala.collection.mutable`); pass them through without host-path rewriting.
fn scala_resolve_module_path(specifier: &str, _current_module: &str) -> Option<String> {
    Some(specifier.to_string())
}

/// Resolve the fully-qualified name for a Scala declaration.
///
/// FQN scheme: `.`-joined — `Counter.increment`, `Outer.Inner.method`.  This is
/// the internal identity key every language config and the walker's own
/// `parent_fqn` (`scope_path`) use, so a chunk's `parent_fqn` exactly equals its
/// parent chunk's `fqn` (no cross-field separator skew).
///
/// We consult the walker's scope stack first (it pushes a frame for every
/// `class_definition`/`object_definition`/`trait_definition` via the
/// class/trait kinds AND `enum_definition` via the enum-scope-frame path, so
/// enum methods/cases nest under the enum).  As a defensive fallback we climb
/// the AST to the nearest enclosing type container, mirroring the Swift /
/// Kotlin / Java resolvers.
///
/// PACKAGE-SCOPE DECISION: the `package_clause` is NOT pushed as a scope frame
/// (it is not in any `*_kinds`), so FQNs are type-rooted (`Counter.increment`),
/// NOT package-qualified (`com.example.app.Counter.increment`).  This is
/// consistent with the Java/Kotlin configs.
fn scala_resolve_fqn(node: Node, src: &[u8], scope: &ScopeStack) -> Option<String> {
    let name = default_name_from_field(node, src, "name")?;

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
        "class_definition",
        "object_definition",
        "trait_definition",
        "enum_definition",
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

/// EXT-6c-2: resolve supertypes from a Scala `class_definition`,
/// `trait_definition`, or `object_definition` via its `extends_clause`.
///
/// Grammar shape (tree-sitter-scala 0.26.0):
///   class_definition / trait_definition / object_definition
///     extends_clause                ← named child (no dedicated field)
///       extends = "extends"
///       <first type/type_identifier>    → Extends (the base class/trait)
///       with = "with"
///       <subsequent type_identifier>*   → Implements (mixed-in traits)
///
/// Strategy: scan the `extends_clause`'s children sequentially.
/// * The first supertype node (before any `with`) → Extends.
/// * Each subsequent supertype node (after a `with` sibling) → Implements.
///
/// In Scala `object X extends Y with Z`, `Y` is Extends and `Z` is
/// Implements.  For traits (`trait T extends A with B`) the same rule applies.
///
/// SUPERTYPE NODE KINDS (tree-sitter-scala 0.26.0 `extends_clause` `type` field):
/// the supertypes are NOT all bare `type_identifier` — qualified and generic
/// supertypes get their own node kinds, so matching only `type_identifier`
/// silently dropped them (e.g. `class C extends pkg.Base with mutable.Cloneable[T]`
/// would record neither supertype).  We resolve, via `scala_supertype_name`:
/// * `type_identifier`         — bare `Base`.
/// * `stable_type_identifier`  — qualified `pkg.Base`; its trailing `type_identifier`
///   named child is the simple name (`Base`).
/// * `generic_type`            — `Cloneable[T]`; its `type` field is the base
///   (a `type_identifier` or `stable_type_identifier`), resolved recursively.
///
/// The literal `"type"` arm in the old code was a FIELD name, never a node kind,
/// so it never matched anything — these kinds are the actual fix.
fn scala_resolve_supertypes(node: Node, src: &[u8]) -> Vec<(String, EdgeKind)> {
    let mut out = Vec::new();
    if !matches!(
        node.kind(),
        "class_definition" | "trait_definition" | "object_definition"
    ) {
        return out;
    }

    // Find the `extends_clause` named child (by kind, not field).
    let mut cursor = node.walk();
    let extends_clause = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "extends_clause");
    let clause = match extends_clause {
        Some(c) => c,
        None => return out,
    };

    // Walk all children (named + unnamed) of the extends_clause sequentially.
    let mut after_with = false;
    let mut first_type_seen = false;
    let mut ec = clause.walk();
    for child in clause.children(&mut ec) {
        match child.kind() {
            "with" => {
                after_with = true;
            }
            // Resolve bare, qualified (stable_type_identifier) and generic
            // (generic_type) supertypes — not just bare `type_identifier`.
            "type_identifier" | "stable_type_identifier" | "generic_type" => {
                let name = match scala_supertype_name(child, src) {
                    Some(n) => n,
                    None => continue,
                };
                if name.is_empty() {
                    continue;
                }
                let edge = if !first_type_seen && !after_with {
                    first_type_seen = true;
                    EdgeKind::Extends
                } else {
                    EdgeKind::Implements
                };
                out.push((name, edge));
            }
            _ => {}
        }
    }

    out
}

/// Resolve the simple supertype name from an `extends_clause` type node.
///
/// Handles the supertype node kinds that appear in a Scala `extends_clause`:
/// * `type_identifier`        → its raw text (`Base`).
/// * `stable_type_identifier` → the trailing `type_identifier` named child of a
///   qualified path (`pkg.Base` → `Base`); the leading segment is a
///   `stable_identifier`/`identifier` we skip.
/// * `generic_type`           → recurse into its `type` field (the base type,
///   itself a `type_identifier` or `stable_type_identifier`), discarding
///   `type_arguments` (`Cloneable[T]` → `Cloneable`).
///
/// As a defensive fallback we still strip a trailing `[…]`/`(…)` from the raw
/// text, so an unexpected shape never leaks generic/constructor noise.
fn scala_supertype_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" => node.utf8_text(src).ok().map(String::from),
        // Qualified `pkg.Base` — the trailing named child is the simple type name.
        "stable_type_identifier" => {
            let mut c = node.walk();
            // `.last()` (not `.next_back()`) — the filtered named-children iterator
            // is not DoubleEndedIterator.
            let last = node
                .named_children(&mut c)
                .filter(|n| n.kind() == "type_identifier")
                .last();
            match last {
                Some(n) => n.utf8_text(src).ok().map(String::from),
                // Fall back to the trailing dotted segment of the raw text.
                None => node.utf8_text(src).ok().map(|t| {
                    scala_strip_generics(t)
                        .rsplit('.')
                        .next()
                        .unwrap_or("")
                        .to_string()
                }),
            }
        }
        // Generic `Base[T]` — resolve the base via the `type` field.
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|base| scala_supertype_name(base, src)),
        // Defensive fallback: strip generics/ctor args and take the trailing segment.
        _ => node.utf8_text(src).ok().map(|t| {
            scala_strip_generics(t)
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_string()
        }),
    }
}

/// Strip a trailing `[…]` (Scala generic) or `(…)` suffix from a type name.
fn scala_strip_generics(name: &str) -> &str {
    // Scala generics use `[T]`, not `<T>`.
    let name = if let Some(pos) = name.find('[') {
        &name[..pos]
    } else {
        name
    };
    // Also strip constructor args `(…)` for Scala 3 enum cases.
    let name = if let Some(pos) = name.find('(') {
        &name[..pos]
    } else {
        name
    };
    name.trim()
}

/// Scala `LanguageConfig`.
///
/// TYPE-DECLARATION CLASSIFICATION: tree-sitter-scala 0.26.0 gives each type its
/// own node kind, so classification is clean (no Kotlin/Swift-style overloading):
///
/// * `class_definition`  → `class`  (a `case class` is a `class_definition` with
///   a `case` modifier — same node kind, so it is also classified `class`).
/// * `trait_definition`  → `trait`  (Scala traits are interface-like mixins; the
///   shared config HAS `trait_kinds`/`ScopeKind::Trait`, preferred over folding
///   into `interface`).
/// * `object_definition` → `class`  (DOCUMENTED CHOICE: an `object` is a
///   singleton — there is no dedicated "singleton"/"module-object" chunk type, so
///   it is labelled `class`, the closest named-type-container analog; this also
///   covers companion objects and top-level entry objects.  Its members nest
///   under `Object.member`, matching Scala access semantics).
/// * `enum_definition`   → `enum`   (Scala 3 enums; the 0.26 grammar parses them).
///
/// FUNCTIONS: `function_definition` (`def f = ...`, with a body) covers methods,
/// object/top-level defs.  `function_declaration` is an ABSTRACT def (`def f:
/// T`, no body — e.g. a trait's abstract method); both are `method_kinds` so an
/// abstract member still chunks.  Scala has no truly free top-level `def` outside
/// an object/package in idiomatic code; all defs are method-centric, so labelling
/// them `method` (not `function`) is correct and keeps leaf-chunk/call-pass
/// behavior uniform.
///
/// VAL/VAR DECISION (documented): `val_definition`/`var_definition` (and their
/// abstract `val_declaration`/`var_declaration` forms) are NOT chunked
/// (`variable_kinds`/`constant_kinds` empty).  Members and top-level vals would
/// flood the output with noise; methods are the unit of interest, matching the
/// method-centric stance of the Kotlin/Swift configs.
///
/// ENUM CASES: `simple_enum_case`/`full_enum_case` are `enum_member_kinds`; they
/// are reached by recursing `enum_body` → `enum_case_definitions`.  Enum methods
/// (direct `enum_body` children) nest as `Enum.method` in both `fqn` and
/// `parent_fqn` because `enum_definition` pushes an `Enum` scope frame.
///
/// CALLS: `call_expression` only (see `scala_resolve_call_name`); infix
/// operator calls (`infix_expression`) are intentionally skipped.
pub static SCALA: LanguageConfig = LanguageConfig {
    name: "scala",
    extensions: &[".scala", ".sc"],
    language_fn: language,

    // Scala defs are method-centric (always inside a type/object); classified as
    // methods. See struct doc comment.
    function_kinds: &[],
    // `object_definition` is a singleton type container (documented as `class`).
    class_kinds: &["class_definition", "object_definition"],
    // `function_definition` = concrete def; `function_declaration` = abstract def.
    method_kinds: &["function_definition", "function_declaration"],
    interface_kinds: &[],
    struct_kinds: &[],
    enum_kinds: &["enum_definition"],
    enum_member_kinds: &["simple_enum_case", "full_enum_case"],
    type_alias_kinds: &["type_definition"],
    variable_kinds: &[],
    constant_kinds: &[],
    macro_kinds: &[],
    namespace_kinds: &[],
    // `package_clause` is NOT a chunk and NOT a scope frame (FQNs are
    // type-rooted, consistent with Java/Kotlin). See scala_resolve_fqn.
    module_kinds: &[],
    // Scala traits are interface-like; use the dedicated trait category.
    trait_kinds: &["trait_definition"],
    impl_kinds: &[],

    import_kinds: &["import_declaration"],
    call_kinds: &["call_expression"],
    // Member-select callee names are resolved via scala_resolve_call_name.
    member_expr_kinds: &["field_expression"],
    type_ref_kinds: &["type_identifier"],

    comment_kinds: &["comment", "block_comment"],
    doc_comment_kinds: &[],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "return_type",
    import_source_field: "",
    // Call callee names are resolved via scala_resolve_call_name (the callee is
    // a `function`-field identifier or field_expression).
    call_function_field: "function",
    member_property_field: "field",
    receiver_field: "",

    queries: &EMPTY_QUERIES,
    hooks: LanguageHooks {
        resolve_fqn: Some(scala_resolve_fqn),
        get_visibility: Some(scala_visibility),
        should_recurse_into: Some(scala_should_recurse),
        resolve_import_specifier: Some(scala_resolve_import_specifier),
        resolve_call_name: Some(scala_resolve_call_name),
        resolve_module_path: Some(scala_resolve_module_path),
        nested_extraction: true,
        resolve_supertypes: Some(scala_resolve_supertypes),
        ..LanguageHooks::DEFAULT
    },
};
