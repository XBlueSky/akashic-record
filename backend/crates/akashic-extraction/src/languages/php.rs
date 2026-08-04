use crate::types::*;
use crate::walker_helpers::default_name_from_field;
use tree_sitter::Node;

/// PHP grammar choice: `LANGUAGE_PHP` (the text-aware grammar that understands
/// `<?php ?>` tags and interspersed HTML/text), NOT `LANGUAGE_PHP_ONLY`.  Real
/// `.php` source files begin with `<?php`, so the tag-aware grammar is the
/// correct one — `LANGUAGE_PHP_ONLY` would fail to parse the opening tag.
fn language() -> tree_sitter::Language {
    tree_sitter_php::LANGUAGE_PHP.into()
}

// ─── Laravel framework-route queries ─────────────────────────────────────────
//
// Matches `Route::get('/path', [Controller::class, 'method'])` and
// `Route::post('/path', 'Controller@method')` call patterns (Laravel 8+).
//
// AST shape (tree-sitter-php, LANGUAGE_PHP):
//
//   scoped_call_expression                   ← @route.reg
//     [scope]  (name) "Route"
//     [name]   (name) "get"/"post"/…         ← @route.method
//     [arguments] (arguments)
//       (argument)
//         (string)
//           (string_content) "/path"         ← @route.path  (no quotes)
//       (argument)
//         (array_creation_expression)        ← array-form handler
//           (array_element_initializer)        [0] Ctrl::class  (skipped)
//           (array_element_initializer)        [1] 'method'
//             (string)
//               (string_content) "method"    ← @route.handler  (no quotes)
//
// A second QueryDef handles the string-handler form:
//   Route::post('/path', 'Controller@action')
//     — the second argument is a plain (string)
//       (string_content) "Controller@action" ← @route.handler  (no quotes)
//
// We capture string_content (the bare text node inside a PHP string literal,
// with no surrounding quote characters) for both @route.path and @route.handler.
// The runner's trim_matches logic is a no-op for bare content, which is correct.
static PHP_FRAMEWORK_ROUTES: [QueryDef; 2] = [
    // Array-form: Route::get('/x', [Ctrl::class, 'method'])
    QueryDef {
        name: "laravel_route_array",
        source: "(scoped_call_expression \
  name: (name) @route.method \
  (#match? @route.method \"^(get|post|put|delete|patch|any)$\") \
  arguments: (arguments \
    (argument (string (string_content) @route.path)) \
    (argument (array_creation_expression \
      (array_element_initializer) \
      (array_element_initializer \
        (string (string_content) @route.handler)))))) @route.reg",
        compiled: std::sync::OnceLock::new(),
    },
    // String-form: Route::get('/x', 'Ctrl@action')
    QueryDef {
        name: "laravel_route_string",
        source: "(scoped_call_expression \
  name: (name) @route.method \
  (#match? @route.method \"^(get|post|put|delete|patch|any)$\") \
  arguments: (arguments \
    (argument (string (string_content) @route.path)) \
    (argument (string (string_content) @route.handler)))) @route.reg",
        compiled: std::sync::OnceLock::new(),
    },
];

static PHP_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &PHP_FRAMEWORK_ROUTES,
    http_calls: &[],
    extra_chunks: &[],
};

