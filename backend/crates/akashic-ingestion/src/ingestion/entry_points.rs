//! Re-export: entry-point detection moved to `akashic-domain::algos::entry_points` (A1 Task 2).
//!
//! All call sites importing from this module continue to compile unchanged.

pub use akashic_domain::algos::entry_points::{DetectedEntryPoint, detect};

// Re-export the language-specific detection helpers so any existing
// `super::entry_points::detect_rust` / `detect_typescript` path still resolves.
// (They are private in the domain crate, so callers that used them as public
// through this module would have had access only if this file had `pub fn`
// wrappers — which it did not. Safe to leave as the domain canonical only.)
