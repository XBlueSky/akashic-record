use crate::resolution::chunk_index::{normalize_base_type, resolve_dyn_trait_name};
use crate::types::*;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_rust::LANGUAGE.into()
}

// ─── Axum framework-route queries ─────────────────────────────────────────────
//
// Matches `.route(path, method(handler))` call chains (Axum 0.6+).
//
// AST shape (tree-sitter-rust):
//
//   call_expression                          ← @route.reg
//     field_expression
//       <receiver-expression>
//       field_identifier "route"            ← @_m (predicate: must == "route")
//     arguments
//       string_literal                      ← @route.path
//       call_expression
//         identifier                        ← @route.method  (e.g. "get")
//         arguments
//           identifier                      ← @route.handler (e.g. "list_users")
//
// Real Axum handlers are usually module-qualified — `get(health::health)`,
// `get(graph::core::get_graph)` — which parse as `scoped_identifier` (NOT a
// plain `identifier`); some are member paths (`field_expression`). The query
// therefore accepts all three node kinds for the handler. The runner takes the
// LAST `::`/`.`-delimited segment of the captured text as the bare handler name
// (`health::health` -> `health`) so it resolves against a chunk's short `name`.
static RUST_FRAMEWORK_ROUTES: [QueryDef; 1] = [QueryDef {
    name: "axum_route",
    source: "(call_expression \
  function: (field_expression \
    field: (field_identifier) @_m \
    (#eq? @_m \"route\")) \
  arguments: (arguments \
    (string_literal) @route.path \
    (call_expression \
      function: (identifier) @route.method \
      arguments: (arguments \
        [(identifier) (scoped_identifier) (field_expression)] @route.handler)))) @route.reg",
    compiled: std::sync::OnceLock::new(),
}];

// ─── Client HTTP-call queries (C1) ────────────────────────────────────────────
//
// Captures literal-path Rust `reqwest`-style client calls. Two forms:
//   1. method-style `<expr>.get("…")` / .post / .put / .delete / .patch / .head
//   2. free-function `reqwest::get("…")`
// The HTTP verb is restricted by a `#match?` predicate (mirrors Python's route
// query). The FIRST argument (leading `.` anchor) must be a string literal; its
// URL-shape (`http(s)://…` or leading `/`) is the SOLE guard, applied in the
// runner (`http_url_to_path`) — tree-sitter has no type info, so there is
// deliberately NO receiver-type check. Interpolated / non-string args → no match.
static RUST_HTTP_CALLS: [QueryDef; 2] = [
    QueryDef {
        name: "rust_http_method_call",
        source: "(call_expression \
  function: (field_expression \
    field: (field_identifier) @call.method \
    (#match? @call.method \"^(get|post|put|delete|patch|head)$\")) \
  arguments: (arguments \
    . (string_literal) @call.url)) @call.reg",
        compiled: std::sync::OnceLock::new(),
    },
    QueryDef {
        name: "rust_reqwest_free_call",
        source: "(call_expression \
  function: (scoped_identifier \
    path: (identifier) @_crate \
    name: (identifier) @call.method \
    (#eq? @_crate \"reqwest\") \
    (#match? @call.method \"^(get|post|put|delete|patch|head)$\")) \
  arguments: (arguments \
    . (string_literal) @call.url)) @call.reg",
        compiled: std::sync::OnceLock::new(),
    },
];

static RUST_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &RUST_FRAMEWORK_ROUTES,
    http_calls: &RUST_HTTP_CALLS,
    extra_chunks: &[],
};

/// Resolve the display name of a node.
///
/// `impl_item` is named by its `type` field (the implementing type), not
/// `name`, so `impl Counter { fn new() {} }` → scope frame "Counter" → method
/// FQN "Counter.new".  All other kinds use their `name` field (e.g.
/// `function_item`, `struct_item`, `trait_item`, `macro_definition`, …).
fn rust_resolve_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "impl_item" => node
            .child_by_field_name("type")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from),
        _ => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from),
    }
}

/// Return true when a `function_item` is async.
///
/// In tree-sitter-rust 0.23 the `async` keyword lives inside a
/// `function_modifiers` named child, not as a direct unnamed child of
/// `function_item`.  We walk both levels so the hook is robust to either
/// grammar layout.
fn rust_is_async(node: Node) -> bool {
    // Check direct children first (future-proofing).
    if (0..node.child_count()).any(|i| node.child(i).is_some_and(|c| c.kind() == "async")) {
        return true;
    }
    // Check inside function_modifiers (tree-sitter-rust ≥ 0.23 layout).
    for i in 0..node.named_child_count() {
        if let Some(mods) = node.named_child(i)
            && mods.kind() == "function_modifiers"
            && (0..mods.child_count()).any(|j| mods.child(j).is_some_and(|c| c.kind() == "async"))
        {
            return true;
        }
    }
    false
}

/// Visibility from `visibility_modifier` named child.
///
/// Recognises `pub(crate)` → `Crate`, `pub` → `Public`; anything else is
/// `Private`.
fn rust_visibility(node: Node, src: &[u8]) -> Visibility {
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i)
            && c.kind() == "visibility_modifier"
        {
            let t = c.utf8_text(src).unwrap_or("");
            if t.starts_with("pub(crate)") {
                return Visibility::Crate;
            }
            if t.starts_with("pub") {
                return Visibility::Public;
            }
        }
    }
    Visibility::Private
}

/// Resolve the callee name for `call_expression` and `macro_invocation`.
///
/// - `macro_invocation`: reads the `macro` field (e.g. `println` from
///   `println!(...)`).
/// - `call_expression` with a `field_expression` function (method call):
///   returns the `field` child of the `field_expression`.
/// - `call_expression` with a `scoped_identifier` function (`Foo::bar()`):
///   returns the `name` child.
/// - Otherwise: returns the full text of the `function` child.
fn rust_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
    if node.kind() == "macro_invocation" {
        return node
            .child_by_field_name("macro")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from);
    }
    let func = node.child_by_field_name("function")?;
    match func.kind() {
        "field_expression" => func
            .child_by_field_name("field")
            .and_then(|f| f.utf8_text(src).ok())
            .map(String::from),
        "scoped_identifier" => func
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from),
        _ => func.utf8_text(src).ok().map(String::from),
    }
}