/// Recursion guard for the PHP walker.
///
/// PHP container nodes the walker must descend to reach member declarations:
///
/// * Root `program` (`parent().is_none()`).
/// * `namespace_definition` — the namespace scope container.  Its braced form
///   (`namespace App { ... }`) wraps its members in a `compound_statement`
///   body; the brace-less form (`namespace App;`) has no body and its members
///   are program-level siblings (handled by the fqn climb below).
/// * `class_declaration` / `interface_declaration` / `trait_declaration` —
///   type containers whose `body` field is a `declaration_list`.
/// * `enum_declaration` — its `body` field is an `enum_declaration_list`.
/// * `declaration_list` — the braced body wrapping class/interface/trait
///   members.
/// * `enum_declaration_list` — the braced body wrapping enum cases + methods.
///
/// CRITICAL — no nested-method/function leak: method and free-function bodies
/// are `compound_statement` nodes.  PHP reuses `compound_statement` for BOTH a
/// braced namespace body AND a function/method body, exactly like Ruby reuses
/// `body_statement`.  We disambiguate by the parent: a `compound_statement` is
/// descended ONLY when its immediate parent is a `namespace_definition` (a
/// namespace body).  A `compound_statement` under a `function_definition` /
/// `method_declaration` is a callable body — we never enter it, so nested
/// defs do not surface as top-level chunks and a callable's interior calls are
/// reached via the walker's call-pass instead.
fn php_should_recurse(node: Node) -> bool {
    if node.parent().is_none() {
        return true;
    }
    match node.kind() {
        "namespace_definition"
        | "class_declaration"
        | "interface_declaration"
        | "trait_declaration"
        | "enum_declaration"
        | "declaration_list"
        | "enum_declaration_list" => true,
        // A braced-namespace body is a compound_statement; a function/method
        // body is also a compound_statement.  Only descend the former.
        "compound_statement" => node
            .parent()
            .is_some_and(|p| p.kind() == "namespace_definition"),
        _ => false,
    }
}

/// Resolve the display name of a PHP declaration, normalizing namespace-path
/// separators `\`→`.`.
///
/// This matters for the `namespace_definition` container, whose `name` field is
/// a `namespace_name` like `App\Services`.  The walker uses this same hook to
/// (a) set each chunk's `name` AND (b) name the scope frame it pushes; the
/// scope frame name is what the walker joins (with `.`) to build every child's
/// `parent_fqn`.  If we left the raw `\` in the frame name, a method's
/// walker-built `parent_fqn` (`App\Services.Calculator`) would NOT equal its
/// parent class chunk's `.`-joined `fqn` (`App.Services.Calculator`) — the exact
/// cross-field separator skew the fqn rule forbids.  Normalizing here keeps
/// `name`, `fqn`, and `parent_fqn` uniformly `.`-separated across all chunks.
/// For non-namespace declarations (class/method/function names contain no `\`)
/// this is a no-op.
fn php_resolve_name(node: Node, src: &[u8]) -> Option<String> {
    default_name_from_field(node, src, "name").map(|n| n.replace('\\', "."))
}

/// Extract the callee name from a PHP call expression.
///
/// Four call node kinds are handled (all share an `arguments` field):
///
/// * `function_call_expression` — bare `foo()` / `Ns\foo()`.  The `function`
///   field is a `name` / `qualified_name` (read its text; for a qualified name
///   we take the trailing segment).
/// * `member_call_expression` — `$obj->method()`.  The `name` field is the
///   method identifier.
/// * `nullsafe_member_call_expression` — `$obj?->method()`.  Same `name` field.
/// * `scoped_call_expression` — `Foo::bar()` / `self::x()` / `static::y()`.
///   The `name` field is the method identifier.
///
/// In every case we return the bare callee identifier (the trailing segment),
/// matching the convention used by the C# / Java / Ruby resolvers.
fn php_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "function_call_expression" => {
            let func = node.child_by_field_name("function")?;
            let text = func.utf8_text(src).ok()?;
            // For `Ns\foo()` keep only the trailing `foo`.
            Some(text.rsplit('\\').next().unwrap_or(text).to_string())
        }
        "member_call_expression" | "nullsafe_member_call_expression" | "scoped_call_expression" => {
            node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())
                .map(String::from)
        }
        _ => None,
    }
}

/// Extract visibility from a PHP `method_declaration`.
///
/// PHP method modifiers are named children of the declaration node; the access
/// modifier is a `visibility_modifier` child whose text is `public` /
/// `private` / `protected`.  When no `visibility_modifier` is present the
/// method is implicitly `public` (PHP's default).
///
/// Free `function_definition`s and type declarations have no visibility
/// modifier in PHP, so they also resolve to `Public`.
fn php_visibility(node: Node, src: &[u8]) -> Visibility {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i)
            && child.kind() == "visibility_modifier"
            && let Ok(text) = child.utf8_text(src)
        {
            return match text {
                "public" => Visibility::Public,
                "private" => Visibility::Private,
                "protected" => Visibility::Protected,
                _ => Visibility::Public,
            };
        }
    }
    // No explicit visibility modifier → public (PHP's implicit default).
    Visibility::Public
}

