// backend/src/ingestion/extraction/parser_pool.rs
//! Thread-local parser pool: lazily initializes one [`tree_sitter::Parser`]
//! per `(thread, language)` and reuses it across files.
//!
//! Call [`parse_with`] instead of constructing a `Parser` inline; call
//! [`validate_languages`] once at startup to catch bad language bindings early.

use crate::types::{ExtractionError, LanguageConfig};
use std::cell::RefCell;
use std::collections::HashMap;
use tree_sitter::{Parser, Tree};

thread_local! {
    static POOL: RefCell<HashMap<&'static str, Parser>> = RefCell::new(HashMap::new());
}

/// Parse `source` using the per-thread cached parser for `config`. Lazily
/// initializes a Parser for this (thread, language) on first use.
///
/// Returns `Err(ExtractionError::ParseEmpty { path: String::new() })` if
/// tree-sitter returns `None`. The empty path is deliberate — the caller
/// (the walker) fills in the real path before surfacing the error.
pub fn parse_with(config: &'static LanguageConfig, source: &[u8]) -> Result<Tree, ExtractionError> {
    POOL.with(|cell| {
        let mut pool = cell.borrow_mut();
        let parser = pool.entry(config.name).or_insert_with(|| {
            let mut p = Parser::new();
            let lang = (config.language_fn)();
            p.set_language(&lang)
                .expect("language init must succeed; verified at startup");
            tracing::debug!(target: "extraction::parser_pool",
                            lang = config.name, "parser_init");
            p
        });
        parser
            .parse(source, None)
            .ok_or_else(|| ExtractionError::ParseEmpty {
                path: String::new(),
            })
    })
}

/// Validate every language at startup. **Must be called before any worker
/// threads are spawned.** Panics if any language fails to initialise; such a
/// failure is a programming error, not a recoverable runtime condition.
///
/// This does not pre-populate the per-thread parser cache — it only confirms
/// `set_language` will succeed when threads first call [`parse_with`].
pub fn validate_languages(configs: &[&'static LanguageConfig]) {
    for cfg in configs {
        // Fresh Parser, not parse_with: avoid polluting the main-thread pool with
        // parsers that worker threads will never reuse.
        let mut p = Parser::new();
        let lang = (cfg.language_fn)();
        p.set_language(&lang)
            .unwrap_or_else(|e| panic!("language init failed for {}: {}", cfg.name, e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LanguageConfig, LanguageHooks, LanguageQueries};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static INIT_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn ts_language() -> tree_sitter::Language {
        INIT_COUNT.fetch_add(1, Ordering::SeqCst);
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }

    static EMPTY_QUERIES: LanguageQueries = LanguageQueries {
        callbacks: &[],
        framework_routes: &[],
        http_calls: &[],
        extra_chunks: &[],
    };

    static TEST_LANG: LanguageConfig = LanguageConfig {
        name: "test_ts",
        extensions: &[".ts"],
        language_fn: ts_language,
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
    fn parser_initialized_once_per_thread() {
        INIT_COUNT.store(0, Ordering::SeqCst);
        let source = b"function foo() {}";
        let _t1 = parse_with(&TEST_LANG, source).unwrap();
        let _t2 = parse_with(&TEST_LANG, source).unwrap();
        assert_eq!(
            INIT_COUNT.load(Ordering::SeqCst),
            1,
            "language_fn must be invoked exactly once per (thread, language)"
        );
    }
}