/// Trailing type-name segment of a call-receiver path node.
///
/// Handles the shapes `rust_resolve_call_receiver` can see as `func`'s
/// `path` child (verified against tree-sitter-rust 0.23's actual grammar
/// output — see the doc comment below):
///
/// - `scoped_identifier` / `scoped_type_identifier` (`a::b::Baz`, or the base
///   of a further-nested turbofish path): trailing segment is the `name`
///   field.
/// - `generic_type` (`Vec::<i32>`, produced when the path segment carries a
///   turbofish): descend into its `type` field, which is itself one of these
///   shapes (a plain `type_identifier`, or — for `a::b::Vec::<i32>` — a
///   nested `scoped_identifier`), and recurse.
/// - Anything else (`type_identifier`, `identifier`, `self`, `crate`, …):
///   the node's own text is the whole (single-segment) name.
fn trailing_type_name<'a>(node: Node<'a>, src: &'a [u8]) -> Option<&'a str> {
    match node.kind() {
        // `scoped_type_identifier` is unreached on call-receiver paths (call
        // receivers are expression-context `scoped_identifier`, never the
        // type-context `scoped_type_identifier`); kept for forward-compat.
        "scoped_identifier" | "scoped_type_identifier" => {
            node.child_by_field_name("name")?.utf8_text(src).ok()
        }
        "generic_type" => trailing_type_name(node.child_by_field_name("type")?, src),
        _ => node.utf8_text(src).ok(),
    }
}

/// Receiver TYPE of a path-qualified call: `Foo::bar()` → "Foo",
/// `a::b::Baz::of()` → "Baz", `Vec::<i32>::new()` → "Vec". Reads the
/// `function` child; only a `scoped_identifier` yields a type (its `path`
/// child's trailing segment, resolved through any turbofish `generic_type`
/// wrapper via [`trailing_type_name`]). Method calls (`field_expression`)
/// and bare calls yield `None`.
fn rust_resolve_call_receiver(node: Node, src: &[u8]) -> Option<String> {
    let func = node.child_by_field_name("function")?;
    if func.kind() != "scoped_identifier" {
        return None;
    }
    let path = func.child_by_field_name("path")?;
    let ty = trailing_type_name(path, src)?;
    // Lowercase-leading qualifiers are modules/vars, not types (e.g. `self::f`,
    // `crate::x`); a type is Rust-idiomatic UpperCamelCase. Skip non-types.
    if ty.chars().next().is_some_and(|c| c.is_uppercase()) {
        Some(ty.to_string())
    } else {
        None
    }
}

/// Leading receiver token of a VALUE-method call (`x.m()` → "x", `self.m()` →
/// "self"). Returns the receiver only when the call's `function` is a
/// `field_expression` whose own `value` (the receiver) is a plain `identifier`
/// or `self` — i.e. a name the caller can look up in the intra-chunk type_env.
/// Non-trivial receivers (method chains `a.b().c()`, field-of-field `a.b.c()`,
/// index/call expressions) and path-qualified/bare calls yield `None`, so the
/// edge falls through to the existing cascade.
fn rust_resolve_call_receiver_expr(node: Node, src: &[u8]) -> Option<String> {
    let func = node.child_by_field_name("function")?;
    if func.kind() != "field_expression" {
        return None;
    }
    let recv = func.child_by_field_name("value")?;
    match recv.kind() {
        "identifier" | "self" => recv.utf8_text(src).ok().map(String::from),
        _ => None,
    }
}

/// Front half of D1c-1 method-chain resolution: for the OUTER call node of a
/// 2-hop chain `<inner_recv>.foo().bar()`, extract `(inner_recv's type,
/// "foo")` so the resolver can look up `foo`'s return type and then `bar` on
/// it.
///
/// Requires:
/// - `node`'s `function` is a `field_expression` (the outer `.bar` member
///   access — same shape [`rust_resolve_call_receiver_expr`] checks),
/// - the `field_expression`'s `value` (the RECEIVER of `.bar()`) is itself a
///   `call_expression` (i.e. `.bar()`'s receiver is the RESULT of a call, not
///   a plain token — exactly the case [`rust_resolve_call_receiver_expr`]
///   returns `None` for),
/// - that inner `call_expression`'s own `function` is ALSO a
///   `field_expression` `<inner_recv>.foo` (a value-method call, not
///   path-qualified/bare),
/// - `inner_recv.kind() ∈ {identifier, self}` (a simple typed token — same
///   restriction as [`rust_resolve_call_receiver_expr`]),
/// - `type_env.get(inner_recv_token)` hits (the token's type is intra-chunk
///   known — `self` is pre-seeded into `type_env` by the walker before
///   `call_pass` runs, same as D1b).
///
/// Anything else — the inner receiver is itself another call (3rd+ hop of a
/// longer chain, deferred), a field access (`a.b().c()`), an index
/// expression, or an unknown token — yields `None` (fail-closed; the edge
/// falls through to the existing cascade unchanged, matching every other
/// D1/D1b/D1c miss path).
fn rust_resolve_call_chain_receiver(
    node: Node,
    src: &[u8],
    type_env: &HashMap<String, String>,
) -> Option<(String, String)> {
    let func = node.child_by_field_name("function")?;
    if func.kind() != "field_expression" {
        return None;
    }
    let recv = func.child_by_field_name("value")?;
    if recv.kind() != "call_expression" {
        return None;
    }
    let inner_func = recv.child_by_field_name("function")?;
    if inner_func.kind() != "field_expression" {
        return None;
    }
    let inner_method = inner_func
        .child_by_field_name("field")?
        .utf8_text(src)
        .ok()?;
    let inner_recv = inner_func.child_by_field_name("value")?;
    let inner_token = match inner_recv.kind() {
        "identifier" | "self" => inner_recv.utf8_text(src).ok()?,
        _ => return None,
    };
    let owner_type = type_env.get(inner_token)?;
    Some((owner_type.clone(), inner_method.to_string()))
}

