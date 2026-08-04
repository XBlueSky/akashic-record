use crate::types::*;
use crate::walker_helpers::default_name_from_field;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_ruby::LANGUAGE.into()
}

// ─── Rails framework-route queries ────────────────────────────────────────────
//
// Matches `get '/path', to: 'ctrl#action'` DSL calls inside a
// `Rails.application.routes.draw do … end` block.
//
// AST shape (tree-sitter-ruby):
//
//   call                                     ← @route.reg
//     method: identifier  "get"/"post"/…     ← @route.method
//     arguments: argument_list
//       string  "'/path'"                    ← @route.path  (includes quotes)
//       pair
//         key: hash_key_symbol  "to"
//         value: string
//           string_content  "ctrl#action"    ← @route.handler  (no quotes)
//
// Note: `get '/x', to: '...'` (without parentheses) parses as a `call` node in
// tree-sitter-ruby (confirmed empirically), not `command`. The `pair` captures
// the `to:` keyword argument; its `value` is the handler string.
//
// The `pair`'s `key` is a `hash_key_symbol` whose text is the bare symbol name
// (no trailing colon — the `:` is a separate token of the `pair` rule). We pin
// it to `to` via `#eq?` so that ONLY the `to:` pair's string value is treated
// as the handler. Without this constraint, any later keyword arg with a string
// value (e.g. `as: 'widgets_list'`, `constraints: '...'`) would also match
// `value: (string ...)` and could be mis-captured as the route handler. This
// mirrors the constrained-key idiom already used for the JS/TS Route queries
// (`(pair key: (property_identifier) @_k (#eq? @_k "...") value: ...)`).
static RUBY_FRAMEWORK_ROUTES: [QueryDef; 1] = [QueryDef {
    name: "rails_route",
    source: "(call \
  method: (identifier) @route.method \
  (#match? @route.method \"^(get|post|put|delete|patch)$\") \
  arguments: (argument_list \
    (string) @route.path \
    (pair \
      key: (hash_key_symbol) @_route.key \
      (#eq? @_route.key \"to\") \
      value: (string (string_content) @route.handler)))) @route.reg",
    compiled: std::sync::OnceLock::new(),
}];

static RUBY_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &RUBY_FRAMEWORK_ROUTES,
    http_calls: &[],
    extra_chunks: &[],
};

/// Recursion guard for the Ruby walker.
///
/// Ruby's grammar uses `body_statement` for BOTH the body of a scope container
/// (`class` / `module` / `singleton_class`) AND the body of a `method` /
/// `singleton_method`.  The node kind alone is therefore ambiguous, exactly
/// like Python's `block`.  We disambiguate by the parent:
///
/// * root `program` (no parent) → recurse.
/// * `class` / `module` / `singleton_class` → recurse (their `body_statement`
///   holds member definitions).
/// * `body_statement` → recurse ONLY when its immediate parent is a scope
///   container (`class` / `module` / `singleton_class`).  A `body_statement`
///   under a `method` / `singleton_method` is a method body — we do NOT enter
///   it, so nested defs do not leak as top-level chunks and a method's interior
///   calls are reached via the walker's call-pass instead.
/// * everything else → do NOT recurse.
///
/// This preserves the CRITICAL zero-nested-leak invariant: `method` /
/// `singleton_method` bodies are never descended by the main walk.
fn ruby_should_recurse(node: Node) -> bool {
    if node.parent().is_none() {
        return true;
    }
    match node.kind() {
        "class" | "module" | "singleton_class" => true,
        "body_statement" => node
            .parent()
            .is_some_and(|p| matches!(p.kind(), "class" | "module" | "singleton_class")),
        _ => false,
    }
}

