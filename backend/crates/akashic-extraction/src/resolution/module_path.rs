//! Module-path resolution helpers. Populated in EXT-8 (tsconfig path aliases,
//! Cargo workspace member globs, Go module resolution). EXT-1 ships per-language
//! resolve_module_path hooks on each LanguageConfig; this module is the future
//! home for cross-language/workspace-aware resolution.