/// Front half of D1c-2 field-access resolution: for the OUTER call node of
/// `<inner_recv>.field.method()`, extract `(inner_recv's type, "field")` so
/// the resolver can look up `field`'s declared type on that struct and then
/// `method` on it.
///
/// Requires:
/// - `node`'s `function` is a `field_expression` (the outer `.method` member
///   access — same shape [`rust_resolve_call_receiver_expr`] and
///   [`rust_resolve_call_chain_receiver`] both check),
/// - the `field_expression`'s `value` (the RECEIVER of `.method()`) is
///   itself a `field_expression` (i.e. `.method()`'s receiver is a FIELD
///   ACCESS `<inner_recv>.field` — the exact shape
///   [`rust_resolve_call_receiver_expr`] returns `None` for, and distinct
///   from [`rust_resolve_call_chain_receiver`]'s `call_expression` shape),
/// - that inner `field_expression`'s `field` child is a NAMED field
///   (`field_identifier`), NOT a tuple-index (`integer_literal`, e.g.
///   `self.0`) — tuple/positional-struct fields are a documented D1c-2
///   non-goal (`parse_struct_fields` doesn't index them either),
/// - `inner_recv` (that field_expression's `value`) has
///   `kind() ∈ {identifier, self}` (a simple typed token — same restriction
///   as every other value-receiver hook; further field-of-field access
///   `a.b.c()` is multi-level, deferred),
/// - `type_env.get(inner_recv_token)` hits (the token's type is intra-chunk
///   known — `self` is pre-seeded into `type_env` by the walker, same as
///   D1b/D1c-1).
///
/// Anything else — the inner receiver is a call (D1c-1's job), another field
/// access (multi-level, deferred), an index expression, a tuple-index field,
/// or an unknown token — yields `None` (fail-closed; the edge falls through
/// to the existing cascade unchanged, matching every other D1/D1b/D1c miss
/// path).
fn rust_resolve_call_field_receiver(
    node: Node,
    src: &[u8],
    type_env: &HashMap<String, String>,
) -> Option<(String, String)> {
    let func = node.child_by_field_name("function")?;
    if func.kind() != "field_expression" {
        return None;
    }
    let recv = func.child_by_field_name("value")?;
    if recv.kind() != "field_expression" {
        return None;
    }
    let field_node = recv.child_by_field_name("field")?;
    if field_node.kind() != "field_identifier" {
        return None;
    }
    let field_name = field_node.utf8_text(src).ok()?;
    let inner_recv = recv.child_by_field_name("value")?;
    let inner_token = match inner_recv.kind() {
        "identifier" | "self" => inner_recv.utf8_text(src).ok()?,
        _ => return None,
    };
    let owner_type = type_env.get(inner_token)?;
    Some((owner_type.clone(), field_name.to_string()))
}

/// Scan a `function_item` node's generic parameters for trait bounds, building
/// `{generic_param_name -> first_bound_trait_name}` (D3). Covers both inline
/// bounds (`fn f<T: Trait>(…)`) and `where`-clause bounds (`fn f<T>(…) where T:
/// Trait`); an inline bound wins over a `where` bound for the same parameter,
/// and a multi-bound (`T: A + B`, either form) takes the FIRST bound. Computed
/// once per chunk alongside `type_env`; not cached or shared across chunks.
///
/// Grammar (tree-sitter-rust 0.23.3, verified via `.to_sexp()`):
/// - `type_parameters` (field on `function_item`) contains `type_parameter`
///   nodes with field `name` = `type_identifier` and, when bounded, field
///   `bounds` = `trait_bounds`;
/// - `where_clause` is a PLAIN child of `function_item` (no field label)
///   containing `where_predicate` nodes with field `left` = `type_identifier`
///   and field `bounds` = `trait_bounds`;
/// - the first bound in a `trait_bounds` is resolved via [`rust_trait_name_from`]
///   (handles `type_identifier`/`generic_type`/`scoped_type_identifier` and
///   returns `None` for lifetime/`+` tokens, so a leading lifetime bound is
///   skipped and the first real trait is taken).
fn parse_generic_trait_bounds(node: Node, src: &[u8]) -> HashMap<String, String> {
    let mut bounds: HashMap<String, String> = HashMap::new();

    // `where`-clause predicates first, so inline `type_parameters` bounds
    // (inserted second) win on collision for the same param name.
    let mut wc = node.walk();
    for child in node.children(&mut wc) {
        if child.kind() != "where_clause" {
            continue;
        }
        let mut pc = child.walk();
        for pred in child.named_children(&mut pc) {
            if pred.kind() != "where_predicate" {
                continue;
            }
            let (Some(left), Some(tb)) = (
                pred.child_by_field_name("left"),
                pred.child_by_field_name("bounds"),
            ) else {
                continue;
            };
            if let (Ok(name), Some(trait_name)) = (left.utf8_text(src), first_trait_bound(tb, src))
            {
                bounds.insert(name.to_string(), trait_name);
            }
        }
    }

    // Inline `<T: Trait>` bounds — overwrite where-clause entries (inline wins).
    if let Some(tps) = node.child_by_field_name("type_parameters") {
        let mut tc = tps.walk();
        for tp in tps.named_children(&mut tc) {
            if tp.kind() != "type_parameter" {
                continue; // skips lifetime_parameter / const_parameter
            }
            let (Some(name_node), Some(tb)) = (
                tp.child_by_field_name("name"),
                tp.child_by_field_name("bounds"),
            ) else {
                continue; // an unbounded `<T>` has no `bounds` field
            };
            if let (Ok(name), Some(trait_name)) =
                (name_node.utf8_text(src), first_trait_bound(tb, src))
            {
                bounds.insert(name.to_string(), trait_name);
            }
        }
    }
    bounds
}

/// First trait name in a `trait_bounds` node, via [`rust_trait_name_from`] —
/// which skips lifetime/`+` tokens, so `T: 'a + Trait` yields `Trait`.
fn first_trait_bound(trait_bounds: Node, src: &[u8]) -> Option<String> {
    let mut c = trait_bounds.walk();
    for b in trait_bounds.children(&mut c) {
        if let Some(t) = rust_trait_name_from(b, src) {
            return Some(t);
        }
    }
    None
}

