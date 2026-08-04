use crate::types::*;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_bash::LANGUAGE.into()
}

static EMPTY_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &[],
    http_calls: &[],
    extra_chunks: &[],
};

/// Read the `command_name` field text of a `command` node.
///
/// In tree-sitter-bash a `command`'s callee lives in the `name` field, which is
/// a `command_name` node wrapping a single `word` (e.g. `echo`, `source`, `.`,
/// or a user-defined function name).  We return that text verbatim.  Used by
/// both [`bash_resolve_call_name`] and [`bash_resolve_import_specifier`].
fn command_name_text<'a>(node: Node<'a>, src: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
}

/// Resolve the FQN for a Bash `function_definition`.
///
/// FQN SCHEME (documented choice): Bash functions are FLAT — the language has
/// no namespace / module / class construct, so the FQN is simply the function's
/// own short name (`foo`, `bar`).  There are never any scope frames to consult
/// (no scope-pushing node-kinds are registered), and top-level functions get
/// `parent_fqn = None`.  This mirrors Lua, the other flat dynamic language.
///
/// The `name` field of a `function_definition` is a bare `word` for BOTH the
/// `foo() { }` POSIX form and the `function bar { }` ksh form, so reading the
/// `name` field uniformly yields the function name in either case.
fn bash_resolve_fqn(node: Node, src: &[u8], _scope: &ScopeStack) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .map(String::from)
}

/// Extract the callee name from a Bash `command` node.
///
/// Every invoked shell command — built-ins (`echo`, `cd`), external programs
/// (`ls`), and calls to user-defined functions (`foo`) — parses as a `command`
/// whose `command_name` is the callee.  We emit a `Call` edge for all of them;
/// the downstream resolver drops the ones that do not resolve to a function
/// defined in the graph (so `echo`/`cd`/`ls` simply have no target and are
/// dropped, while `foo` resolves to its `function_definition`).
fn bash_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
    command_name_text(node, src).map(String::from)
}

/// Reclassify `source <path>` / `. <path>` commands as imports.
///
/// IMPORT DESIGN DECISION (Option A, mirroring Ruby/Lua): Bash has no import
/// *statement* — sourcing another script is the ordinary command `source
/// ./lib.sh` or its POSIX synonym `. ./lib.sh`.  Both parse as `command` nodes.
/// To let Bash participate in the IMPORTS_FROM graph we list `command` in BOTH
/// `import_kinds` and `call_kinds` and use this specifier resolver as a FILTER:
///
///   - command name is `source` or `.` → return the first `argument` (the path)
///     → the walker emits an `Import` edge.
///   - ANY other command (`echo`, `ls`, `foo`, ...) → return `None`, so the
///     walker emits no import edge.  Those commands are still captured as `Call`
///     edges by the call-pass.
///
/// This needs NO walker changes: the walker already calls
/// `resolve_import_specifier` for every node whose kind is in `import_kinds` and
/// treats a `None` return as "not an import".  A `command` that IS a source/.
/// therefore yields one Import edge (from this hook) and one Call edge (from the
/// call-pass) — the downstream resolver only keeps the Call edge if it resolves
/// to a defined function, which `source`/`.` never do, so no double-count.
fn bash_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    let name = command_name_text(node, src)?;
    if name != "source" && name != "." {
        return None;
    }
    // The path is the first `argument`-field child of the command.
    let mut cursor = node.walk();
    for child in node.children_by_field_name("argument", &mut cursor) {
        if let Ok(text) = child.utf8_text(src) {
            return Some(
                text.trim_matches(|c: char| matches!(c, '\'' | '"'))
                    .to_string(),
            );
        }
    }
    None
}

/// Bash source paths are passed through verbatim.
///
/// `source ./lib.sh` names a file relative to the sourcing script (or via
/// `$PATH` for a bare name); there is no load-path rewriting to do without
/// runtime resolution, so we keep the specifier exactly as written.
fn bash_resolve_module_path(specifier: &str, _current_module: &str) -> Option<String> {
    Some(specifier.to_string())
}

/// Visibility for Bash function definitions.
///
/// DESIGN CHOICE: Bash has no access modifiers — every function is reachable by
/// any code that has sourced the defining script.  We therefore default every
/// definition to `Public`, mirroring Ruby.
fn bash_visibility(_node: Node, _src: &[u8]) -> Visibility {
    Visibility::Public
}

/// Recursion guard for the Bash walker.
///
/// CRITICAL zero-nested-leak invariant: a `function_definition`'s body is a
/// `compound_statement`.  We must NOT descend into it, otherwise a function
/// defined inside another function's body would leak as a top-level chunk and
/// the outer function's interior commands would be double-counted (the
/// call-pass already covers them).
///
/// Rules:
///   - root `program` (no parent) → recurse (reach top-level definitions and
///     top-level `source`/`.`/command nodes).
///   - everything else (notably `compound_statement`) → do NOT recurse.
///
/// Bash has no syntactic scope containers (no class/module/namespace nodes), so
/// the root is the only thing to descend.  Top-level `source`/`.` commands and
/// top-level calls live directly under `program` and are reached by the main
/// walk.
fn bash_should_recurse(node: Node) -> bool {
    node.parent().is_none()
}

/// FQN scheme: flat — the function's own name (Bash has no namespacing).
/// Visibility is always `Public` (no access modifiers).  Function bodies
/// (`compound_statement`) are never descended (see [`bash_should_recurse`]):
/// nested functions do not leak and interior commands are reached via the
/// walker's call-pass.  `source`/`.` commands are filtered into Import edges by
/// [`bash_resolve_import_specifier`]; every other `command` stays a Call edge.
pub static BASH: LanguageConfig = LanguageConfig {
    name: "bash",
    extensions: &[".sh", ".bash"],
    language_fn: language,

    // `function_definition` covers both `foo() { }` (POSIX) and `function bar
    // { }` (ksh) forms; its `name` field is a `word` in both cases.
    function_kinds: &["function_definition"],
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
    // Bash has no syntactic module / namespace construct.
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    // `command` is listed for imports too: `source`/`.` are commands, filtered
    // into Import edges by bash_resolve_import_specifier; every other command
    // stays a Call edge.
    import_kinds: &["command"],
    call_kinds: &["command"],
    // The callee is the `command_name` field, handled by bash_resolve_call_name.
    member_expr_kinds: &[],
    type_ref_kinds: &[],

    comment_kinds: &["comment"],
    doc_comment_kinds: &[],

    name_field: "name",
    body_field: "body",
    params_field: "",
    return_field: "",
    import_source_field: "",
    // command's callee field is `name` (a `command_name`); the call-name hook
    // reads it directly, but keep the field accurate for completeness.
    call_function_field: "name",
    member_property_field: "",
    receiver_field: "",

    queries: &EMPTY_QUERIES,
    hooks: LanguageHooks {
        resolve_fqn: Some(bash_resolve_fqn),
        get_visibility: Some(bash_visibility),
        should_recurse_into: Some(bash_should_recurse),
        resolve_import_specifier: Some(bash_resolve_import_specifier),
        resolve_call_name: Some(bash_resolve_call_name),
        resolve_module_path: Some(bash_resolve_module_path),
        ..LanguageHooks::DEFAULT
    },
};
