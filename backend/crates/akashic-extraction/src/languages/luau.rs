use crate::types::*;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_luau::LANGUAGE.into()
}

static EMPTY_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &[],
    http_calls: &[],
    extra_chunks: &[],
};

/// Read the `name` field of a `function_declaration` and return the *trailing
/// segment* as the chunk's short name.
///
/// Luau's `function_declaration` is structurally identical to Lua's: the `name`
/// field is `identifier`, `dot_index_expression`, or `method_index_expression`.
/// Typed signatures (`function f(a: number): string`) add `parameter` type
/// children and a trailing return-`type` child, but do not change the `name`
/// field shape.  We return only the final identifier; the dotted prefix is
/// carried by the `fqn` (see [`luau_resolve_fqn`]).
fn luau_resolve_name(node: Node, src: &[u8]) -> Option<String> {
    let name_node = node.child_by_field_name("name")?;
    match name_node.kind() {
        "dot_index_expression" => name_node
            .child_by_field_name("field")
            .and_then(|f| f.utf8_text(src).ok())
            .map(String::from),
        "method_index_expression" => name_node
            .child_by_field_name("method")
            .and_then(|m| m.utf8_text(src).ok())
            .map(String::from),
        _ => name_node.utf8_text(src).ok().map(String::from),
    }
}

/// Resolve the fully-qualified name for a Luau `function_declaration`.
///
/// FQN SCHEME (documented choice, identical to Lua): FQNs are joined with `.`.
/// Luau, like Lua, has no syntactic module / namespace construct (a "module" is
/// a runtime table), so the FQN is derived entirely from the function name node:
///
///   - `function foo()`        → "foo"
///   - `local function bar()`  → "bar"
///   - `function M.foo()`      → "M.foo"   (dot_index_expression text)
///   - `function M:bar()`      → "M.bar"   (method_index: ':' rewritten to '.')
///
/// `type_definition` chunks (type aliases) fall through to the default FQN path
/// (their bare name), which is correct — they live at module top level.
fn luau_resolve_fqn(node: Node, src: &[u8], scope: &ScopeStack) -> Option<String> {
    // Only function declarations need the dotted/colon rewrite; type aliases
    // (and anything else) use the default name-based FQN.
    if node.kind() != "function_declaration" {
        let name = luau_resolve_name(node, src).or_else(|| {
            node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())
                .map(String::from)
        })?;
        return crate::walker_helpers::default_fqn(&name, scope);
    }
    let name_node = node.child_by_field_name("name")?;
    let text = name_node.utf8_text(src).ok()?;
    // EXT-5: prefix the enclosing function scope so a NAMED nested function gets
    // `outer.inner` (top-level fns have an empty scope → bare), via the same
    // scope-aware helper the type-alias branch above uses.
    crate::walker_helpers::default_fqn(&text.replace(':', "."), scope)
}

/// Extract the callee name from a Luau `function_call` node.
///
/// Identical shape to Lua: the callee is the `name` field — `identifier`,
/// `dot_index_expression` (→ trailing `field`), or `method_index_expression`
/// (→ `method`); otherwise fall back to the whole callee text.
fn luau_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
    let callee = node.child_by_field_name("name")?;
    match callee.kind() {
        "dot_index_expression" => callee
            .child_by_field_name("field")
            .and_then(|f| f.utf8_text(src).ok())
            .map(String::from),
        "method_index_expression" => callee
            .child_by_field_name("method")
            .and_then(|m| m.utf8_text(src).ok())
            .map(String::from),
        _ => callee.utf8_text(src).ok().map(String::from),
    }
}

/// Reclassify `require("mod")` / `require "mod"` calls as imports.
///
/// IMPORT DESIGN DECISION (Option A, identical to Lua/Ruby): `function_call` is
/// listed in BOTH `import_kinds` and `call_kinds`; this resolver returns the
/// string argument ONLY when the callee is the bare `require` identifier, else
/// `None` (the call stays a `Call` edge).  No walker changes required.
fn luau_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    let callee = node.child_by_field_name("name")?;
    if callee.kind() != "identifier" || callee.utf8_text(src).ok()? != "require" {
        return None;
    }
    let args = node.child_by_field_name("arguments")?;
    let mut string_arg = None;
    for i in 0..args.named_child_count() {
        if let Some(c) = args.named_child(i)
            && c.kind() == "string"
        {
            string_arg = Some(c);
            break;
        }
    }
    let target = string_arg?;
    let spec = target
        .child_by_field_name("content")
        .and_then(|c| c.utf8_text(src).ok())
        .map(String::from)
        .or_else(|| {
            target.utf8_text(src).ok().map(|s| {
                s.trim_matches(|c: char| matches!(c, '\'' | '"' | '[' | ']'))
                    .to_string()
            })
        })?;
    Some(spec)
}