/// Build the value-binding half of a Rust callable chunk's intra-chunk type
/// environment (binding name → bare base type). Covers, with shadowing
/// last-write-wins:
///
/// - typed parameters `p: T` (incl. `&self`/`self` — the SELF param is skipped
///   here; the walker injects `self`'s type from the scope stack),
/// - `let x: T [= …]` explicit annotations,
/// - `let x = T::new(…)` / `let x = T::default()` (call whose function is a
///   `scoped_identifier` with an UpperCamelCase leading segment → that segment),
/// - `let x = T { … }` struct-literal RHS.
///
/// Uninferable RHS (`let z = foo();`, method chains, etc.) is return-type flow —
/// deferred (D1c) — and contributes nothing. All stored types are run through
/// [`normalize_base_type`] so they key identically to the method index.
///
/// D3: a parameter whose declared type is a trait object (`dyn Trait`/
/// `Box<dyn Trait>`/`Arc<dyn Trait>`/`Rc<dyn Trait>`, via [`resolve_dyn_trait_name`])
/// or a generic parameter bound to a trait (via [`parse_generic_trait_bounds`])
/// is stored under the TRAIT's name instead of falling through to
/// `normalize_base_type`, and the binding name is additionally recorded in the
/// returned `HashSet` — so the walker can flag the resulting call edge
/// `recv_is_trait_bound` (the resolver's `trait_default` branch). `let`-bound
/// generic-typed locals are NOT covered (only typed params feed this — see the
/// design spec's Non-Goals).
fn rust_build_type_env(node: Node, src: &[u8]) -> (HashMap<String, String>, HashSet<String>) {
    let mut env: HashMap<String, String> = HashMap::new();
    // D3: binding names whose declared type is a trait object / trait-bound
    // generic param — stored in `env` under the TRAIT name and recorded here so
    // the walker can flag the resulting edge `recv_is_trait_bound`.
    let mut trait_bound_tokens: HashSet<String> = HashSet::new();
    // D3: this chunk's `{generic_param -> trait}` bound map, consulted for a
    // param whose (normalized) type is a bare generic identifier.
    let generic_bounds = parse_generic_trait_bounds(node, src);

    // 1. Typed parameters: node's `parameters` child holds `parameter` nodes,
    //    each with a `pattern` (the binding) and `type`. `self`/`&self` parse as
    //    a `self_parameter` (no `type` field) and are skipped.
    if let Some(params) = node.child_by_field_name("parameters") {
        let mut pc = params.walk();
        for p in params.named_children(&mut pc) {
            if p.kind() != "parameter" {
                continue; // skips `self_parameter`
            }
            let (Some(pat), Some(ty)) = (
                p.child_by_field_name("pattern"),
                p.child_by_field_name("type"),
            ) else {
                continue;
            };
            let (Ok(name), Ok(tyt)) = (pat.utf8_text(src), ty.utf8_text(src)) else {
                continue;
            };
            // D3: (a) a trait object `dyn …`/`Box<dyn …>`/… → the trait name;
            //     (b) a generic param bound to a trait (keyed on its normalized
            //     base, so `T` and `&T` both hit) → the bound's trait name;
            //     (c) fall back to today's concrete normalization.
            if let Some(trait_name) = resolve_dyn_trait_name(tyt)
                .or_else(|| generic_bounds.get(&normalize_base_type(tyt)).cloned())
            {
                env.insert(name.to_string(), trait_name);
                trait_bound_tokens.insert(name.to_string());
            } else {
                env.insert(name.to_string(), normalize_base_type(tyt));
            }
        }
    }

    // 2. `let` declarations anywhere in the body (depth-first). See
    //    collect_rust_lets — unchanged; `let`s never enter trait_bound_tokens.
    collect_rust_lets(node, src, &mut env);
    (env, trait_bound_tokens)
}

/// Depth-first scan for `let_declaration` nodes, recording each binding's
/// inferred base type into `env` (last-write-wins). See [`rust_build_type_env`].
fn collect_rust_lets(node: Node, src: &[u8], env: &mut HashMap<String, String>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "let_declaration"
            && let Some((name, ty)) = rust_let_binding_type(child, src)
        {
            env.insert(name, ty);
        }
        collect_rust_lets(child, src, env);
    }
}

/// Infer `(binding, base_type)` for a single `let_declaration`, or `None` when
/// the binding is non-trivial (tuple/struct pattern) or the type is uninferable.
///
/// Precedence: an explicit `type` annotation wins; otherwise inspect the `value`
/// RHS for a constructor call (`T::new()`) or struct literal (`T { … }`).
fn rust_let_binding_type(node: Node, src: &[u8]) -> Option<(String, String)> {
    let pat = node.child_by_field_name("pattern")?;
    // Only simple identifier bindings (`let x = …`); skip tuple/struct patterns.
    if pat.kind() != "identifier" {
        return None;
    }
    let name = pat.utf8_text(src).ok()?.to_string();

    // Explicit annotation `let x: T`.
    if let Some(ty) = node.child_by_field_name("type")
        && let Ok(tyt) = ty.utf8_text(src)
    {
        return Some((name, normalize_base_type(tyt)));
    }

    // Otherwise inspect the RHS value.
    let value = node.child_by_field_name("value")?;
    let ty = rust_type_of_value_expr(value, src)?;
    Some((name, ty))
}

/// Base type produced by a constructor call (`T::new(…)` — a `call_expression`
/// whose `function` is a `scoped_identifier` naming a type) or a struct literal
/// (`T { … }` — a `struct_expression`). Returns `None` for anything else
/// (method calls, free-fn calls, literals — return-type flow is deferred).
fn rust_type_of_value_expr(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        // `T::new(...)` / `T::default()` — reuse the path-qualified receiver
        // hook (it already yields the UpperCamelCase leading type segment, or
        // None for lowercase module qualifiers).
        "call_expression" => rust_resolve_call_receiver(node, src).map(|t| normalize_base_type(&t)),
        // `T { field: … }` — the type is the `name` field of the struct expr.
        "struct_expression" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(normalize_base_type),
        _ => None,
    }
}

/// Extract the import path from a `use_declaration` node.
///
/// Reads the full node text (e.g. `use std::collections::HashMap;`), strips
/// `use ` and `;`, then trims brace-group suffixes (`::{ … }`) and glob
/// suffixes (`::*`) to produce the module prefix.
///
/// Finding (re-export visibility leak): a `pub use a::B;` re-export carries a
/// leading `visibility_modifier` (`pub`, `pub(crate)`, `pub(in path)`) on the
/// `use_declaration` node, so the node text begins with the visibility prefix,
/// not `use `. The old `trim_start_matches("use ")` therefore stripped nothing
/// and leaked `"pub use a::B"` into every re-export edge's `module_specifier`.
/// We now strip a leading `visibility_modifier` child's text (whatever its
/// `pub`/`pub(...)` form) before stripping the `use ` keyword, yielding the
/// clean path. Stripping the child text by its byte length (not a `find("use")`
/// scan) avoids matching a `use` substring inside a path segment.
fn rust_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    let mut t = node.utf8_text(src).ok()?;
    // A re-export (`pub use …`) prefixes the node with a `visibility_modifier`
    // child; drop its exact text (plus following whitespace) so only `use …`
    // remains for the keyword strip below.
    if let Some(vis) = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == "visibility_modifier")
        && let Ok(vis_text) = vis.utf8_text(src)
        && let Some(rest) = t.strip_prefix(vis_text)
    {
        t = rest.trim_start();
    }
    let trimmed = t.trim_start_matches("use ").trim_end_matches(';').trim();
    if let Some(brace) = trimmed.find("::{") {
        return Some(trimmed[..brace].to_string());
    }
    if let Some(star) = trimmed.rfind("::*") {
        return Some(trimmed[..star].to_string());
    }
    Some(trimmed.to_string())
}

/// Rust module paths are crate-root-absolute or std/self/super-relative.
/// Pass them through verbatim; `pipeline::resolve_import_target` canonicalises
/// the `crate::a::b::Item` logical form into a repo module path (EXT-3c) for the
/// IMPORTS_FROM module-edge resolution.
fn rust_resolve_module_path(specifier: &str, _current: &str) -> Option<String> {
    Some(specifier.to_string())
}

