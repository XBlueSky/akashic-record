use crate::types::*;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_lua::LANGUAGE.into()
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
/// The `name` field is one of three node kinds:
///   - `identifier`            → `function foo()` / `local function bar()` → "foo"/"bar"
///   - `dot_index_expression`  → `function M.foo()`  (table `M`, field `foo`) → "foo"
///   - `method_index_expression` → `function M:bar()` (table `M`, method `bar`) → "bar"
///
/// For the dotted / colon forms we return only the final identifier so the
/// short `name` is uniform ("foo"), while the dotted prefix is carried by the
/// `fqn` (see [`lua_resolve_fqn`]).  Anonymous `function_definition` nodes have
/// no `name` field and are intentionally not classified as chunks.
fn lua_resolve_name(node: Node, src: &[u8]) -> Option<String> {
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

/// Resolve the fully-qualified name for a Lua `function_declaration`.
///
/// FQN SCHEME (documented choice): FQNs are joined with `.` — matching every
/// other language config and the walker's own `parent_fqn` (`scope_path`).
/// Lua has NO syntactic module / namespace construct (a "module" is a runtime
/// table such as `local M = {}` returned at end-of-file), so there are no scope
/// frames to consult — the FQN is derived entirely from the function's own
/// name node:
///
///   - `function foo()`        → "foo"
///   - `local function bar()`  → "bar"
///   - `function M.foo()`      → "M.foo"        (dot_index_expression text)
///   - `function M:bar()`      → "M.bar"        (method_index: ':' rewritten to '.')
///   - `function A.B.c()`      → "A.B.c"        (nested dot_index, text as-is)
///
/// The colon form (`M:bar`, an implicit-`self` method) is normalised to the
/// dotted form `M.bar` so callers / resolvers see one consistent separator and
/// a method's `fqn` does not depend on call syntax.
fn lua_resolve_fqn(node: Node, src: &[u8], scope: &ScopeStack) -> Option<String> {
    let name_node = node.child_by_field_name("name")?;
    let text = name_node.utf8_text(src).ok()?;
    // `dot_index_expression` text is already "M.foo"; `method_index_expression`
    // text is "M:bar" → rewrite the single ':' separator to '.'.
    let name = text.replace(':', ".");
    // EXT-5: prefix the enclosing function scope so a NAMED nested `local
    // function` gets `outer.inner` (top-level fns have an empty scope → bare).
    crate::walker_helpers::default_fqn(&name, scope)
}

/// Extract the callee name from a Lua `function_call` node.
///
/// The callee lives in the `name` field (NOT `function`, unlike most C-family
/// grammars).  It is one of:
///   - `identifier`              → `foo(...)`       → "foo"
///   - `dot_index_expression`    → `obj.method(...)` → trailing `field` → "method"
///   - `method_index_expression` → `obj:method(...)` → `method` → "method"
///   - `function_call` / `parenthesized_expression` → chained / computed callee;
///     fall back to the whole callee text.
fn lua_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
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
/// IMPORT DESIGN DECISION (Option A, mirroring Ruby): Lua has no import
/// *statement* — `require("a.b")` and the paren-less `require "c"` are ordinary
/// `function_call` nodes with a single string-literal argument.  To let Lua
/// participate in the IMPORTS_FROM graph we list `function_call` in BOTH
/// `import_kinds` and `call_kinds` and use this specifier resolver as a FILTER:
///
///   - callee is the bare identifier `require` → return the unquoted string arg
///     → walker emits an `Import` edge.
///   - ANY other call (`helper`, `M.foo`, `obj:m`, ...) → return `None`, so the
///     walker emits no import edge.  Those calls are still captured as `Call`
///     edges by the call-pass.
///
/// This needs NO walker changes — the walker already treats a `None` return
/// from `resolve_import_specifier` as "not an import".  Both call forms wrap the
/// argument in an `arguments` node whose first named child is the `string`.
fn lua_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    let callee = node.child_by_field_name("name")?;
    // Only a bare `require` identifier qualifies; `foo.require(...)` does not.
    if callee.kind() != "identifier" || callee.utf8_text(src).ok()? != "require" {
        return None;
    }
    let args = node.child_by_field_name("arguments")?;
    // First string-literal argument (paren'd or paren-less both nest it here).
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
    // The unquoted content is the `content` field (`string_content`); fall back
    // to the whole string text with surrounding quote/bracket delimiters trimmed.
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

/// Lua `require` paths are passed through verbatim.
///
/// `require("a.b")` names a module by its dotted package path (resolved at
/// runtime via `package.path`); there is no file-relative rewriting to do
/// without a load-path, so we keep the specifier exactly as written.
fn lua_resolve_module_path(specifier: &str, _current_module: &str) -> Option<String> {
    Some(specifier.to_string())
}

/// Visibility for Lua function declarations.
///
/// DESIGN CHOICE: Lua has no access modifiers.  A `local function f()` is
/// file-local (cannot be referenced from another chunk/file), so we model it as
/// `Private`.  A bare `function f()` / `function M.foo()` defines a global (or a
/// field on an exported table) and is reachable from outside the file, so we
/// model it as `Public`.  We detect `local` by the presence of the `local`
/// keyword as an unnamed child of the `function_declaration` (the grammar emits
/// it as `["local", "function", ... "end"]`).
fn lua_visibility(node: Node, _src: &[u8]) -> Visibility {
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

/// Recursion guard for the Lua walker.
///
/// CRITICAL zero-nested-leak invariant: Lua nests functions heavily (closures,
/// functions returning functions).  A `function_declaration`'s body is a
/// `block` node; we must NOT descend into it, otherwise nested closures would
/// leak as top-level chunks and a function's interior calls would be
/// double-counted (the call-pass already covers them).
///
/// Rules:
///   - root `chunk` (no parent) → recurse (reach top-level declarations).
///   - everything else → do NOT recurse.
///
/// Lua has no syntactic scope containers (no class/module/namespace nodes), so
/// there is nothing else to descend into at the top level.  Top-level
/// `require` calls live directly under `chunk` and are reached by the main
/// walk; a top-level `local x = require "c"` is a `variable_declaration` —
/// because we do not recurse into it, the embedded `require` call is reached by
/// the `variable_declaration`'s own... no: a `variable_declaration` is not a
/// chunk and not recursed, so its `require` would be missed.  To capture those
/// we additionally descend the non-chunk statement wrappers that can directly
/// contain a top-level `require`: `variable_declaration` and its inner
/// `assignment_statement` / `expression_list`.
fn lua_should_recurse(node: Node) -> bool {
    if node.parent().is_none() {
        return true;
    }
    matches!(
        node.kind(),
        "variable_declaration" | "assignment_statement" | "expression_list"
    )
}

/// FQN scheme: dotted (`M.foo`, `M.bar` for `M:bar`), derived from the function
/// name node — Lua has no syntactic modules so no scope frames are pushed.
/// `local function` → `Private`; everything else → `Public`.  Function bodies
/// (`block`) are never descended (see `lua_should_recurse`): nested closures do
/// not leak and interior calls are reached via the walker's call-pass.
pub static LUA: LanguageConfig = LanguageConfig {
    name: "lua",
    extensions: &[".lua"],
    language_fn: language,

    // `function_declaration` covers global, namespaced (`M.foo`), method
    // (`M:bar`), and `local function` forms.  Anonymous `function_definition`
    // (assigned to a local/field) has no `name` field and is not a chunk.
    function_kinds: &["function_declaration"],
    class_kinds: &[],
    method_kinds: &[],
    interface_kinds: &[],
    struct_kinds: &[],
    enum_kinds: &[],
    enum_member_kinds: &[],
    type_alias_kinds: &[],
    variable_kinds: &[],
    constant_kinds: &[],
    macro_kinds: &[],
    namespace_kinds: &[],
    // Lua "modules" are runtime tables (`local M = {}`), not syntactic nodes.
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    // `function_call` is listed for imports too: `require(...)` is a call,
    // filtered into Import edges by lua_resolve_import_specifier.
    import_kinds: &["function_call"],
    call_kinds: &["function_call"],
    // The callee is exposed via the `name` field, handled by lua_resolve_call_name.
    member_expr_kinds: &["dot_index_expression", "method_index_expression"],
    type_ref_kinds: &[],

    comment_kinds: &["comment"],
    doc_comment_kinds: &[],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "",
    import_source_field: "",
    // function_call's callee field is `name` (not `function`); the call-name
    // hook reads it directly, but keep the field accurate for completeness.
    call_function_field: "name",
    member_property_field: "field",
    receiver_field: "",

    queries: &EMPTY_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(lua_resolve_name),
        resolve_fqn: Some(lua_resolve_fqn),
        get_visibility: Some(lua_visibility),
        should_recurse_into: Some(lua_should_recurse),
        resolve_import_specifier: Some(lua_resolve_import_specifier),
        resolve_call_name: Some(lua_resolve_call_name),
        resolve_module_path: Some(lua_resolve_module_path),
        nested_extraction: true,
        ..LanguageHooks::DEFAULT
    },
};