/// Extract the imported qualified name(s) from a `namespace_use_declaration`.
///
/// PHP `use` statements take several forms:
///   `use App\Models\User;`
///   `use App\Models\User as U;`              (aliased)
///   `use App\Models\{User, Post};`           (grouped)
///   `use function App\Helpers\format;`       (function/const import)
///
/// IMPORT-PATH SEPARATOR DECISION: we preserve the LITERAL source path with its
/// native `\` separators in the import edge *target string* (e.g.
/// `App\Models\User`).  This mirrors how C#/Java keep their dotted paths
/// verbatim — the edge target faithfully records what the source wrote.  This
/// is independent of `fqn`, which MUST use `.` (see `php_resolve_fqn`); the two
/// fields serve different purposes (edge target = literal import source; fqn =
/// internal `.`-joined identity key).
///
/// For a single (possibly aliased) clause we return that clause's qualified
/// name.  For a grouped `use A\B\{C, D};` we synthesize the full path of the
/// FIRST clause (`A\B\C`) as the specifier so a single representative Import
/// edge is emitted (the walker emits one edge per `namespace_use_declaration`
/// when `resolve_import_names` is not set).
fn php_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    // Locate the `namespace_use_group` body (grouped form) or the inline
    // `namespace_use_clause` children (plain / aliased form).
    let group = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == "namespace_use_group");

    if let Some(group) = group {
        // Grouped: `use App\Models\{User, Post};`.  The prefix path precedes
        // the group; collect leading `namespace_name` text as the prefix and
        // join with the first clause's name.
        let prefix = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "namespace_name")
            .and_then(|c| c.utf8_text(src).ok())
            .unwrap_or("");
        let first_clause = (0..group.named_child_count())
            .filter_map(|i| group.named_child(i))
            .find(|c| c.kind() == "namespace_use_clause")?;
        let leaf = clause_qualified_name(first_clause, src)?;
        if prefix.is_empty() {
            return Some(leaf);
        }
        return Some(format!("{prefix}\\{leaf}"));
    }

    // Plain / aliased: a single `namespace_use_clause` child.
    let clause = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == "namespace_use_clause")?;
    clause_qualified_name(clause, src)
}

/// Return the imported qualified name of a `namespace_use_clause`, ignoring its
/// `as <alias>` part (the alias is in the `alias` field, the imported path is
/// the `name` / `qualified_name` child).
fn clause_qualified_name(clause: Node, src: &[u8]) -> Option<String> {
    (0..clause.named_child_count())
        .filter_map(|i| clause.named_child(i))
        .find(|c| matches!(c.kind(), "qualified_name" | "name"))
        .and_then(|c| c.utf8_text(src).ok())
        .map(|s| s.trim_start_matches('\\').to_string())
}

/// PHP `use` targets are fully-qualified namespace paths; pass them through
/// verbatim (literal `\` separators preserved — see the separator decision in
/// `php_resolve_import_specifier`).
fn php_resolve_module_path(specifier: &str, _current_module: &str) -> Option<String> {
    Some(specifier.to_string())
}

