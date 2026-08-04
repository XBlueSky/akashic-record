// backend/src/ingestion/extraction/walker_helpers.rs
//! Default helper functions consumed by the generic walker when
//! [`LanguageConfig`] hooks return `None`.
//!
//! All helpers are pure functions that operate on tree-sitter [`Node`]s and
//! raw UTF-8 source bytes.  They are intentionally kept free of any mutable
//! state so they can be called from multiple threads without synchronisation.
use crate::types::{ImportSpec, ScopeStack};
use tree_sitter::Node;

/// UTF-8 text of a node, propagating invalid UTF-8 as an `Err`.
pub fn utf8_text<'a>(node: Node<'a>, src: &'a [u8]) -> Result<&'a str, std::str::Utf8Error> {
    node.utf8_text(src)
}

/// Default name extraction: read the `name_field` child as utf-8.
pub fn default_name_from_field(node: Node, src: &[u8], name_field: &str) -> Option<String> {
    node.child_by_field_name(name_field)
        .and_then(|n| n.utf8_text(src).ok())
        .map(String::from)
}

/// Dotted join of the scope-frame names (e.g. "Outer.Inner"). None if empty.
pub fn scope_path(scope: &ScopeStack) -> Option<String> {
    if scope.is_empty() {
        return None;
    }
    let mut s = String::new();
    for (i, frame) in scope.iter().enumerate() {
        if i > 0 {
            s.push('.');
        }
        s.push_str(&frame.name);
    }
    Some(s)
}

/// Default FQN: dotted parent-scope path + name.  If no parent scope, returns name.
pub fn default_fqn(name: &str, scope: &ScopeStack) -> Option<String> {
    match scope_path(scope) {
        Some(p) => Some(format!("{p}.{name}")),
        None => Some(name.to_string()),
    }
}

/// Default signature: first line of the node text, trimmed of a trailing `{`.
pub fn default_signature(node: Node, src: &[u8]) -> Option<String> {
    let text = node.utf8_text(src).ok()?;
    let first = text.lines().next()?;
    Some(
        first
            .trim_end()
            .trim_end_matches('{')
            .trim_end()
            .to_string(),
    )
}

/// Default callee-name resolution from a `call_kinds` node.
///
/// The function child's node kind is matched against the declarative
/// `member_expr_kinds` registry (plus `field_expression` / `attribute` as
/// built-in fallback literals so a language that forgot to populate
/// `member_expr_kinds` still works for those common kinds).
///
/// For `obj.method(...)`: returns `"method"` (the `member_property_field` of the
/// `call_function_field`'s child).  For `foo(...)`: returns `"foo"`.
pub fn default_callee_name(
    call_node: Node,
    src: &[u8],
    member_expr_kinds: &[&str],
    call_function_field: &str,
    member_property_field: &str,
) -> Option<String> {
    let func = call_node.child_by_field_name(call_function_field)?;
    let k = func.kind();
    if member_expr_kinds.contains(&k) || k == "field_expression" || k == "attribute" {
        func.child_by_field_name(member_property_field)
            .and_then(|p| p.utf8_text(src).ok())
            .map(String::from)
    } else {
        func.utf8_text(src).ok().map(String::from)
    }
}

/// Default import specifier extraction: read `import_source_field`, strip quotes.
pub fn default_import_specifier(
    node: Node,
    src: &[u8],
    import_source_field: &str,
) -> Option<String> {
    node.child_by_field_name(import_source_field)
        .and_then(|n| n.utf8_text(src).ok())
        .map(|s| {
            s.trim_matches(|c: char| matches!(c, '\'' | '"' | '`'))
                .to_string()
        })
}

