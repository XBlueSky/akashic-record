use crate::types::*;
use tree_sitter::Node;

fn language() -> tree_sitter::Language {
    tree_sitter_objc::LANGUAGE.into()
}

static EMPTY_QUERIES: LanguageQueries = LanguageQueries {
    callbacks: &[],
    framework_routes: &[],
    http_calls: &[],
    extra_chunks: &[],
};

/// Assemble an Objective-C selector from a method declaration / definition.
///
/// GRAMMAR (tree-sitter-objc 3.0.2): a `method_declaration` (in `@interface` /
/// `@protocol`) and a `method_definition` (in `@implementation`) have NO `name`
/// field.  Their shape is:
///
///   `- (ReturnType) keyword0 :(T0)arg0 keyword1 :(T1)arg1 …`
///
/// which parses to a `method_type` (the return type) followed by a sequence of
/// keyword `identifier` children, each (when it takes an argument) paired with a
/// following `method_parameter` child.  A no-argument selector is a single bare
/// `identifier` with zero `method_parameter`s.
///
/// SELECTOR ASSEMBLY SCHEME (ObjC convention, used for BOTH method names and
/// message-send call names so call edges match method declarations):
///
///   * no arguments  → just the keyword, NO trailing colon: `increment`
///   * one keyword + arg(s) → keyword(s) each suffixed with a colon:
///     `nameForId:`, `setValue:andLabel:`
///
/// Concretely: collect the direct-child keyword `identifier`s and count the
/// `method_parameter` children.  If there are zero parameters, the selector is
/// the single keyword verbatim.  Otherwise every keyword gets a trailing colon
/// (keyword count == parameter count for any well-formed ObjC selector).
fn objc_assemble_method_selector(node: Node, src: &[u8]) -> Option<String> {
    let mut keywords: Vec<&str> = Vec::new();
    let mut param_count = 0usize;
    for i in 0..node.named_child_count() {
        let Some(c) = node.named_child(i) else {
            continue;
        };
        match c.kind() {
            "identifier" => {
                if let Ok(t) = c.utf8_text(src) {
                    keywords.push(t);
                }
            }
            "method_parameter" => param_count += 1,
            _ => {}
        }
    }
    if keywords.is_empty() {
        return None;
    }
    if param_count == 0 {
        // No-argument selector: a single bare keyword, no colon.
        Some(keywords.join(""))
    } else {
        // Argument selector: each keyword carries a trailing colon.
        Some(keywords.iter().map(|k| format!("{k}:")).collect())
    }
}

/// Resolve the display name of an Objective-C construct.
///
///   * `class_interface` / `class_implementation` — the class name is the FIRST
///     `identifier` child (NOT a `name` field; `superclass` / `category` ARE
///     fields, the class name is not).  For a CATEGORY (`@interface Foo (Cat)` /
///     `@implementation Foo (Cat)`) the `category` field is present; we name the
///     chunk `Foo(Cat)` so the two category halves group under a distinct name
///     from the base class.
///   * `protocol_declaration` — name is the first `identifier` child.
///   * `method_declaration` / `method_definition` — assemble the selector.
///   * plain C `function_definition` / `struct`/`enum`/`typedef`/macros — fall
///     back to the declarator-chain / `name`-field resolution used by c.rs.
fn objc_resolve_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "class_interface" | "class_implementation" => {
            let base = first_identifier(node, src)?;
            if let Some(cat) = node
                .child_by_field_name("category")
                .and_then(|n| n.utf8_text(src).ok())
            {
                Some(format!("{base}({cat})"))
            } else {
                Some(base)
            }
        }
        "protocol_declaration" => first_identifier(node, src),
        "method_declaration" | "method_definition" => objc_assemble_method_selector(node, src),
        "struct_specifier" | "enum_specifier" | "union_specifier" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from),
        "type_definition" => node
            .child_by_field_name("declarator")
            .and_then(|d| innermost_identifier(d, src)),
        "preproc_def" | "preproc_function_def" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from),
        _ => node
            .child_by_field_name("declarator")
            .and_then(|d| innermost_identifier(d, src)),
    }
}

/// First direct-child `identifier` of a node (the class / protocol name in
/// tree-sitter-objc, which exposes it as an unnamed positional child rather than
/// a `name` field).
fn first_identifier(node: Node, src: &[u8]) -> Option<String> {
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i)
            && c.kind() == "identifier"
        {
            return c.utf8_text(src).ok().map(String::from);
        }
    }
    None
}

/// Walk a (possibly nested) C declarator to its innermost identifier (shared
/// with the C-superset function/typedef handling).
fn innermost_identifier(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "type_identifier" | "field_identifier" => {
            return node.utf8_text(src).ok().map(String::from);
        }
        _ => {}
    }
    if let Some(inner) = node.child_by_field_name("declarator")
        && let Some(s) = innermost_identifier(inner, src)
    {
        return Some(s);
    }
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i)
            && let Some(s) = innermost_identifier(c, src)
        {
            return Some(s);
        }
    }
    None
}

