//! Port trait for cross-cutting Neo4j graph WRITES that are not scoped to a
//! single repo's ingestion (unlike `IngestEdgeRepo`, whose methods all take a
//! `repo_name` and delete-then-MERGE within that repo).
//!
//! `GraphWriteRepo` owns the C2 `HTTP_CALLS` edge: a cross-repo relationship
//! that belongs to neither the client repo nor the server repo, so its rebuild
//! is GLOBAL (delete ALL `HTTP_CALLS`, then MERGE the fresh set).
//!
//! The impl lives in `akashic-store-neo4j::repos::graph_write`.

use async_trait::async_trait;

use crate::algos::http_link::CrossServiceLink;

/// Neo4j write port for global (cross-repo) graph edges.
#[async_trait]
pub trait GraphWriteRepo: Send + Sync {
    /// Idempotently rebuild ALL `HTTP_CALLS` edges: delete every existing
    /// `(:Chunk)-[:HTTP_CALLS]->()` (GLOBAL — no repo filter), then UNWIND-MERGE
    /// one edge per link keyed on `(http_method, http_path)`, setting
    /// `matched_via` + `cross_repo`. An empty `links` slice performs the delete
    /// only (clears stale edges) and returns.
    async fn create_http_calls_edges(&self, links: &[CrossServiceLink]) -> anyhow::Result<()>;
}