/// Resolve the fully-qualified name for a Ruby definition.
///
/// FQN SCHEME (documented choice): FQNs are joined with `.` — e.g.
/// `Geometry.Circle.area`, `Geometry.Utils.square`, `Calculator.version`.
/// Although Ruby's idiomatic namespace separator is `::`, the `fqn` is an
/// internal identity / match key (not user-facing prose), and every other
/// language config plus the walker's own `parent_fqn` computation
/// (`scope_path`) joins with `.`.  Using `.` here keeps Ruby consistent with
/// the rest of the system and guarantees a chunk's `parent_fqn` exactly equals
/// its parent chunk's `fqn` (no cross-field separator skew).  We deliberately
/// do NOT distinguish instance methods (`#`) from class methods (`.`) in the
/// FQN: a uniform `.`-joined path keeps resolution simple across the
/// IMPORTS_FROM / CALLS graph.  `def self.foo` (a `singleton_method`)
/// therefore yields the same `Outer.foo` shape as an instance `def foo`.
///
/// The walker pushes a scope frame for every `class` / `module` (registered as
/// `class_kinds` / `module_kinds`); `singleton_class` is registered too.  We
/// consult that scope stack first.  As a defensive fallback we climb the AST to
/// find the nearest enclosing `class` / `module` whose `name` field is a
/// `constant`, mirroring the Java / C# resolvers.
fn ruby_resolve_fqn(node: Node, src: &[u8], scope: &ScopeStack) -> Option<String> {
    let name = default_name_from_field(node, src, "name")?;

    if !scope.is_empty() {
        let prefix = scope
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        return Some(format!("{prefix}.{name}"));
    }

    // Scope stack empty: climb the AST for the nearest enclosing container.
    let mut parent = node.parent();
    while let Some(p) = parent {
        if matches!(p.kind(), "class" | "module" | "singleton_class")
            && let Some(cn) = default_name_from_field(p, src, "name")
        {
            return Some(format!("{cn}.{name}"));
        }
        parent = p.parent();
    }

    Some(name)
}

/// Extract the callee name from a Ruby `call` node.
///
/// The `call` node exposes a `method` field (an `identifier`/`constant`/
/// `operator`) that is the bare method name, plus an optional `receiver` field
/// for `obj.method` forms.  For both `add(1, 2)` and `JSON.parse(path)` we want
/// the `method` field text (`add`, `parse`).  Paren-less calls (`include Foo`)
/// are also `call` nodes with the same `method` field, so they are handled
/// uniformly.
fn ruby_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("method")
        .and_then(|n| n.utf8_text(src).ok())
        .map(String::from)
}

/// Reclassify `require` / `require_relative` / `autoload` calls as imports.
///
/// IMPORT DESIGN DECISION (Option A): Ruby has no import *statement* —
/// `require "json"`, `require_relative "../lib/x"`, and `autoload :Sym, "path"`
/// are ordinary method calls with a string-literal argument.  To let Ruby
/// participate in the IMPORTS_FROM graph like every other language, we list
/// `call` in `import_kinds` and use this specifier resolver as a FILTER:
///
/// * If the call's `method` field is `require` / `require_relative` /
///   `autoload`, we return the (unquoted) first string-literal argument as the
///   import specifier → the walker emits an `Import` edge.
/// * For ANY other call (`add`, `JSON.parse`, `include`, `attr_accessor`, ...)
///   we return `None`, so `extract_import_edge` short-circuits and emits no
///   import edge.  Those calls are still captured as `Call` edges by the
///   call-pass, exactly as before.
///
/// This slots into the existing generic walker with NO walker changes: the
/// walker already calls `resolve_import_specifier` for every node whose kind is
/// in `import_kinds`, and treats a `None` return as "not an import".
///
/// `autoload`'s first argument is a symbol (the constant name), not the path,
/// so for `autoload` we take the LAST string-literal argument (the path).  For
/// require/require_relative the first (and only) string argument is the path.
fn ruby_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    let method = node
        .child_by_field_name("method")
        .and_then(|m| m.utf8_text(src).ok())?;
    if !matches!(method, "require" | "require_relative" | "autoload") {
        return None;
    }

    let args = node.child_by_field_name("arguments")?;
    // Collect string-literal arguments in order.
    let mut strings: Vec<Node> = Vec::new();
    for i in 0..args.named_child_count() {
        if let Some(c) = args.named_child(i)
            && c.kind() == "string"
        {
            strings.push(c);
        }
    }
    // autoload(:Sym, "path") → path is the last string; require("x") → first.
    let target = if method == "autoload" {
        strings.last()
    } else {
        strings.first()
    }?;

    // The string's content is in a `string_content` child; fall back to the
    // whole string text with surrounding quotes trimmed.
    let spec = target
        .named_child(0)
        .filter(|c| c.kind() == "string_content")
        .and_then(|c| c.utf8_text(src).ok())
        .map(String::from)
        .or_else(|| {
            target.utf8_text(src).ok().map(|s| {
                s.trim_matches(|c: char| matches!(c, '\'' | '"'))
                    .to_string()
            })
        })?;
    Some(spec)
}