/// Resolve the FQN for an Objective-C declaration.
///
/// FQN scheme: `.`-joined — `Counter.increment`, `Counter.setValue:andLabel:`.
/// The selector (with its internal colons) is a single `.`-separated segment;
/// the `.` only separates the class/protocol name from the selector.  This is
/// the identity key every config and the walker's `parent_fqn` (`scope_path`)
/// use, so a method's `parent_fqn` exactly equals its class chunk's `fqn`.
///
/// We consult the walker's scope stack first (it pushes a Class frame for
/// `class_interface` / `class_implementation` and an Interface frame for
/// `protocol_declaration`, all of which are scope containers).  As a defensive
/// fallback we climb the AST to the nearest enclosing type container.  Plain C
/// functions at file scope have an empty scope and resolve to their bare name.
fn objc_resolve_fqn(node: Node, src: &[u8], scope: &ScopeStack) -> Option<String> {
    let name = objc_resolve_name(node, src)?;

    if !scope.is_empty() {
        let prefix = scope
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        return Some(format!("{prefix}.{name}"));
    }

    let container_kinds = [
        "class_interface",
        "class_implementation",
        "protocol_declaration",
    ];
    let mut parent = node.parent();
    while let Some(p) = parent {
        if container_kinds.contains(&p.kind())
            && let Some(cn) = objc_resolve_name(p, src)
        {
            return Some(format!("{cn}.{name}"));
        }
        parent = p.parent();
    }

    Some(name)
}

/// Recursion guard.
///
/// We descend the root plus every Objective-C type container so methods inside
/// are reached and chunked:
///
///   * `class_interface` / `protocol_declaration` — hold `method_declaration`s
///     as DIRECT children.
///   * `class_implementation` — holds `method_definition`s wrapped one level
///     deeper inside an `implementation_definition` node; we descend BOTH so the
///     methods surface.  (`implementation_definition` also wraps any plain C
///     `function_definition` written inside `@implementation`.)
///
/// CRITICAL — no nested-body leak: method/function bodies are `compound_statement`
/// nodes, which are deliberately NOT listed here.  A callable's interior is
/// therefore never walked for chunks (no nested-def leak); its interior calls
/// are instead reached via the walker's call-pass over each leaf chunk.
fn objc_should_recurse(node: Node) -> bool {
    node.parent().is_none()
        || matches!(
            node.kind(),
            "class_interface"
                | "class_implementation"
                | "protocol_declaration"
                | "implementation_definition"
        )
}

/// Extract the callee name from a call site.
///
/// Objective-C call sites are of two kinds:
///
///   * `message_expression` `[receiver selPart:arg …]` — the callee is the
///     SELECTOR, assembled from the `method`-field keyword `identifier`s using
///     the SAME scheme as method names so a message send matches its method
///     declaration's selector.  Argument count (the non-`method` `identifier`
///     children) decides the colon convention: zero args → bare keyword
///     (`increment`), one-or-more → colon-suffixed keywords
///     (`setValue:andLabel:`).
///   * plain C `call_expression` `foo(...)` — the callee is the `function`
///     field's text (matching c.rs).
fn objc_resolve_call_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "message_expression" => {
            let mut keywords: Vec<&str> = Vec::new();
            let mut arg_count = 0usize;
            for i in 0..node.named_child_count() {
                let Some(child) = node.named_child(i) else {
                    continue;
                };
                match node.field_name_for_named_child(i as u32) {
                    Some("receiver") => {}
                    Some("method") => {
                        if let Ok(t) = child.utf8_text(src) {
                            keywords.push(t);
                        }
                    }
                    // Field-less named children are the call arguments.
                    _ => arg_count += 1,
                }
            }
            if keywords.is_empty() {
                return None;
            }
            if arg_count == 0 {
                Some(keywords.join(""))
            } else {
                Some(keywords.iter().map(|k| format!("{k}:")).collect())
            }
        }
        _ => node
            .child_by_field_name("function")
            .and_then(|f| f.utf8_text(src).ok())
            .map(String::from),
    }
}

/// Return true when a plain C `function_definition` has a `static`
/// storage-class specifier (mirrors c.rs / cpp.rs).
fn objc_is_static(node: Node) -> bool {
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i)
            && c.kind() == "storage_class_specifier"
        {
            for j in 0..c.child_count() {
                if c.child(j).is_some_and(|kw| kw.kind() == "static") {
                    return true;
                }
            }
        }
    }
    false
}

/// Extract the import path from a `preproc_include` (`#import` / `#include`),
/// stripping the surrounding `"..."` or `<...>` delimiters (mirrors c.rs).
fn objc_resolve_import_specifier(node: Node, src: &[u8]) -> Option<String> {
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i)
            && matches!(c.kind(), "string_literal" | "system_lib_string")
        {
            return c.utf8_text(src).ok().map(|t| {
                t.trim_matches(|ch: char| matches!(ch, '"' | '<' | '>'))
                    .to_string()
            });
        }
    }
    None
}