/// Recursion guard: descend into impl/trait/mod bodies.
///
/// The walker's default guard only enters scope-container node kinds and the
/// root.  For Rust we also need to enter:
///
/// * `impl_item` / `trait_item` / `mod_item` — scope containers whose bodies
///   hold the inner `function_item`s.
/// * `declaration_list` — the actual body node of the above containers in the
///   tree-sitter-rust grammar.  Without this, the walker sees the
///   `impl_item`/`trait_item`/`mod_item` scope but doesn't descend into the
///   `declaration_list` body, and all inner `function_item`s are missed.
fn rust_should_recurse(node: Node) -> bool {
    node.parent().is_none()
        || matches!(
            node.kind(),
            "impl_item" | "trait_item" | "mod_item" | "declaration_list"
        )
}

/// Per-symbol import binding resolver for Rust.
///
/// Walks the use-tree under `use_declaration`'s `argument` field and emits the
/// LOCAL binding name (the leaf, or the alias when renamed) for every terminal
/// path it brings into scope, each as [`ImportSpec::Named`]. A Rust `use` leaf
/// CAN be a call target (`use a::b::func;` then `func()`), so unlike Python's
/// module-binding `import x`, the leaf is emitted.
///
/// Use-tree node forms handled (recursing the tree):
/// - `use a::b::c;` (`scoped_identifier`) → leaf `c` (the `name` field).
/// - `use a::b;` (`scoped_identifier`) → leaf `b`.
/// - `use foo;` (bare `identifier`) → `foo`.
/// - `use a::b::{c, d};` (`scoped_use_list`) → each `use_list` entry: `c`, `d`.
/// - `use a::b::c as e;` (`use_as_clause`) → the `alias` field `e`.
/// - `use a::b::*;` (`use_wildcard`) → nothing (Glob; module-level edge).
/// - nested `use a::{b::{c, d}, e};` → flatten to leaves `c`, `d`, `e` by
///   recursing through `use_list` / `scoped_use_list`.
///
/// Local/source rule: a renamed import (`c as e`) emits the LOCAL name `e` with
/// the SOURCE name `c` (the leaf of the aliased `path`); the walker records the
/// source so tier-1 resolution bridges `e()` to `c`'s definition chunk. A
/// non-aliased leaf emits source == local. Recursion is depth-capped
/// (`MAX_USE_DEPTH`) to bound pathological nesting; real code never approaches
/// it.
fn rust_resolve_import_names(node: Node, src: &[u8]) -> Vec<(String, String, ImportSpec)> {
    const MAX_USE_DEPTH: usize = 32;
    let mut out = Vec::new();
    if let Some(arg) = node.child_by_field_name("argument") {
        collect_use_leaves(arg, src, 0, MAX_USE_DEPTH, &mut out);
    }
    out
}

/// Leaf binding name of a use-tree path node (`scoped_identifier` → its `name`
/// field; bare `identifier` → its text). Used to derive the SOURCE name of an
/// aliased `use … as …` clause.
fn use_path_leaf_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "scoped_identifier" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from),
        "identifier" => node.utf8_text(src).ok().map(String::from),
        _ => None,
    }
}

/// Recurse a use-tree node, pushing `(local, source, Named)` for each terminal
/// binding. See [`rust_resolve_import_names`] for the form table.
fn collect_use_leaves(
    node: Node,
    src: &[u8],
    depth: usize,
    max_depth: usize,
    out: &mut Vec<(String, String, ImportSpec)>,
) {
    if depth > max_depth {
        return;
    }
    match node.kind() {
        // `a::b::c` — the leaf binding is the `name` field (`c`); source == local.
        "scoped_identifier" => {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())
            {
                out.push((name.to_string(), name.to_string(), ImportSpec::Named));
            }
        }
        // Bare `use foo;` or a single leaf inside a `use_list`; source == local.
        "identifier" => {
            if let Ok(name) = node.utf8_text(src) {
                out.push((name.to_string(), name.to_string(), ImportSpec::Named));
            }
        }
        // `… as alias` — the local binding is the `alias` field; the source is
        // the leaf of the aliased `path` (falls back to local if unresolved).
        "use_as_clause" => {
            if let Some(alias) = node
                .child_by_field_name("alias")
                .and_then(|n| n.utf8_text(src).ok())
            {
                let source = node
                    .child_by_field_name("path")
                    .and_then(|p| use_path_leaf_name(p, src))
                    .unwrap_or_else(|| alias.to_string());
                out.push((alias.to_string(), source, ImportSpec::Named));
            }
        }
        // `a::b::{ … }` — recurse into the `list` field (a `use_list`).
        "scoped_use_list" => {
            if let Some(list) = node.child_by_field_name("list") {
                collect_use_leaves(list, src, depth + 1, max_depth, out);
            }
        }
        // `{ a, b::{c}, … }` — recurse each named entry.
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_use_leaves(child, src, depth + 1, max_depth, out);
            }
        }
        // `a::b::*` (`use_wildcard`) and anything else contribute no leaf →
        // module-level Glob edge.
        _ => {}
    }
}

/// Resolve the bare trait name from a node that names a trait in a supertype
/// position (the trait of an `impl … for …`, or a supertrait bound). In
/// tree-sitter-rust 0.23 a trait reference is one of three node kinds — a bare
/// `type_identifier` (`Display`), a `generic_type` whose `type` field is the
/// base (`From<X>` → `From`), or a `scoped_type_identifier` whose `name` field
/// is the trailing segment (`std::fmt::Display` → `Display`). `generic_type`'s
/// base may itself be scoped (`a::b::Trait<X>`), so we recurse one level.
/// Returns `None` for anything else (lifetimes, `type_parameters`, etc.), which
/// is how generic params like `impl<T>` are excluded from IMPLEMENTS edges.
fn rust_trait_name_from(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" => node.utf8_text(src).ok().map(String::from),
        // `From<X>` / `a::b::Trait<X>` — the trait is the `type` field.
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|base| rust_trait_name_from(base, src)),
        // `std::fmt::Display` — the bare trait name is the trailing `name` field.
        "scoped_type_identifier" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from),
        _ => None,
    }
}

