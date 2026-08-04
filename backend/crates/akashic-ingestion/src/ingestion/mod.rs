pub mod accumulator;
pub mod adapter;
pub mod chunker;
pub mod clone;
pub mod corpus;
pub mod corpus_derive;
pub mod corpus_pull;
pub mod crawler;
pub mod doc_clustering;
pub mod doc_store;
pub mod entry_points;
pub mod file_discovery;
pub mod file_routes;
pub mod flows;
pub mod go_structural;
pub mod import_aliases;
pub mod pipeline;
/// `pub` (not `pub(crate)`): the `akashic-ingest` binary is a separate
/// compilation unit from this lib crate even within the same Cargo package,
/// so `pub(crate)` would not resolve across that boundary — this module (and
/// `import_graph_snapshot` within it) must be externally visible for
/// `bin/akashic-ingest.rs` to reach it as
/// `akashic_ingestion::ingestion::snapshot_commit::import_graph_snapshot`.
pub mod snapshot_commit;
pub mod stages;
pub mod store;
pub mod tier0;
pub mod virtual_modules;

#[cfg(test)]
mod e2e_test;
#[cfg(test)]
mod ram_first_e2e_test;
#[cfg(test)]
mod snapshot_e2e_test;

pub use pipeline::IngestionPipeline;
