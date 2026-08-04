// backend/src/ingestion/extraction/walker.rs
//! Generic tree-sitter walker: chunk extraction with scope-frame management.
//!
//! [`extract`] is the single entry point.  It parses `source` with the
//! language described by `config`, walks the AST once, and returns
//! [`ExtractionOutput`] populated with [`RawChunk`]s.
//!
//! Chunk extraction is **top-level only** by default — the recursion guard does
//! NOT descend into function/method bodies, reproducing the pre-refactor
//! per-language parsers' behavior.  A language config may override this via
//! [`LanguageHooks::should_recurse_into`].
//!
//! Edge extraction (calls/imports) uses a call-pass: import edges are emitted
//! at top level via the main recursion, while call (and nested import) edges are
//! emitted via [`call_pass`] over each LEAF chunk's subtree.
use crate::parser_pool;
use crate::types::*;
use crate::walker_helpers::*;
use tree_sitter::{Node, QueryCursor, StreamingIterator};

// ─────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────

/// Parse `source` with `config`, walk the AST once, return chunks.
///
/// Chunk extraction is top-level only — the default recursion guard does NOT
/// descend into function/method bodies, which reproduces the pre-refactor
/// per-language parsers' behavior.
mod chunks;
mod docs;
mod edges;
mod routes;
mod typerefs;

