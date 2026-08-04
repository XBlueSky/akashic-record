// backend/src/ingestion/extraction/query_cache.rs
//! Lazy-compiled tree-sitter Query cache backed by [`std::sync::OnceLock`].
//!
//! [`QueryDef::compile`] compiles the S-expression source on first use and
//! stores the result in the `QueryDef`'s embedded `OnceLock<Query>`.
//! Subsequent calls return the already-compiled [`Query`] directly.
use crate::types::{ExtractionError, QueryDef};
use tree_sitter::{Language, Query};

impl QueryDef {
    /// Lazily compile and cache this query for the given language. Subsequent
    /// calls return the cached Query.
    ///
    /// Thread-safe via [`OnceLock`] — two threads racing the first call may each
    /// compile the query; the loser's result is dropped and the winner's is stored.
    /// All subsequent callers take the `get()` fast-path with no lock overhead.
    pub fn compile(
        &'static self,
        lang: &Language,
        lang_name: &'static str,
    ) -> Result<&'static Query, ExtractionError> {
        if let Some(q) = self.compiled.get() {
            return Ok(q);
        }
        let q = Query::new(lang, self.source).map_err(|e| ExtractionError::QueryCompile {
            name: self.name,
            lang: lang_name,
            source: e,
        })?;
        // get_or_init returns the value actually stored after the race resolves.
        Ok(self.compiled.get_or_init(|| q))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::QueryDef;
    use std::sync::OnceLock;

    static SAMPLE: QueryDef = QueryDef {
        name: "sample",
        source: "(function_declaration name: (identifier) @fn_name)",
        compiled: OnceLock::new(),
    };

    #[test]
    fn compiles_lazily_and_caches() {
        let lang: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let q1 = SAMPLE.compile(&lang, "typescript").unwrap();
        let q2 = SAMPLE.compile(&lang, "typescript").unwrap();
        assert!(
            std::ptr::eq(q1, q2),
            "second compile must return the cached Query"
        );
    }

    #[test]
    fn surfaces_query_compile_error() {
        static BAD: QueryDef = QueryDef {
            name: "bad",
            source: "(this is not valid s-expression",
            compiled: OnceLock::new(),
        };
        let lang: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let err = BAD.compile(&lang, "typescript").unwrap_err();
        assert!(
            matches!(err, ExtractionError::QueryCompile { .. }),
            "bad query source must surface ExtractionError::QueryCompile, got {err:?}"
        );
    }
}