/// EXT-6c: a Rust type's supertypes. `impl Trait for Type` → the implemented
/// trait is the trait-naming child before the `for` keyword (Implements);
/// a `trait_item`'s supertraits live in a `trait_bounds` child (Implements).
/// struct/enum have none.
///
/// Finding (rust_resolve_supertypes generic/qualified trait impls): matching
/// only a bare `type_identifier` silently dropped the IMPLEMENTS edge for
/// generic (`impl From<X> for Y`) and path-qualified (`impl std::fmt::Display
/// for Y`) impls — and likewise for generic/qualified supertraits. We now
/// resolve the trait name via [`rust_trait_name_from`], which handles
/// `type_identifier`, `generic_type` (its `type` field) and
/// `scoped_type_identifier` (its `name` field) while ignoring generic params
/// (`impl<T>`, whose `type_parameters` node yields no name). Closed.
fn rust_resolve_supertypes(node: Node, src: &[u8]) -> Vec<(String, EdgeKind)> {
    let mut out = Vec::new();
    match node.kind() {
        "impl_item" => {
            let mut cursor = node.walk();
            let children: Vec<Node> = node.children(&mut cursor).collect();
            if let Some(for_idx) = children.iter().position(|c| c.kind() == "for") {
                for c in &children[..for_idx] {
                    if let Some(t) = rust_trait_name_from(*c, src) {
                        out.push((t, EdgeKind::Implements));
                    }
                }
            }
        }
        "trait_item" => {
            let mut cursor = node.walk();
            for c in node.children(&mut cursor) {
                if c.kind() == "trait_bounds" {
                    let mut bc = c.walk();
                    for b in c.children(&mut bc) {
                        if let Some(t) = rust_trait_name_from(b, src) {
                            out.push((t, EdgeKind::Implements));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

pub static RUST: LanguageConfig = LanguageConfig {
    name: "rust",
    extensions: &[".rs"],
    language_fn: language,

    function_kinds: &["function_item"],
    class_kinds: &[],
    method_kinds: &[],
    interface_kinds: &[],
    struct_kinds: &["struct_item"],
    enum_kinds: &["enum_item"],
    enum_member_kinds: &["enum_variant"],
    type_alias_kinds: &["type_item"],
    variable_kinds: &[],
    constant_kinds: &["const_item", "static_item"],
    macro_kinds: &["macro_definition"],
    namespace_kinds: &[],
    module_kinds: &["mod_item"],
    trait_kinds: &["trait_item"],
    impl_kinds: &["impl_item"],

    import_kinds: &["use_declaration"],
    call_kinds: &["call_expression", "macro_invocation"],
    member_expr_kinds: &["field_expression"],
    type_ref_kinds: &["type_identifier"],

    comment_kinds: &["line_comment", "block_comment"],
    // Doc extraction enabled for Rust: `///`/`//!` (line_comment) and
    // `/**`/`/*!` (block_comment) are filtered from ordinary comments by the
    // walker's `is_doc_comment` marker predicate, and consecutive doc lines are
    // coalesced. RawChunk.doc feeds the embedding text as its richest signal.
    doc_comment_kinds: &["line_comment", "block_comment"],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "return_type",
    import_source_field: "argument",
    call_function_field: "function",
    member_property_field: "field",
    receiver_field: "",

    queries: &RUST_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(rust_resolve_name),
        is_async: Some(rust_is_async),
        get_visibility: Some(rust_visibility),
        resolve_call_name: Some(rust_resolve_call_name),
        resolve_call_receiver: Some(rust_resolve_call_receiver),
        build_type_env: Some(rust_build_type_env),
        resolve_call_receiver_expr: Some(rust_resolve_call_receiver_expr),
        resolve_call_receiver_chain: Some(rust_resolve_call_chain_receiver),
        resolve_call_receiver_field: Some(rust_resolve_call_field_receiver),
        resolve_import_specifier: Some(rust_resolve_import_specifier),
        resolve_import_names: Some(rust_resolve_import_names),
        resolve_module_path: Some(rust_resolve_module_path),
        should_recurse_into: Some(rust_should_recurse),
        nested_extraction: true,
        resolve_supertypes: Some(rust_resolve_supertypes),
        ..LanguageHooks::DEFAULT
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver_of_call(src: &str) -> Option<String> {
        use tree_sitter::Parser;
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = p.parse(src, None).unwrap();
        fn find_call<'a>(n: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
            if n.kind() == "call_expression" {
                return Some(n);
            }
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                if let Some(f) = find_call(ch) {
                    return Some(f);
                }
            }
            None
        }
        let call = find_call(tree.root_node())?;
        rust_resolve_call_receiver(call, src.as_bytes())
    }

    #[test]
    fn call_receiver_type_from_scoped_call() {
        // `Foo::new()` → receiver type "Foo"; `a::b::Baz::of()` → "Baz";
        // bare `foo()` and method `x.m()` → None.
        assert_eq!(receiver_of_call("Foo::new()"), Some("Foo".to_string()));
        assert_eq!(receiver_of_call("a::b::Baz::of()"), Some("Baz".to_string()));
        assert_eq!(receiver_of_call("foo()"), None);
        assert_eq!(receiver_of_call("x.m()"), None);
    }

    #[test]
    fn call_receiver_type_from_turbofish_call() {
        // Turbofish-qualified path (`function`'s `path` child is a
        // `generic_type`, not a plain/scoped identifier): must resolve to the
        // base type name, not the raw `"Vec::<i32>"` slice.
        assert_eq!(
            receiver_of_call("Vec::<i32>::new()"),
            Some("Vec".to_string())
        );
    }

    #[test]
    fn call_receiver_type_lowercase_qualifier_is_none() {
        // Lowercase path qualifiers are modules/keywords, not types; exercises
        // the `is_uppercase()` gate directly (distinct from the early-return
        // `None`s in `call_receiver_type_from_scoped_call`).
        assert_eq!(receiver_of_call("self::helper()"), None);
        assert_eq!(receiver_of_call("crate::mk()"), None);
    }

    #[cfg(test)]
    fn type_env_of_fn(src: &str) -> std::collections::HashMap<String, String> {
        use tree_sitter::Parser;
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = p.parse(src, None).unwrap();
        fn find_fn<'a>(n: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
            if n.kind() == "function_item" {
                return Some(n);
            }
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                if let Some(f) = find_fn(ch) {
                    return Some(f);
                }
            }
            None
        }
        let f = find_fn(tree.root_node()).expect("a function_item");
        rust_build_type_env(f, src.as_bytes()).0
    }

    #[test]
    fn type_env_let_annotation() {
        let env = type_env_of_fn("fn f() { let x: Bar = make(); }");
        assert_eq!(env.get("x").map(String::as_str), Some("Bar"));
    }

    #[test]
    fn type_env_let_ctor_and_struct_literal() {
        let env = type_env_of_fn("fn f() { let a = Bar::new(); let b = Baz { n: 0 }; }");
        assert_eq!(env.get("a").map(String::as_str), Some("Bar"));
        assert_eq!(env.get("b").map(String::as_str), Some("Baz"));
    }

    #[test]
    fn type_env_params() {
        let env = type_env_of_fn("fn f(p: Widget, q: &mut Vec<Foo>) {}");
        assert_eq!(env.get("p").map(String::as_str), Some("Widget"));
        // reference + generics normalized to the base type.
        assert_eq!(env.get("q").map(String::as_str), Some("Vec"));
    }

    #[test]
    fn type_env_param_lifetime_annotated_ref_normalizes() {
        // Cross-task fix (Task 1 review): normalize_base_type must strip a
        // leading lifetime token after the reference strip, or `&'a Widget`
        // normalizes to `"'a Widget"` instead of `"Widget"`. Proves the
        // end-to-end path through rust_build_type_env's param handling.
        let env = type_env_of_fn("fn f(p: &'a Widget) {}");
        assert_eq!(env.get("p").map(String::as_str), Some("Widget"));
    }

    #[test]
    fn type_env_shadowing_last_write_wins() {
        let env = type_env_of_fn("fn f() { let x: Bar = m(); let x: Baz = n(); }");
        assert_eq!(env.get("x").map(String::as_str), Some("Baz"));
    }

    #[test]
    fn type_env_path_qualified_type_normalized() {
        // A `let x: outer::Widget` annotation is stored under the bare base.
        let env = type_env_of_fn("fn f() { let x: outer::Widget = m(); }");
        assert_eq!(env.get("x").map(String::as_str), Some("Widget"));
    }

    #[test]
    fn type_env_no_self_key() {
        // rust_build_type_env never injects `self`; the walker adds it from the
        // scope stack (only the walker holds the enclosing impl frame).
        let env = type_env_of_fn("fn f(&self) { let x: Bar = m(); }");
        assert!(!env.contains_key("self"));
    }

    #[test]
    fn type_env_uninferable_rhs_absent() {
        // `let z = foo();` is a return-type flow (deferred), not a ctor/struct
        // literal, and has no annotation → not inferred.
        let env = type_env_of_fn("fn f() { let z = foo(); }");
        assert!(!env.contains_key("z"));
    }

    #[cfg(test)]
    fn receiver_expr_of_call(src: &str) -> Option<String> {
        use tree_sitter::Parser;
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = p.parse(src, None).unwrap();
        fn find_call<'a>(n: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
            if n.kind() == "call_expression" {
                return Some(n);
            }
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                if let Some(f) = find_call(ch) {
                    return Some(f);
                }
            }
            None
        }
        let call = find_call(tree.root_node())?;
        rust_resolve_call_receiver_expr(call, src.as_bytes())
    }

    #[test]
    fn receiver_expr_of_value_method_call() {
        // `x.m()` → leading token "x"; `self.m()` → "self".
        assert_eq!(receiver_expr_of_call("x.m()"), Some("x".to_string()));
        assert_eq!(receiver_expr_of_call("self.m()"), Some("self".to_string()));
    }

    #[test]
    fn receiver_expr_none_for_non_value_method() {
        // path-qualified, bare, and non-trivial receivers yield None.
        assert_eq!(receiver_expr_of_call("Foo::new()"), None);
        assert_eq!(receiver_expr_of_call("foo()"), None);
        // method chain: receiver is itself a call, not a plain identifier/self.
        assert_eq!(receiver_expr_of_call("a.b().c()"), None);
        // field-of-field / index receivers are non-trivial.
        assert_eq!(receiver_expr_of_call("a.b.c()"), None);
    }

    #[cfg(test)]
    fn chain_receiver_of_call(
        src: &str,
        type_env: &std::collections::HashMap<String, String>,
    ) -> Option<(String, String)> {
        use tree_sitter::Parser;
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = p.parse(src, None).unwrap();
        fn find_call<'a>(n: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
            if n.kind() == "call_expression" {
                return Some(n);
            }
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                if let Some(f) = find_call(ch) {
                    return Some(f);
                }
            }
            None
        }
        let call = find_call(tree.root_node())?;
        rust_resolve_call_chain_receiver(call, src.as_bytes(), type_env)
    }

    #[test]
    fn chain_receiver_resolves_typed_token() {
        let mut env = HashMap::new();
        env.insert("x".to_string(), "T".to_string());
        assert_eq!(
            chain_receiver_of_call("x.foo().bar()", &env),
            Some(("T".to_string(), "foo".to_string()))
        );
    }

    #[test]
    fn chain_receiver_resolves_self() {
        let mut env = HashMap::new();
        env.insert("self".to_string(), "Owner".to_string());
        assert_eq!(
            chain_receiver_of_call("self.foo().bar()", &env),
            Some(("Owner".to_string(), "foo".to_string()))
        );
    }

    #[test]
    fn chain_receiver_none_for_free_fn_inner() {
        // foo() has NO field_expression function (it's a bare identifier) —
        // the inner call isn't a value-method call at all.
        let env = HashMap::new();
        assert_eq!(chain_receiver_of_call("foo().bar()", &env), None);
    }

    #[test]
    fn chain_receiver_none_for_deeper_chain_inner_is_call() {
        // a.b().c().d(): .d()'s inner receiver is a.b().c() — itself a call,
        // not a plain token. 3rd+ hop is deferred.
        let env = HashMap::new();
        assert_eq!(chain_receiver_of_call("a.b().c().d()", &env), None);
    }

    #[test]
    fn chain_receiver_none_for_index_receiver() {
        let env = HashMap::new();
        assert_eq!(chain_receiver_of_call("arr[0].m()", &env), None);
    }

    #[test]
    fn chain_receiver_none_when_token_missing_from_type_env() {
        // x.foo().bar() but x's type is NOT in type_env -> None (fail-closed,
        // same policy as D1b's resolve_call_receiver_expr miss path).
        let env = HashMap::new();
        assert_eq!(chain_receiver_of_call("x.foo().bar()", &env), None);
    }

    #[cfg(test)]
    fn field_receiver_of_call(
        src: &str,
        type_env: &std::collections::HashMap<String, String>,
    ) -> Option<(String, String)> {
        use tree_sitter::Parser;
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = p.parse(src, None).unwrap();
        fn find_call<'a>(n: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
            if n.kind() == "call_expression" {
                return Some(n);
            }
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                if let Some(f) = find_call(ch) {
                    return Some(f);
                }
            }
            None
        }
        let call = find_call(tree.root_node())?;
        rust_resolve_call_field_receiver(call, src.as_bytes(), type_env)
    }

    #[test]
    fn field_receiver_resolves_self() {
        let mut env = HashMap::new();
        env.insert("self".to_string(), "Owner".to_string());
        assert_eq!(
            field_receiver_of_call("self.field.method()", &env),
            Some(("Owner".to_string(), "field".to_string()))
        );
    }

    #[test]
    fn field_receiver_resolves_typed_token() {
        let mut env = HashMap::new();
        env.insert("x".to_string(), "T".to_string());
        assert_eq!(
            field_receiver_of_call("x.field.method()", &env),
            Some(("T".to_string(), "field".to_string()))
        );
    }

    #[test]
    fn field_receiver_none_for_plain_value_method_call() {
        // x.method() (1-hop) — D1b's job, not D1c-2's.
        let mut env = HashMap::new();
        env.insert("x".to_string(), "T".to_string());
        assert_eq!(field_receiver_of_call("x.method()", &env), None);
    }

    #[test]
    fn field_receiver_none_for_method_chain() {
        // x.foo().bar() — D1c-1's job (inner receiver is a CALL, not a field).
        let mut env = HashMap::new();
        env.insert("x".to_string(), "T".to_string());
        assert_eq!(field_receiver_of_call("x.foo().bar()", &env), None);
    }

    #[test]
    fn field_receiver_none_for_multi_level_field_access() {
        // x.a.b.method(): .method()'s receiver x.a.b is itself a
        // field_expression wrapping ANOTHER field_expression (x.a) —
        // multi-level access, deferred.
        let mut env = HashMap::new();
        env.insert("x".to_string(), "T".to_string());
        assert_eq!(field_receiver_of_call("x.a.b.method()", &env), None);
    }

    #[test]
    fn field_receiver_none_for_index_receiver() {
        let env = HashMap::new();
        assert_eq!(field_receiver_of_call("arr[0].field.method()", &env), None);
    }

    #[test]
    fn field_receiver_none_when_token_missing_from_type_env() {
        let env = HashMap::new();
        assert_eq!(field_receiver_of_call("x.field.method()", &env), None);
    }

    #[test]
    fn field_receiver_none_for_tuple_index_field() {
        // self.0.clone(): the inner field_expression's `field` child is an
        // integer_literal (tuple-index access), not a field_identifier
        // (named field) — a documented D1c-2 non-goal (tuple/positional
        // fields aren't indexed by parse_struct_fields either). Without this
        // guard the hook would tag metadata that can never resolve, and
        // would change tests/fixtures/rust/11-generic-qualified-impls.rs's
        // golden `self.0.clone()` edge (see Step 5's golden audit).
        let mut env = HashMap::new();
        env.insert("self".to_string(), "Holder".to_string());
        assert_eq!(field_receiver_of_call("self.0.clone()", &env), None);
    }

    #[cfg(test)]
    fn trait_bounds_of_fn(src: &str) -> std::collections::HashSet<String> {
        use tree_sitter::Parser;
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = p.parse(src, None).unwrap();
        fn find_fn<'a>(n: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
            if n.kind() == "function_item" {
                return Some(n);
            }
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                if let Some(f) = find_fn(ch) {
                    return Some(f);
                }
            }
            None
        }
        let f = find_fn(tree.root_node()).expect("a function_item");
        rust_build_type_env(f, src.as_bytes()).1
    }

    #[cfg(test)]
    fn generic_bounds_of_fn(src: &str) -> HashMap<String, String> {
        use tree_sitter::Parser;
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = p.parse(src, None).unwrap();
        fn find_fn<'a>(n: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
            if n.kind() == "function_item" {
                return Some(n);
            }
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                if let Some(f) = find_fn(ch) {
                    return Some(f);
                }
            }
            None
        }
        let f = find_fn(tree.root_node()).expect("a function_item");
        parse_generic_trait_bounds(f, src.as_bytes())
    }

    #[test]
    fn generic_bounds_inline() {
        let b = generic_bounds_of_fn("fn f<T: Trait>(x: T) {}");
        assert_eq!(b.get("T").map(String::as_str), Some("Trait"));
    }

    #[test]
    fn generic_bounds_where_clause() {
        let b = generic_bounds_of_fn("fn f<T>(x: T) where T: Trait {}");
        assert_eq!(b.get("T").map(String::as_str), Some("Trait"));
    }

    #[test]
    fn generic_bounds_inline_wins_over_where() {
        // Both an inline bound and a where bound name T; inline must win.
        let b = generic_bounds_of_fn("fn f<T: Inline>(x: T) where T: Wherey {}");
        assert_eq!(b.get("T").map(String::as_str), Some("Inline"));
    }

    #[test]
    fn generic_bounds_multi_takes_first() {
        let b = generic_bounds_of_fn("fn f<T: A + B>(x: T) {}");
        assert_eq!(b.get("T").map(String::as_str), Some("A"));
    }

    #[test]
    fn generic_bounds_unbounded_is_empty() {
        let b = generic_bounds_of_fn("fn f<T>(x: T) {}");
        assert!(b.is_empty());
    }

    #[test]
    fn type_env_dyn_trait_param_maps_to_trait_and_flags_token() {
        let env = type_env_of_fn("fn f(g: dyn Greeter) {}");
        assert_eq!(env.get("g").map(String::as_str), Some("Greeter"));
        let toks = trait_bounds_of_fn("fn f(g: dyn Greeter) {}");
        assert!(toks.contains("g"));
    }

    #[test]
    fn type_env_boxed_and_arced_dyn_params_map_to_trait() {
        let env = type_env_of_fn("fn f(b: Box<dyn Greeter>, a: Arc<dyn Greeter>) {}");
        assert_eq!(env.get("b").map(String::as_str), Some("Greeter"));
        assert_eq!(env.get("a").map(String::as_str), Some("Greeter"));
        let toks = trait_bounds_of_fn("fn f(b: Box<dyn Greeter>, a: Arc<dyn Greeter>) {}");
        assert!(toks.contains("b"));
        assert!(toks.contains("a"));
    }

    #[test]
    fn type_env_generic_bound_param_maps_to_trait_and_flags_token() {
        let env = type_env_of_fn("fn f<T: Greeter>(x: T) {}");
        assert_eq!(env.get("x").map(String::as_str), Some("Greeter"));
        let toks = trait_bounds_of_fn("fn f<T: Greeter>(x: T) {}");
        assert!(toks.contains("x"));
    }

    #[test]
    fn type_env_concrete_param_not_in_trait_bound_tokens() {
        // Regression: a plain concrete-typed param populates env but NOT the
        // trait-bound token set (so it stays receiver_type, not trait_default).
        let env = type_env_of_fn("fn f(p: Widget) {}");
        assert_eq!(env.get("p").map(String::as_str), Some("Widget"));
        let toks = trait_bounds_of_fn("fn f(p: Widget) {}");
        assert!(!toks.contains("p"));
    }
}