pub use chunks::extract;
pub(crate) use chunks::*;
pub(crate) use docs::*;
pub(crate) use edges::*;
pub(crate) use routes::*;
pub(crate) use typerefs::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_doc_comment_distinguishes_doc_from_ordinary() {
        assert!(is_doc_comment("/// outer doc"));
        assert!(is_doc_comment("//! inner doc"));
        assert!(is_doc_comment("  /** block doc */"));
        assert!(is_doc_comment("/*! inner block */"));
        assert!(!is_doc_comment("// ordinary"));
        assert!(!is_doc_comment("/* ordinary block */"));
        assert!(!is_doc_comment("code(); // trailing"));
    }

    #[test]
    fn extract_leading_doc_coalesces_consecutive_rust_doc_lines() {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let src = b"// not a doc\n/// First line.\n/// Second line.\npub fn add(a: i32, b: i32) -> i32 { a + b }\n";
        let tree = p.parse(&src[..], None).unwrap();
        let root = tree.root_node();
        let mut c = root.walk();
        let func = root
            .named_children(&mut c)
            .find(|n| n.kind() == "function_item")
            .expect("function_item present");
        let doc = extract_leading_doc(func, src, &["line_comment", "block_comment"]);
        // Both `///` lines are captured in source order; the ordinary `//`
        // comment above them is excluded by the doc-ness predicate.
        assert_eq!(doc.as_deref(), Some("/// First line.\n/// Second line."));
    }

    #[test]
    fn extract_leading_doc_ascends_through_export_wrapper() {
        // A TS `export function` is chunked at the inner `function_declaration`,
        // whose own preceding sibling is None — the JSDoc precedes the enclosing
        // `export_statement`. extract_leading_doc must ascend to find it.
        let mut p = tree_sitter::Parser::new();
        p.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        let src = b"// not a doc\n/** Adds two ints. */\nexport function add(a: number): number { return a; }\n";
        let tree = p.parse(&src[..], None).unwrap();
        // Descend to the inner function_declaration (inside export_statement).
        let mut func = None;
        let root = tree.root_node();
        let mut c0 = root.walk();
        for top in root.named_children(&mut c0) {
            let mut c1 = top.walk();
            for inner in top.named_children(&mut c1) {
                if inner.kind() == "function_declaration" {
                    func = Some(inner);
                }
            }
        }
        let func = func.expect("function_declaration inside export_statement");
        assert_eq!(
            func.prev_named_sibling(),
            None,
            "precondition: no direct sibling"
        );
        let doc = extract_leading_doc(func, src, &["comment"]);
        assert_eq!(doc.as_deref(), Some("/** Adds two ints. */"));
    }

    #[test]
    fn extract_leading_doc_empty_kinds_is_dormant() {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let src = b"/// doc\npub fn f() {}\n";
        let tree = p.parse(&src[..], None).unwrap();
        let root = tree.root_node();
        let mut c = root.walk();
        let func = root
            .named_children(&mut c)
            .find(|n| n.kind() == "function_item")
            .unwrap();
        // A language that has not opted in (empty doc_comment_kinds) extracts nothing.
        assert_eq!(extract_leading_doc(func, src, &[]), None);
    }

    static EMPTY_QUERIES: LanguageQueries = LanguageQueries {
        callbacks: &[],
        framework_routes: &[],
        http_calls: &[],
        extra_chunks: &[],
    };

    fn ts() -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }

    static MINIMAL_TS: LanguageConfig = LanguageConfig {
        name: "minimal_ts",
        extensions: &[".ts"],
        language_fn: ts,
        function_kinds: &["function_declaration"],
        class_kinds: &["class_declaration"],
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
        module_kinds: &[],
        trait_kinds: &[],
        impl_kinds: &[],
        import_kinds: &[],
        call_kinds: &[],
        member_expr_kinds: &[],
        type_ref_kinds: &[],
        comment_kinds: &[],
        doc_comment_kinds: &[],
        name_field: "name",
        body_field: "body",
        params_field: "parameters",
        return_field: "return_type",
        import_source_field: "source",
        call_function_field: "function",
        member_property_field: "property",
        receiver_field: "receiver",
        queries: &EMPTY_QUERIES,
        hooks: LanguageHooks::DEFAULT,
    };

    #[test]
    fn extracts_top_level_function() {
        let src = "function foo() {}";
        let out = extract(&MINIMAL_TS, src.as_bytes(), "test.ts").unwrap();
        assert_eq!(out.chunks.len(), 1);
        assert_eq!(out.chunks[0].chunk_type, "function");
        assert_eq!(out.chunks[0].name, "foo");
        assert_eq!(out.chunks[0].start_line, 1);
    }

    #[test]
    fn extracts_top_level_class() {
        let src = "class Bar {}";
        let out = extract(&MINIMAL_TS, src.as_bytes(), "test.ts").unwrap();
        assert_eq!(out.chunks.len(), 1);
        assert_eq!(out.chunks[0].chunk_type, "class");
        assert_eq!(out.chunks[0].name, "Bar");
    }

    #[test]
    fn container_fqn_is_not_self_referential() {
        let src = "class Bar {}";
        let out = extract(&MINIMAL_TS, src.as_bytes(), "test.ts").unwrap();
        assert_eq!(out.chunks.len(), 1);
        assert_eq!(out.chunks[0].name, "Bar");
        assert_eq!(
            out.chunks[0].fqn.as_deref(),
            Some("Bar"),
            "class fqn must be 'Bar', not 'Bar.Bar'"
        );
        assert_eq!(
            out.chunks[0].parent_fqn, None,
            "top-level class must have no parent_fqn"
        );
    }

    #[test]
    fn does_not_descend_into_function_body_by_default() {
        // Nested function inside outer's body must NOT be extracted (zero-diff).
        let src = "function outer() {\n  function inner() {}\n}";
        let out = extract(&MINIMAL_TS, src.as_bytes(), "test.ts").unwrap();
        assert_eq!(out.chunks.len(), 1, "only outer should be extracted");
        assert_eq!(out.chunks[0].name, "outer");
    }

    static TS_WITH_EDGES: LanguageConfig = LanguageConfig {
        name: "ts_with_edges",
        extensions: &[".ts"],
        language_fn: ts,
        function_kinds: &["function_declaration"],
        class_kinds: &["class_declaration"],
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
        module_kinds: &[],
        trait_kinds: &[],
        impl_kinds: &[],
        import_kinds: &["import_statement"],
        call_kinds: &["call_expression"],
        member_expr_kinds: &["member_expression"],
        type_ref_kinds: &[],
        comment_kinds: &[],
        doc_comment_kinds: &[],
        name_field: "name",
        body_field: "body",
        params_field: "parameters",
        return_field: "return_type",
        import_source_field: "source",
        call_function_field: "function",
        member_property_field: "property",
        receiver_field: "receiver",
        queries: &EMPTY_QUERIES,
        hooks: LanguageHooks::DEFAULT,
    };

    #[test]
    fn extracts_import_edge() {
        let src = "import { foo } from './bar';";
        let out = extract(&TS_WITH_EDGES, src.as_bytes(), "x/y.ts").unwrap();
        let imp = out
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Import)
            .expect("must produce one Import edge");
        if let EdgeEndpoint::Name {
            module_specifier, ..
        } = &imp.target
        {
            assert_eq!(module_specifier.as_deref(), Some("./bar"));
        } else {
            panic!("expected Name endpoint, got {:?}", imp.target);
        }
    }

    #[test]
    fn extracts_call_edge_inside_function_body() {
        let src = "function outer() { inner(); }";
        let out = extract(&TS_WITH_EDGES, src.as_bytes(), "x/y.ts").unwrap();
        let calls: Vec<_> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Call)
            .collect();
        assert_eq!(
            calls.len(),
            1,
            "call inside fn body must be extracted via call-pass"
        );
        if let EdgeEndpoint::Name { name, .. } = &calls[0].target {
            assert_eq!(name, "inner");
        } else {
            panic!("expected Name endpoint");
        }
    }

    #[test]
    fn extracts_implements_edges() {
        use crate::languages::RUST;
        let src = "trait Draw {}\nstruct Btn;\nimpl Draw for Btn {}";
        let out = extract(&RUST, src.as_bytes(), "x/y.rs").unwrap();
        let impls: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            impls.contains(&"Draw"),
            "expected impl edge to Draw, got {impls:?}"
        );
        // EXT-6c: the source must be the subtype's NAME ("Btn"), not a ByteRange,
        // so Stage 6 resolves the declaration-level edge by name (not byte-span).
        let edge = out
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Implements)
            .unwrap();
        assert!(
            matches!(&edge.source, EdgeEndpoint::Name { name, .. } if name == "Btn"),
            "expected Implements source = Name{{\"Btn\"}}, got {:?}",
            edge.source
        );
    }

    #[test]
    fn python_pep604_union_emits_both_operands_no_garbage() {
        // BUG 1: `A | B` and `C | None` PEP 604 unions must emit REFERENCES to
        // each real type operand (A, B, C) and NEVER a garbage "A | B" / "C | None"
        // name. The `None` builtin operand of a union is dropped.
        use crate::languages::PYTHON;
        let src = "def f(x: A | B) -> C | None:\n    pass\n";
        let out = extract(&PYTHON, src.as_bytes(), "x/y.py").unwrap();
        let type_refs: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        for want in ["A", "B", "C"] {
            assert!(
                type_refs.contains(&want),
                "expected union type ref to {want}, got {type_refs:?}"
            );
        }
        for garbage in ["A | B", "C | None"] {
            assert!(
                !type_refs.contains(&garbage),
                "must NOT emit garbage multi-token type name {garbage:?}, got {type_refs:?}"
            );
        }
        // The `None` builtin union operand is dropped (it is a `none` node, not a
        // real user type).
        assert!(
            !type_refs.contains(&"None"),
            "None union operand must be dropped, got {type_refs:?}"
        );
        // No emitted type-ref name may contain whitespace or a pipe (belt-and-braces).
        assert!(
            type_refs
                .iter()
                .all(|n| !n.contains('|') && !n.contains(' ')),
            "no type-ref name may contain '|' or whitespace, got {type_refs:?}"
        );
    }

    #[test]
    fn python_pep604_union_with_generic_operand_no_dup_no_garbage() {
        // A union operand that is itself a GENERIC parses as a `union_type`
        // node (NOT `binary_operator`) whose operands are nested `type` nodes:
        //   `List[A] | B`     → expect exactly {List, A, B} (List ONCE)
        //   `Optional[C] | None` → expect exactly {Optional, C} (None dropped)
        // Before the fix the union_type head fell through to the single-name
        // path, double-emitting the generic head (List/Optional twice) and
        // leaking `None`.
        use crate::languages::PYTHON;
        let src = "def f(x: List[A] | B, y: Optional[C] | None):\n    pass\n";
        let out = extract(&PYTHON, src.as_bytes(), "x/y.py").unwrap();
        let mut type_refs: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        type_refs.sort_unstable();
        assert_eq!(
            type_refs,
            vec!["A", "B", "C", "List", "Optional"],
            "generic-in-union must expand to each operand exactly once, \
             drop None, and emit no garbage; got {type_refs:?}"
        );
        // Belt-and-braces: no name carries a pipe or whitespace.
        assert!(
            type_refs
                .iter()
                .all(|n| !n.contains('|') && !n.contains(char::is_whitespace)),
            "no type-ref name may contain '|' or whitespace, got {type_refs:?}"
        );
    }

    #[test]
    fn csharp_type_ref_edges_carry_source_line() {
        // BUG 2: C# type-ref References edges must carry a real source line
        // (Some(..)), matching every other typed language — not the lone null.
        use crate::languages::CSHARP;
        let src = "class C {\n    private int _state;\n    public string Name(int a) { return \"x\"; }\n}\n";
        let out = extract(&CSHARP, src.as_bytes(), "x/C.cs").unwrap();
        let refs: Vec<&RawEdge> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        assert!(!refs.is_empty(), "expected some C# type-ref edges");
        assert!(
            refs.iter().all(|e| e.line.is_some()),
            "all C# type-ref edges must have Some(line); got {:?}",
            refs.iter().map(|e| (&e.target, e.line)).collect::<Vec<_>>()
        );
        // `int _state` is declared on line 2; its type ref must report line 2.
        let int_line = refs.iter().find_map(|e| match &e.target {
            EdgeEndpoint::Name { name, .. } if name == "int" => e.line,
            _ => None,
        });
        assert_eq!(
            int_line,
            Some(2),
            "the first `int` field-type ref must report source line 2"
        );
    }

    #[test]
    fn extracts_type_reference_edges() {
        use crate::languages::RUST;
        let src = "struct Bar; struct Baz; fn foo(x: Bar) -> Baz { todo!() }";
        let out = extract(&RUST, src.as_bytes(), "x/y.rs").unwrap();
        let type_refs: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            type_refs.contains(&"Bar"),
            "expected type ref to Bar, got {type_refs:?}"
        );
        assert!(
            type_refs.contains(&"Baz"),
            "expected type ref to Baz, got {type_refs:?}"
        );
        assert!(
            out.edges
                .iter()
                .filter(|e| e.kind == EdgeKind::References)
                .all(|e| matches!(
                    e.metadata.fields.get("ref_kind"),
                    Some(MetadataValue::String(s)) if s == "type"
                )),
            "all References edges must have ref_kind=type metadata"
        );
    }

    #[test]
    fn extracts_axum_routes() {
        use crate::languages::RUST;
        let src = "fn app() -> Router {\n    Router::new()\n        .route(\"/users\", get(list_users))\n        .route(\"/users/:id\", post(create_user))\n}\n";
        let out = extract(&RUST, src.as_bytes(), "x/y.rs").unwrap();
        let routes: Vec<&str> = out
            .chunks
            .iter()
            .filter(|c| c.chunk_type == "route")
            .map(|c| c.name.as_str())
            .collect();
        assert!(
            routes.iter().any(|r| r.contains("/users")),
            "expected a /users route, got {routes:?}"
        );
        let handlers: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            handlers.contains(&"list_users"),
            "expected RoutesTo list_users, got {handlers:?}"
        );
    }

    // ── EXT-7-2: Express (JS + TS) + Gin (Go) route tests ────────────────────

    #[test]
    fn extracts_express_routes_js() {
        use crate::languages::JAVASCRIPT;
        let src = "const app = express();\napp.get('/users', listUsers);\napp.post('/users', createUser);\n";
        let out = extract(&JAVASCRIPT, src.as_bytes(), "x/routes.js").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "route" && c.name.contains("/users")),
            "route chunk; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "route")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        let h: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(h.contains(&"listUsers"), "RoutesTo listUsers; got {h:?}");
    }

    #[test]
    fn extracts_express_routes_ts() {
        use crate::languages::TYPESCRIPT;
        let src = "const app = express();\napp.get('/users', listUsers);\napp.post('/users', createUser);\n";
        let out = extract(&TYPESCRIPT, src.as_bytes(), "x/routes.ts").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "route" && c.name.contains("/users")),
            "route chunk; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "route")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        let h: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(h.contains(&"listUsers"), "RoutesTo listUsers; got {h:?}");
    }

    #[test]
    fn extracts_gin_routes_go() {
        use crate::languages::GO;
        let src =
            "package main\nfunc setup(r *gin.Engine) {\n    r.GET(\"/users\", listUsers)\n}\n";
        let out = extract(&GO, src.as_bytes(), "x/routes.go").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "route" && c.name.contains("/users")),
            "route chunk; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "route")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        let h: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(h.contains(&"listUsers"), "RoutesTo listUsers; got {h:?}");
    }

    // ── EXT-7-2 Batch B: FastAPI/Flask (Python) + Spring (Java) route tests ──

    #[test]
    fn extracts_fastapi_routes_py() {
        use crate::languages::PYTHON;
        let src = "@app.get(\"/users\")\ndef list_users():\n    return []\n";
        let out = extract(&PYTHON, src.as_bytes(), "x/routes.py").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "route" && c.name.contains("/users")),
            "route chunk; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "route")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        let h: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(h.contains(&"list_users"), "RoutesTo list_users; got {h:?}");
    }

    #[test]
    fn extracts_spring_routes_java() {
        use crate::languages::JAVA;
        let src = "class C {\n    @GetMapping(\"/users\")\n    public String listUsers() { return \"x\"; }\n}\n";
        let out = extract(&JAVA, src.as_bytes(), "x/C.java").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "route" && c.name.contains("/users")),
            "route chunk; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "route")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        let h: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(h.contains(&"listUsers"), "RoutesTo listUsers; got {h:?}");
    }

    // ── EXT-7-3 Batch A: NestJS (TS) + Rails (Ruby) route tests ─────────────

    #[test]
    fn extracts_nestjs_routes_ts() {
        use crate::languages::TYPESCRIPT;
        let src = "class UsersController {\n  @Get('/users')\n  findAll() { return []; }\n}\n";
        let out = extract(&TYPESCRIPT, src.as_bytes(), "x/users.controller.ts").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "route" && c.name.contains("/users")),
            "route chunk; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "route")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        let h: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(h.contains(&"findAll"), "RoutesTo findAll; got {h:?}");
    }

    #[test]
    fn extracts_rails_routes_rb() {
        use crate::languages::RUBY;
        let src = "Rails.application.routes.draw do\n  get '/users', to: 'users#index'\nend\n";
        let out = extract(&RUBY, src.as_bytes(), "config/routes.rb").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "route" && c.name.contains("/users")),
            "route chunk; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "route")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        let h: Vec<String> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert!(
            h.iter().any(|s| s.contains("index")),
            "RoutesTo ...index; got {h:?}"
        );
    }

    // ── EXT-7-3 Batch B: Laravel (PHP) + React-Router (JSX) route tests ──────

    #[test]
    fn extracts_laravel_routes_php() {
        use crate::languages::PHP;
        // Array-style handler: Route::get('/users', [UserController::class, 'index'])
        let src = "<?php\nRoute::get('/users', [UserController::class, 'index']);\n";
        let out = extract(&PHP, src.as_bytes(), "routes/web.php").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "route" && c.name.contains("/users")),
            "route chunk; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "route")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        let h: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(h.contains(&"index"), "RoutesTo index; got {h:?}");
    }

    #[test]
    fn extracts_react_router_routes_ts() {
        // Uses the TSX config (LANGUAGE_TSX) because LANGUAGE_TYPESCRIPT does
        // not include JSX grammar rules. .tsx files route to TSX in the registry.
        use crate::languages::TSX;
        let src = "const r = <Route path=\"/users\" element={<Users/>} />;\n";
        let out = extract(&TSX, src.as_bytes(), "src/routes.tsx").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "route" && c.name.contains("/users")),
            "route chunk; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "route")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        let h: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(h.contains(&"Users"), "RoutesTo Users; got {h:?}");
    }

    #[test]
    fn extracts_react_router_routes_js() {
        use crate::languages::JAVASCRIPT;
        // JS grammar also supports JSX.
        let src = "const r = <Route path=\"/users\" element={<Users/>} />;\n";
        let out = extract(&JAVASCRIPT, src.as_bytes(), "src/routes.jsx").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "route" && c.name.contains("/users")),
            "route chunk; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "route")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        let h: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(h.contains(&"Users"), "RoutesTo Users; got {h:?}");
    }

    // ── EXT-7 route-detection regression tests (bug fixes) ───────────────────

    // Helpers for route assertions.
    fn route_names(out: &ExtractionOutput) -> Vec<String> {
        out.chunks
            .iter()
            .filter(|c| c.chunk_type == "route")
            .map(|c| c.name.clone())
            .collect()
    }
    fn routes_to_handlers(out: &ExtractionOutput) -> Vec<String> {
        out.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    // BUG 1: Axum handlers are module-qualified (`get(health::health)`), which
    // parse as `scoped_identifier`, not `identifier`. They must still match and
    // the emitted handler name must be the LAST path segment.
    #[test]
    fn axum_route_module_qualified_handler() {
        use crate::languages::RUST;
        let src = "fn app() -> Router {\n    Router::new()\n        .route(\"/health\", get(health::health))\n        .route(\"/graph\", get(graph::core::get_graph))\n}\n";
        let out = extract(&RUST, src.as_bytes(), "x/y.rs").unwrap();
        let names = route_names(&out);
        assert!(
            names.iter().any(|r| r == "GET /health"),
            "expected `GET /health` route, got {names:?}"
        );
        let h = routes_to_handlers(&out);
        // last segment, not the whole `health::health`
        assert!(
            h.contains(&"health".to_string()),
            "expected RoutesTo handler `health` (last segment), got {h:?}"
        );
        assert!(
            h.contains(&"get_graph".to_string()),
            "expected RoutesTo handler `get_graph` (last segment), got {h:?}"
        );
    }

    // BUG 2: decorator/annotation methods must normalize to plain verbs / ANY.
    #[test]
    fn normalize_http_method_cases() {
        assert_eq!(normalize_http_method("get"), "GET");
        assert_eq!(normalize_http_method("POST"), "POST");
        assert_eq!(normalize_http_method("GetMapping"), "GET");
        assert_eq!(normalize_http_method("PostMapping"), "POST");
        assert_eq!(normalize_http_method("DeleteMapping"), "DELETE");
        assert_eq!(normalize_http_method("RequestMapping"), "ANY");
        assert_eq!(normalize_http_method("route"), "ANY");
        assert_eq!(normalize_http_method("ALL"), "ALL");
    }

    // BUG 2: Spring @GetMapping must produce `GET /users`, not `GETMAPPING /users`.
    #[test]
    fn spring_route_method_normalized() {
        use crate::languages::JAVA;
        let src = "class C {\n    @GetMapping(\"/users\")\n    public String listUsers() { return \"x\"; }\n}\n";
        let out = extract(&JAVA, src.as_bytes(), "x/C.java").unwrap();
        let names = route_names(&out);
        assert!(
            names.iter().any(|r| r == "GET /users"),
            "expected `GET /users`, got {names:?}"
        );
    }

    // BUG 3: NestJS controller with >=2 decorated methods must NOT cross-product.
    #[test]
    fn nestjs_no_decorator_cross_product() {
        use crate::languages::TYPESCRIPT;
        let src = "class C {\n  @Get('/a')\n  ma() {}\n  @Post('/b')\n  mb() {}\n}\n";
        let out = extract(&TYPESCRIPT, src.as_bytes(), "x/c.controller.ts").unwrap();
        let mut names = route_names(&out);
        names.sort();
        assert_eq!(
            names,
            vec!["GET /a".to_string(), "POST /b".to_string()],
            "expected exactly 2 routes with correct method↔path pairing, got {names:?}"
        );
        // Verify pairing on the edges: GET /a → ma, POST /b → mb, no phantom.
        let mut pairs: Vec<(String, String)> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| {
                let m = e.metadata.fields.get("http_path").and_then(|v| match v {
                    MetadataValue::String(s) => Some(s.clone()),
                    _ => None,
                })?;
                let h = match &e.target {
                    EdgeEndpoint::Name { name, .. } => name.clone(),
                    _ => return None,
                };
                Some((m, h))
            })
            .collect();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("/a".to_string(), "ma".to_string()),
                ("/b".to_string(), "mb".to_string()),
            ],
            "expected exactly GET /a→ma and POST /b→mb with no phantom pairing, got {pairs:?}"
        );
    }

    // BUG 3 (TSX variant): same controller via TSX config must also not cross-product.
    #[test]
    fn nestjs_no_cross_product_tsx() {
        use crate::languages::TSX;
        let src = "class C {\n  @Get('/a')\n  ma() {}\n  @Post('/b')\n  mb() {}\n}\n";
        let out = extract(&TSX, src.as_bytes(), "x/c.controller.tsx").unwrap();
        let mut names = route_names(&out);
        names.sort();
        assert_eq!(
            names,
            vec!["GET /a".to_string(), "POST /b".to_string()],
            "TSX: expected exactly 2 routes, got {names:?}"
        );
    }

    // BUG 4: Rails handler must not retain surrounding quotes.
    #[test]
    fn rails_handler_no_quotes() {
        use crate::languages::RUBY;
        let src = "Rails.application.routes.draw do\n  post '/p', to: 'p#c'\nend\n";
        let out = extract(&RUBY, src.as_bytes(), "config/routes.rb").unwrap();
        let h = routes_to_handlers(&out);
        assert!(
            h.contains(&"p#c".to_string()),
            "expected handler `p#c` with no surrounding quotes, got {h:?}"
        );
    }

    // BUG 5(b): two same-method same-path routes must yield two DISTINCT route
    // chunks (unique fqn) so neither RoutesTo edge is orphaned by last-write-wins
    // in the chunk index — while keeping the human-facing `name` identical.
    #[test]
    fn duplicate_route_registrations_get_unique_fqn() {
        use crate::languages::RUST;
        let src = "fn app() -> Router {\n    Router::new()\n        .route(\"/x\", get(h1))\n        .route(\"/x\", get(h2))\n}\n";
        let out = extract(&RUST, src.as_bytes(), "x/y.rs").unwrap();
        let routes: Vec<&RawChunk> = out
            .chunks
            .iter()
            .filter(|c| c.chunk_type == "route")
            .collect();
        assert_eq!(
            routes.len(),
            2,
            "expected 2 route chunks for duplicate path"
        );
        // Human-facing name stays the same for both.
        assert!(
            routes.iter().all(|c| c.name == "GET /x"),
            "display name must stay `GET /x`, got {:?}",
            routes.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        // But fqns must be distinct so the chunk index doesn't collapse them.
        let fqns: std::collections::HashSet<&Option<String>> =
            routes.iter().map(|c| &c.fqn).collect();
        assert_eq!(
            fqns.len(),
            2,
            "expected 2 distinct fqns, got {:?}",
            routes.iter().map(|c| &c.fqn).collect::<Vec<_>>()
        );
    }

    // ── EXT-6b-3: C# type references ────────────────────────────────────────

    #[test]
    fn extracts_csharp_type_refs() {
        use crate::languages::CSHARP;
        let src = "public class Widget { }\npublic class Svc {\n    public Widget Make(Widget w) { return w; }\n}\n";
        let out = extract(&CSHARP, src.as_bytes(), "x/y.cs").unwrap();
        let trefs: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        // Widget appears as both the return type and the param type
        assert!(
            trefs.contains(&"Widget"),
            "expected type ref to Widget, got {trefs:?}"
        );
        // must NOT capture value identifiers (the param name `w`, the method name `Make`, the class names as self)
        assert!(
            !trefs.contains(&"Make"),
            "must not capture the method name as a type, got {trefs:?}"
        );
        assert!(
            !trefs.contains(&"w"),
            "must not capture the param NAME as a type, got {trefs:?}"
        );
    }

    // ── EXT-6b-3: Python type references ────────────────────────────────────

    #[test]
    fn extracts_python_type_refs() {
        use crate::languages::PYTHON;
        let src = "class Widget:\n    pass\n\ndef build(m: Widget) -> Widget:\n    return m\n";
        let out = extract(&PYTHON, src.as_bytes(), "x/y.py").unwrap();
        let trefs: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            trefs.contains(&"Widget"),
            "expected type ref to Widget, got {trefs:?}"
        );
        assert!(
            out.edges
                .iter()
                .filter(|e| e.kind == EdgeKind::References)
                .all(|e| matches!(
                    e.metadata.fields.get("ref_kind"),
                    Some(MetadataValue::String(s)) if s == "type"
                )),
            "all References edges must have ref_kind=type metadata"
        );
    }

    // ── EXT-7-5: Vue Router config-array route tests ─────────────────────────

    #[test]
    fn extracts_vue_router_routes_ts() {
        use crate::languages::TYPESCRIPT;
        let src = "const routes = [\n  { path: '/users', component: Users },\n  { path: '/about', component: About },\n];\n";
        let out = extract(&TYPESCRIPT, src.as_bytes(), "src/router/index.ts").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "route" && c.name.contains("/users")),
            "route chunk for /users; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "route")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        let h: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(h.contains(&"Users"), "RoutesTo Users; got {h:?}");
    }

    #[test]
    fn extracts_vue_router_routes_js() {
        use crate::languages::JAVASCRIPT;
        let src = "const routes = [\n  { path: '/users', component: Users },\n  { path: '/about', component: About },\n];\n";
        let out = extract(&JAVASCRIPT, src.as_bytes(), "src/router/index.js").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "route" && c.name.contains("/users")),
            "route chunk for /users; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "route")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        let h: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RoutesTo)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(h.contains(&"Users"), "RoutesTo Users; got {h:?}");
    }

    #[test]
    fn rust_value_method_calls_carry_inferred_recv_type() {
        use crate::languages::RUST;
        // `self.tick()` → recv_type "Counter" (self = enclosing impl type),
        // inferred. `w.paint()` with `let w: Widget` → recv_type "Widget",
        // inferred. `helper()` (bare) → no recv metadata.
        let src = "struct Counter;\n\
                   impl Counter {\n\
                       fn run(&self) {\n\
                           let w: Widget = make();\n\
                           self.tick();\n\
                           w.paint();\n\
                           helper();\n\
                       }\n\
                       fn tick(&self) {}\n\
                   }\n";
        let out = extract(&RUST, src.as_bytes(), "x/y.rs").unwrap();

        // Helper: find a Call edge by target name and return its recv_type +
        // recv_inferred metadata.
        let recv = |name: &str| -> (Option<String>, Option<bool>) {
            let e = out
                .edges
                .iter()
                .find(|e| {
                    e.kind == EdgeKind::Call
                        && matches!(&e.target, EdgeEndpoint::Name { name: n, .. } if n == name)
                })
                .unwrap_or_else(|| panic!("no Call edge to {name}; edges={:?}", out.edges));
            let ty = match e.metadata.fields.get("edge.recv_type") {
                Some(MetadataValue::String(s)) => Some(s.clone()),
                _ => None,
            };
            let inferred = match e.metadata.fields.get("edge.recv_inferred") {
                Some(MetadataValue::Bool(b)) => Some(*b),
                _ => None,
            };
            (ty, inferred)
        };

        assert_eq!(recv("tick"), (Some("Counter".to_string()), Some(true)));
        assert_eq!(recv("paint"), (Some("Widget".to_string()), Some(true)));
        // bare call: no receiver metadata at all.
        assert_eq!(recv("helper"), (None, None));
    }

    #[test]
    fn extracts_reqwest_method_call_literal_path() {
        use crate::languages::RUST;
        let src = "async fn caller() {\n    let _ = client.get(\"/api/widget\").await;\n}\n";
        let out = extract(&RUST, src.as_bytes(), "x/client.rs").unwrap();
        let hc: Vec<&str> = out
            .chunks
            .iter()
            .filter(|c| c.chunk_type == "http_call")
            .map(|c| c.name.as_str())
            .collect();
        assert!(
            hc.contains(&"GET /api/widget"),
            "expected http_call chunk 'GET /api/widget'; got {hc:?}"
        );
        // The MakesHttpCall edge targets the http_call chunk's NAME.
        let targets: Vec<&str> = out
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::MakesHttpCall)
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            targets.contains(&"GET /api/widget"),
            "expected MakesHttpCall -> 'GET /api/widget'; got {targets:?}"
        );
    }

    #[test]
    fn extracts_reqwest_free_fn_full_url_reduced_to_path() {
        use crate::languages::RUST;
        let src =
            "async fn caller() {\n    let _ = reqwest::post(\"https://svc/api/y\").await;\n}\n";
        let out = extract(&RUST, src.as_bytes(), "x/client.rs").unwrap();
        assert!(
            out.chunks
                .iter()
                .any(|c| c.chunk_type == "http_call" && c.name == "POST /api/y"),
            "full URL must reduce to path -> 'POST /api/y'; got {:?}",
            out.chunks
                .iter()
                .filter(|c| c.chunk_type == "http_call")
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn skips_interpolated_and_non_url_client_calls() {
        use crate::languages::RUST;
        // format!-interpolated arg (not a string literal) → no capture.
        let interp = "async fn a() {\n    let _ = client.get(format!(\"/api/{}\", id)).await;\n}\n";
        let out = extract(&RUST, interp.as_bytes(), "x/a.rs").unwrap();
        assert!(
            !out.chunks.iter().any(|c| c.chunk_type == "http_call"),
            "interpolated path must not be captured"
        );
        // map-style .get("key") — a string literal that is NOT URL-shaped.
        let mapget = "fn b() {\n    let _ = m.get(\"some_key\");\n}\n";
        let out = extract(&RUST, mapget.as_bytes(), "x/b.rs").unwrap();
        assert!(
            !out.chunks.iter().any(|c| c.chunk_type == "http_call"),
            "non-URL string literal must not be captured (URL-shape guard)"
        );
    }

    #[test]
    fn rust_self_recv_type_normalized_in_generic_impl() {
        use crate::languages::RUST;
        // `impl<T> Holder<T>`: self.other() must record recv_type "Holder"
        // (bare base), NOT the raw "Holder<T>", so tier-0 can match the index.
        let src = "struct Holder<T>(T);\n\
                   impl<T> Holder<T> {\n\
                       fn run(&self) { self.other(); }\n\
                       fn other(&self) {}\n\
                   }\n";
        let out = extract(&RUST, src.as_bytes(), "x/y.rs").unwrap();
        let e = out
            .edges
            .iter()
            .find(|e| {
                e.kind == EdgeKind::Call
                    && matches!(&e.target, EdgeEndpoint::Name { name, .. } if name == "other")
            })
            .expect("Call edge to other");
        assert_eq!(
            e.metadata.fields.get("edge.recv_type"),
            Some(&MetadataValue::String("Holder".to_string())),
            "self recv_type must be normalized to bare base"
        );
        assert_eq!(
            e.metadata.fields.get("edge.recv_inferred"),
            Some(&MetadataValue::Bool(true))
        );
    }
}