/// Luau `require` paths are passed through verbatim (resolved at runtime).
fn luau_resolve_module_path(specifier: &str, _current_module: &str) -> Option<String> {
    Some(specifier.to_string())
}

/// Visibility for Luau function declarations.
///
/// DESIGN CHOICE (identical to Lua): `local function f()` is file-local →
/// `Private`; a bare `function f()` / `function M.foo()` is global / a field on
/// an exported table → `Public`.  The hook also fires for `type_definition`
/// chunks: a top-level type alias has no `local` keyword so it resolves to
/// `Public` (there is no `local type` construct in Luau), which is the correct
/// default for a module-level type alias.  `local` is detected via the unnamed
/// `local` child.
fn luau_visibility(node: Node, _src: &[u8]) -> Visibility {
    let mut cursor = node.walk();
    let is_local = node
        .children(&mut cursor)
        .any(|c| !c.is_named() && c.kind() == "local");
    if is_local {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

/// Recursion guard for the Luau walker (identical strategy to Lua).
///
/// CRITICAL zero-nested-leak invariant: a `function_declaration`'s body is a
/// `block`; we must NOT descend into it so nested closures do not leak as
/// top-level chunks and interior calls are not double-counted (the call-pass
/// covers them).  A `type_definition` body (`object_type` etc.) is likewise not
/// descended — a type alias is a leaf chunk.
///
/// Rules:
///   - root `chunk` (no parent) → recurse (reach top-level declarations).
///   - `variable_declaration` / `assignment_statement` / `expression_list` →
///     recurse, so a top-level `local x = require "c"` is reached.
///   - everything else → do NOT recurse.
fn luau_should_recurse(node: Node) -> bool {
    if node.parent().is_none() {
        return true;
    }
    matches!(
        node.kind(),
        "variable_declaration" | "assignment_statement" | "expression_list"
    )
}

/// FQN scheme: dotted (`M.foo`, `M.bar` for `M:bar`); type aliases use their
/// bare name.  `local function` → `Private`; everything else → `Public`.
/// Function bodies (`block`) and type-alias bodies are never descended (see
/// `luau_should_recurse`): nested closures do not leak, interior calls go via
/// the call-pass.  Luau = typed Lua superset, so it adds `type_definition`
/// (type aliases) and typed parameters/return types on top of the Lua config.
pub static LUAU: LanguageConfig = LanguageConfig {
    name: "luau",
    extensions: &[".luau"],
    language_fn: language,

    function_kinds: &["function_declaration"],
    class_kinds: &[],
    method_kinds: &[],
    interface_kinds: &[],
    struct_kinds: &[],
    enum_kinds: &[],
    enum_member_kinds: &[],
    // Luau type aliases: `type Id = string`, `type Point = { ... }`.
    type_alias_kinds: &["type_definition"],
    variable_kinds: &[],
    constant_kinds: &[],
    macro_kinds: &[],
    namespace_kinds: &[],
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    import_kinds: &["function_call"],
    call_kinds: &["function_call"],
    member_expr_kinds: &["dot_index_expression", "method_index_expression"],
    type_ref_kinds: &[],

    comment_kinds: &["comment"],
    doc_comment_kinds: &[],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "",
    import_source_field: "",
    call_function_field: "name",
    member_property_field: "field",
    receiver_field: "",

    queries: &EMPTY_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(luau_resolve_name),
        resolve_fqn: Some(luau_resolve_fqn),
        get_visibility: Some(luau_visibility),
        should_recurse_into: Some(luau_should_recurse),
        resolve_import_specifier: Some(luau_resolve_import_specifier),
        resolve_call_name: Some(luau_resolve_call_name),
        resolve_module_path: Some(luau_resolve_module_path),
        nested_extraction: true,
        ..LanguageHooks::DEFAULT
    },
};