/// Resolve an `#import` / `#include` path to a module path, relative to the
/// current file's directory (identical to c.rs).  System headers such as
/// `<Foundation/Foundation.h>` become `dir/Foundation/Foundation` and remain
/// unresolvable (harmless), exactly as c.rs treats `<stdio.h>`.
fn objc_resolve_module_path(specifier: &str, current_module: &str) -> Option<String> {
    let mut segments: Vec<&str> = current_module
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    segments.pop(); // drop file stem → its containing directory
    for part in specifier.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other.trim_end_matches(".h")),
        }
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}

/// Objective-C `LanguageConfig`.
///
/// CLASSIFICATION: a class's `@interface` (`class_interface`) and its
/// `@implementation` (`class_implementation`) are BOTH classified as `class`.
/// Because ObjC splits a class across two top-level constructs, ONE class
/// commonly yields TWO `class` chunks — one for the interface (declarations)
/// and one for the implementation (definitions).  This is inherent to the
/// language's split-declaration model and is accepted as-is; methods nest under
/// the correct half via the scope stack (`@interface` methods are
/// `method_declaration`, `@implementation` methods are `method_definition`).
/// A `@protocol` (`protocol_declaration`) is classified as `interface` (its
/// closest analog).  Categories (`@interface Foo (Cat)` / `@implementation Foo
/// (Cat)`) reuse `class_interface` / `class_implementation` and are likewise
/// `class` chunks, named `Foo(Cat)` to distinguish them from the base class.
///
/// SELECTOR NAMING: method names AND message-send call names are assembled
/// selectors (`increment`, `setValue:andLabel:`) so call edges resolve against
/// method declarations.  FQNs are `.`-joined (`Counter.setValue:andLabel:`); the
/// selector's internal colons stay inside its single `.`-segment.
///
/// C-SUPERSET: plain C `function_definition`, `struct`/`enum`/`union`,
/// `typedef`, and `#define` macros are chunked exactly as in c.rs; C
/// `call_expression`s and `#import`/`#include`s reuse c.rs's resolution.
///
/// VISIBILITY: ObjC methods carry no access keywords (all effectively public)
/// and instance-variable `@private` / `@protected` / `@public` sections are
/// niche; we default visibility to Unknown (the shared walker default) rather
/// than synthesizing access levels — acceptable for a v1 config.
///
/// `.h` ROUTING: `.h` headers stay routed to the C++ extractor by the existing
/// `lookup_for_path` special-case (`.h` is ambiguous C/C++/ObjC).  This config
/// claims only `.m` and `.mm`, so ObjC headers route to cpp — an accepted known
/// limitation documented in registry.rs.
pub static OBJC: LanguageConfig = LanguageConfig {
    name: "objc",
    // `.m` = ObjC implementation, `.mm` = ObjC++ implementation. `.h` is left to
    // the existing C++ special-case (see registry.rs / struct doc comment).
    extensions: &[".m", ".mm"],
    language_fn: language,

    function_kinds: &["function_definition"],
    // BOTH @interface and @implementation classify as `class` (see doc comment).
    class_kinds: &["class_interface", "class_implementation"],
    method_kinds: &["method_declaration", "method_definition"],
    // @protocol is ObjC's interface analog.
    interface_kinds: &["protocol_declaration"],
    struct_kinds: &["struct_specifier"],
    enum_kinds: &["enum_specifier"],
    enum_member_kinds: &["enumerator"],
    type_alias_kinds: &["type_definition"],
    variable_kinds: &[],
    constant_kinds: &["preproc_def"],
    macro_kinds: &["preproc_function_def"],
    namespace_kinds: &[],
    module_kinds: &[],
    trait_kinds: &[],
    impl_kinds: &[],

    import_kinds: &["preproc_include"],
    // message_expression = ObjC send; call_expression = plain C call.
    call_kinds: &["message_expression", "call_expression"],
    member_expr_kinds: &["field_expression"],
    type_ref_kinds: &[],

    comment_kinds: &["comment"],
    doc_comment_kinds: &[],

    name_field: "declarator",
    body_field: "body",
    params_field: "parameters",
    return_field: "type",
    import_source_field: "path",
    // Call callee names are resolved via objc_resolve_call_name (message sends
    // assemble a selector; C calls read the `function` field).
    call_function_field: "function",
    member_property_field: "field",
    receiver_field: "",

    queries: &EMPTY_QUERIES,
    hooks: LanguageHooks {
        resolve_name: Some(objc_resolve_name),
        resolve_fqn: Some(objc_resolve_fqn),
        is_static: Some(objc_is_static),
        should_recurse_into: Some(objc_should_recurse),
        resolve_import_specifier: Some(objc_resolve_import_specifier),
        resolve_call_name: Some(objc_resolve_call_name),
        resolve_module_path: Some(objc_resolve_module_path),
        ..LanguageHooks::DEFAULT
    },
};