/// Per-symbol import-binding resolver shared by the TypeScript and JavaScript
/// configs (their ESM `import`/`export … from` grammars are identical apart
/// from TS-only type-only syntax). Given an `import_statement` or
/// `export_statement` node, return the LOCAL binding name(s) introduced by the
/// statement paired with the [`ImportSpec`] kind. The walker emits one Import
/// edge per returned `(local, source, spec)` (see `extract_import_edge`); an
/// empty result makes the walker fall back to a single module-level edge.
///
/// Local/source rule: the LOCAL binding is what call sites reference; the SOURCE
/// is the symbol's name in the defining module. For a non-aliased import they
/// are equal; for an aliased named import (`import { bar as baz }`) the local is
/// `baz` and the source is `bar`. Recording both lets tier-1 resolution bridge
/// `baz()` to `bar`'s definition chunk. For re-exports (`export { a as b } from
/// 'mod'`) there is no local binding; we emit the SOURCE name (`a` — the `name`
/// field) as both local and source, because that is the symbol that exists as a
/// definition in the target module.
///
/// Clause forms handled (each tuple is `(local, source, spec)`):
/// - `import Foo from 'mod'`            → `[("Foo", "Foo", Default)]`
/// - `import { a, b } from 'mod'`       → `[("a","a",Named), ("b","b",Named)]`
/// - `import { a as c } from 'mod'`     → `[("c", "a", Named)]`
/// - `import * as ns from 'mod'`        → `[("ns", "ns", Namespace)]`
/// - `import Foo, { a } from 'mod'`     → `[("Foo","Foo",Default), ("a","a",Named)]`
/// - `import 'mod'` (side-effect)       → `[]` (module-level fallback)
/// - `import type { T } from 'mod'`     → `[("T","T",TypeOnly)]`     (TS only)
/// - `import { type T, x } from 'mod'`  → `[("T","T",TypeOnly), ("x","x",Named)]` (TS)
/// - `export * from 'mod'`              → `[]` (module-level fallback)
/// - `export { a as b } from 'mod'`     → `[("a", "a", ReExport)]`
/// - `export type { C } from 'mod'`     → `[("C", "C", ReExport)]`   (TS only)
///
/// Local-only `export { x }` / `export const c` have no `source` field and are
/// dropped upstream by the specifier resolver before this hook runs, so they
/// never reach here. `support_type_only` is `true` for TS, `false` for JS.
pub fn resolve_esm_import_names(
    node: Node,
    src: &[u8],
    support_type_only: bool,
) -> Vec<(String, String, ImportSpec)> {
    match node.kind() {
        "import_statement" => resolve_import_clause(node, src, support_type_only),
        "export_statement" => resolve_reexport_clause(node, src),
        _ => Vec::new(),
    }
}

/// Pull the `(local, source)` names out of an `import_specifier` node. `source`
/// is the imported symbol's name in the defining module (the `name` field);
/// `local` is the alias if present (the `alias` field), else the source. For a
/// non-aliased `import { foo }` both are `foo`; for `import { bar as baz }` the
/// local is `baz` and the source is `bar`.
fn specifier_local_and_source(spec: Node, src: &[u8]) -> Option<(String, String)> {
    let source = spec
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .map(String::from)?;
    let local = spec
        .child_by_field_name("alias")
        .and_then(|n| n.utf8_text(src).ok())
        .map_or_else(|| source.clone(), String::from);
    Some((local, source))
}

fn resolve_import_clause(
    node: Node,
    src: &[u8],
    support_type_only: bool,
) -> Vec<(String, String, ImportSpec)> {
    let Some(clause) = node
        .children(&mut node.walk())
        .find(|c| c.kind() == "import_clause")
    else {
        // Side-effect import (`import 'mod'`): no clause → module-level fallback.
        return Vec::new();
    };

    // Clause-level `import type { … }`: a `type` keyword sits between `import`
    // and the clause, marking EVERY specifier as type-only.
    let clause_type_only = support_type_only
        && node
            .children(&mut node.walk())
            .take_while(|c| c.kind() != "import_clause")
            .any(|c| c.kind() == "type");

    let mut out = Vec::new();
    let mut cursor = clause.walk();
    for child in clause.children(&mut cursor) {
        match child.kind() {
            // `import Foo from 'mod'` — default import is a bare identifier.
            "identifier" => {
                if let Ok(name) = child.utf8_text(src) {
                    out.push((name.to_string(), name.to_string(), ImportSpec::Default));
                }
            }
            // `import * as ns from 'mod'` — local name is the trailing identifier.
            "namespace_import" => {
                if let Some(id) = child
                    .children(&mut child.walk())
                    .find(|c| c.kind() == "identifier")
                    && let Ok(name) = id.utf8_text(src)
                {
                    out.push((name.to_string(), name.to_string(), ImportSpec::Namespace));
                }
            }
            // `import { a, b as c, type T } from 'mod'`.
            "named_imports" => {
                let mut nc = child.walk();
                for spec in child.children(&mut nc) {
                    if spec.kind() != "import_specifier" {
                        continue;
                    }
                    let Some((local, source)) = specifier_local_and_source(spec, src) else {
                        continue;
                    };
                    // Inline `type` qualifier on a single specifier (TS only).
                    let inline_type = support_type_only
                        && spec.children(&mut spec.walk()).any(|c| c.kind() == "type");
                    let kind = if clause_type_only || inline_type {
                        ImportSpec::TypeOnly
                    } else {
                        ImportSpec::Named
                    };
                    out.push((local, source, kind));
                }
            }
            _ => {}
        }
    }
    out
}

