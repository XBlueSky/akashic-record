//! `akashic-extraction` — declarative tree-sitter AST extraction engine.
//!
//! A pure, IO-free leaf crate: no async, no database, no server dependencies.
//! Provides language configs, query-driven symbol/edge extraction, and resolution
//! helpers used by the ingestion pipeline in `akashic-server`.
pub mod languages;
pub mod parser_pool;
pub mod query_cache;
pub mod registry;
pub mod resolution;
pub mod special;
pub mod types;
pub mod walker;
pub mod walker_helpers;

#[cfg(test)]
mod golden;

// Persistent real-corpus snapshot (EXT-1 Task 20). New-walker-only; survives
// Task 19.
#[cfg(test)]
mod corpus_snapshot;

pub use walker::extract;

pub use types::{
    ChunkMetadata, EdgeEndpoint, EdgeKind, EdgeMetadata, ExtractionError, ExtractionOutput,
    ExtractorKind, ImportSpec, LanguageConfig, LanguageHooks, LanguageQueries, MetadataValue,
    MethodReceiver, Provenance, QueryDef, RawChunk, RawEdge, ScopeFrame, ScopeKind, ScopeStack,
    SpecialExtractor, Visibility,
};
