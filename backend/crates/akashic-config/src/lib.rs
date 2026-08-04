//! Environment-driven configuration for the Akashic Record backend.
//!
//! Extracted from `akashic-server` so other crates can depend on the
//! `Config` type without pulling in the full server dependency graph.

mod error;
mod load;
mod redact;
mod types;
mod validate;

pub use error::{ConfigError, Violation};
pub use types::{AiProvider, AlertsConfig, Config, EmbeddingConfig, EmbeddingPrecision, LlmConfig};
pub use validate::{FORBIDDEN_PLACEHOLDERS, is_container, is_production};