/// Ruby require paths are passed through verbatim.
///
/// `require "json"` names a gem/stdlib feature (no path rewriting possible
/// without a load-path); `require_relative "../lib/helper"` is relative to the
/// requiring file, but the walker's `current_module` resolution is host-path
/// based and the relative target is already meaningful as written.  We keep the
/// specifier as-is so the edge target faithfully records what the source wrote;
/// downstream resolution can interpret `require_relative` paths if needed.
fn ruby_resolve_module_path(specifier: &str, _current_module: &str) -> Option<String> {
    Some(specifier.to_string())
}

/// Visibility for Ruby definitions.
///
/// LIMITATION: Ruby `private` / `protected` / `public` are method calls that
/// flip a mutable visibility state for all subsequently-defined methods in the
/// current scope (and `private :foo` / `private def foo` forms exist too).
/// Tracking this precisely requires stateful, order-dependent analysis of the
/// class body, which is out of scope for a declarative per-node config and
/// would be brittle.  We therefore default EVERY definition to `Public` (the
/// Ruby default at the top of a class body) and accept that methods placed
/// after a bare `private` call are mis-reported as public.  This matches the
/// pragmatic "do not over-engineer state tracking" guidance.
fn ruby_visibility(_node: Node, _src: &[u8]) -> Visibility {
    Visibility::Public
}

/// FQN scheme: `Outer.Inner.method` with `.` separators (matching every other
/// language config and the walker's `parent_fqn`).  `module` is treated as a
/// scope-pushing namespace container so nested classes/modules/methods get
/// fully-qualified paths.  `class` and `singleton_class` are likewise scope
/// containers.  Method/singleton_method bodies are leaf chunks and are never
/// descended (see `ruby_should_recurse`).
pub static RUBY: LanguageConfig = LanguageConfig {
    name: "ruby",
    extensions: &[".rb"],
    language_fn: language,

    // Ruby has no free-standing functions at the grammar level; top-level
    // `def`s parse as `method` nodes (which we classify as methods).
    function_kinds: &[],
    class_kinds: &["class"],
    // `method` = `def foo`; `singleton_method` = `def self.foo` / `def obj.foo`.
    method_kinds: &["method", "singleton_method"],
    interface_kinds: &[],
    struct_kinds: &[],
    enum_kinds: &[],
    enum_member_kinds: &[],
    type_alias_kinds: &[],
    variable_kinds: &[],
    constant_kinds: &[],
    macro_kinds: &[],
    namespace_kinds: &[],
    // `module` is Ruby's namespace container; treat it as a scope-pushing module
    // so FQNs nest (e.g. `Geometry.Circle.area`).  `singleton_class`
    // (`class << self`) is also a scope container.
    module_kinds: &["module", "singleton_class"],
    trait_kinds: &[],
    impl_kinds: &[],

    // `call` is listed for imports too: require/require_relative/autoload are
    // calls, filtered into Import edges by ruby_resolve_import_specifier.
    import_kinds: &["call"],
    call_kinds: &["call"],
    // `call` exposes the callee via the `method` field (handled by the hook).
    member_expr_kinds: &[],
    type_ref_kinds: &[],

    comment_kinds: &["comment"],
    doc_comment_kinds: &[],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "",
    import_source_field: "",
    // call_function_field unused: ruby_resolve_call_name reads the `method`
    // field directly (the default_callee_name path does not fit Ruby's `call`).
    call_function_field: "method",
    member_property_field: "",
    receiver_field: "receiver",

    queries: &RUBY_QUERIES,
    hooks: LanguageHooks {
        resolve_fqn: Some(ruby_resolve_fqn),
        get_visibility: Some(ruby_visibility),
        should_recurse_into: Some(ruby_should_recurse),
        resolve_import_specifier: Some(ruby_resolve_import_specifier),
        resolve_call_name: Some(ruby_resolve_call_name),
        resolve_module_path: Some(ruby_resolve_module_path),
        ..LanguageHooks::DEFAULT
    },
};