/// Resolve the fully-qualified name for a PHP declaration.
///
/// FQN SCHEME (documented choice): FQNs are joined with `.` — e.g.
/// `App.Models.User.save`.  Although PHP's native namespace separator is `\`,
/// the `fqn` is an internal identity / match key (not user-facing prose), and
/// every other language config plus the walker's own `parent_fqn` computation
/// (`scope_path`) joins with `.`.  Using `.` here keeps PHP consistent with the
/// rest of the system and guarantees a chunk's `parent_fqn` exactly equals its
/// parent chunk's `fqn` (no cross-field separator skew, avoiding the EXT-8
/// migration trap).  The literal `\` path is preserved ONLY in import edge
/// target strings, never in fqn.
///
/// The walker pushes a scope frame for every `namespace_definition` (braced
/// form), `class_declaration`, `interface_declaration`, `trait_declaration`,
/// and `enum_declaration` it descends, joining their names with `.`.  We
/// consult that scope stack first.  As a defensive fallback we climb the AST to
/// find the nearest enclosing type container (covers members reached without a
/// scope frame), mirroring the C# / Java / Ruby resolvers.  Namespace-name
/// segments (which contain `\`) are normalized to `.`.
fn php_resolve_fqn(node: Node, src: &[u8], scope: &ScopeStack) -> Option<String> {
    // A declaration's own name may itself be a namespace path (e.g. the
    // `namespace App\Services` container name), so normalize `\`→`.` here too
    // to keep every fqn segment `.`-joined.
    let name = default_name_from_field(node, src, "name")?.replace('\\', ".");

    if !scope.is_empty() {
        let prefix = scope
            .iter()
            .map(|f| f.name.replace('\\', "."))
            .collect::<Vec<_>>()
            .join(".");
        return Some(format!("{prefix}.{name}"));
    }

    // Scope stack empty: climb the AST for the nearest enclosing type
    // container (e.g. a method inside an enum, or a class under a brace-less
    // namespace that was not pushed as a scope frame).
    let container_kinds = [
        "class_declaration",
        "interface_declaration",
        "trait_declaration",
        "enum_declaration",
    ];
    let mut parent = node.parent();
    while let Some(p) = parent {
        if container_kinds.contains(&p.kind())
            && let Some(cn) = default_name_from_field(p, src, "name")
        {
            return Some(format!("{}.{name}", cn.replace('\\', ".")));
        }
        parent = p.parent();
    }

    Some(name)
}

/// PHP `LanguageConfig`.
///
/// FQN scheme: `Namespace.Type.method` with `.` separators (matching every
/// other language config and the walker's `parent_fqn`).  `namespace_definition`
/// is a scope-pushing namespace container; classes/interfaces/traits/enums are
/// scope containers.  Free `function_definition`s chunk as top-level functions
/// (PHP, unlike Java/C#/Ruby, has real free functions).  Method/function bodies
/// are leaf chunks and are never descended (see `php_should_recurse`).
pub static PHP: LanguageConfig = LanguageConfig {
    name: "php",
    extensions: &[".php"],
    language_fn: language,

    // PHP has real free-standing functions.
    function_kinds: &["function_definition"],
    class_kinds: &["class_declaration"],
    method_kinds: &["method_declaration"],
    interface_kinds: &["interface_declaration"],
    struct_kinds: &[],
    enum_kinds: &["enum_declaration"],
    enum_member_kinds: &["enum_case"],
    type_alias_kinds: &[],
    variable_kinds: &[],
    constant_kinds: &[],
    macro_kinds: &[],
    // `namespace_definition` is PHP's namespace container; treat it as a
    // scope-pushing namespace so FQNs nest (e.g. `App.Models.User.save`).
    namespace_kinds: &["namespace_definition"],
    module_kinds: &[],
    // `trait_declaration` is a scope container; treat it as a trait so its
    // methods nest and a scope frame is pushed.
    trait_kinds: &["trait_declaration"],
    impl_kinds: &[],

    import_kinds: &["namespace_use_declaration"],
    call_kinds: &[
        "function_call_expression",
        "member_call_expression",
        "nullsafe_member_call_expression",
        "scoped_call_expression",
    ],
    member_expr_kinds: &[],
    type_ref_kinds: &[],

    comment_kinds: &["comment"],
    doc_comment_kinds: &[],

    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: "return_type",
    import_source_field: "",
    // Call callee names are resolved via php_resolve_call_name (the four call
    // node kinds use different fields), not the default_callee_name path.
    call_function_field: "function",
    member_property_field: "name",
    receiver_field: "object",

    queries: &PHP_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(php_resolve_name),
        resolve_fqn: Some(php_resolve_fqn),
        get_visibility: Some(php_visibility),
        should_recurse_into: Some(php_should_recurse),
        resolve_import_specifier: Some(php_resolve_import_specifier),
        resolve_call_name: Some(php_resolve_call_name),
        resolve_module_path: Some(php_resolve_module_path),
        ..LanguageHooks::DEFAULT
    },
};