/// `export … from 'mod'` re-exports. Only statements WITH a `source` field
/// reach the walker's import path (local exports are dropped by the specifier
/// resolver). `export * from 'mod'` has no clause → empty (module-level edge);
/// `export { a as b } from 'mod'` yields the SOURCE name `a` (the `name`
/// field), which is what exists as a definition in the target module.
fn resolve_reexport_clause(node: Node, src: &[u8]) -> Vec<(String, String, ImportSpec)> {
    let Some(clause) = node
        .children(&mut node.walk())
        .find(|c| c.kind() == "export_clause")
    else {
        // `export * from 'mod'` (no named clause) → module-level fallback.
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = clause.walk();
    for spec in clause.children(&mut cursor) {
        if spec.kind() != "export_specifier" {
            continue;
        }
        if let Some(name) = spec
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
        {
            // Re-exports have no local binding; emit the source name as both
            // local and source (source == local → no aliasing metadata).
            out.push((name.to_string(), name.to_string(), ImportSpec::ReExport));
        }
    }
    out
}

/// Compute the module path for a relative file path
/// (e.g. `"backend/src/foo.rs"` → `"backend/src/foo"`).
pub fn current_module_of(rel_path: &str) -> String {
    rel_path
        .rsplit_once('.')
        .map_or(rel_path, |(stem, _ext)| stem)
        .to_string()
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_ts(src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        p.parse(src, None).unwrap()
    }

    #[test]
    fn utf8_text_returns_node_text() {
        let src = "function foo() {}";
        let tree = parse_ts(src);
        let func = tree.root_node().named_child(0).unwrap();
        assert_eq!(
            utf8_text(func, src.as_bytes()).unwrap(),
            "function foo() {}"
        );
    }

    #[test]
    fn default_name_from_field_walks_field() {
        let src = "function myFunc() {}";
        let tree = parse_ts(src);
        let func = tree.root_node().named_child(0).unwrap();
        assert_eq!(
            default_name_from_field(func, src.as_bytes(), "name"),
            Some("myFunc".to_string())
        );
    }

    #[test]
    fn default_signature_returns_first_line_trimmed() {
        let src = "function foo(a: number,\n              b: string) {\n  return a;\n}";
        let tree = parse_ts(src);
        let func = tree.root_node().named_child(0).unwrap();
        assert_eq!(
            default_signature(func, src.as_bytes()),
            Some("function foo(a: number,".to_string())
        );
    }

    #[test]
    fn default_callee_name_simple_identifier() {
        let src = "foo()";
        let tree = parse_ts(src);
        let expr_stmt = tree.root_node().named_child(0).unwrap();
        let call = expr_stmt.named_child(0).unwrap();
        assert_eq!(
            default_callee_name(
                call,
                src.as_bytes(),
                &["member_expression"],
                "function",
                "property"
            ),
            Some("foo".to_string())
        );
    }

    #[test]
    fn default_callee_name_member_expression() {
        let src = "obj.method()";
        let tree = parse_ts(src);
        let expr_stmt = tree.root_node().named_child(0).unwrap();
        let call = expr_stmt.named_child(0).unwrap();
        assert_eq!(
            default_callee_name(
                call,
                src.as_bytes(),
                &["member_expression"],
                "function",
                "property"
            ),
            Some("method".to_string())
        );
    }
}
