//! Offline embedding-retrieval evaluation harness (Roadmap G, evaluation-first).
//!
//! Measures code-retrieval quality (CodeSearchNet-style doc→code) of any
//! `EmbeddingProvider`, read-only against a Postgres corpus, re-embedding
//! in-memory. Never writes embeddings or touches the schema.
//!
//! Note: the distractor pool is this repo's doc-bearing chunks only (not the
//! canonical CodeSearchNet ~1000-distractor pool), so absolute scores are
//! fair to compare provider-to-provider here but are NOT the canonical
//! CodeSearchNet benchmark number.
pub mod corpus;
pub mod metrics;
pub mod providers;
pub mod runner;
