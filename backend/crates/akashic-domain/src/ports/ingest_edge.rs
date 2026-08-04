//! Port traits for ingestion-time Neo4j edge creation and flow persistence.
//!
//! `IngestEdgeRepo` consolidates the CALLS, REFERENCES, IMPLEMENTS, ROUTES_TO,
//! IMPORTS_FROM, and REFERENCES(import) edges created during the ingestion
//! pipeline (Stages 5 and 6). Adapter implementations live in
//! `akashic-store-neo4j`.
//!
//! `RepoGraphRepo` covers the `MERGE (r:Repository {name: …})` DDL that
//! ensures the root Repository node exists before any other writes.
//!
//! `FlowGraphRepo` covers Flow node persistence (store + delete) in Stage 7.
//! The CALLS-adjacency read side of Stage 7 is now in-memory (see
//! `akashic-ingestion::ingestion::flows::calls_adjacency_from_accumulator`),
//! not a Neo4j-backed port.

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::{FlowRecord, IngestEdge};

// ── RepoGraphRepo ─────────────────────────────────────────────────────────────

/// Repository contract for the top-level `:Repository` node in Neo4j.
#[async_trait]
pub trait RepoGraphRepo: Send + Sync {
    /// MERGE a `:Repository` node by name (creates it if missing).
    ///
    /// Cypher moved verbatim from
    /// `akashic-ingestion::ingestion::pipeline::run_inner` Stage 3 (A1 Task 8).
    async fn ensure_repository_node(&self, repo_name: &str) -> anyhow::Result<()>;

    /// DETACH DELETE the `:Repository` node and all nodes reachable from it.
    ///
    /// Cypher moved verbatim from
    /// `akashic-server::api::routes::repos::delete_repo_data` (A2a Task 8).
    ///
    /// This is the Neo4j half of `delete_repo`; the PostgreSQL half is
    /// handled by the service via a transaction on the PG pool.
    async fn delete_repo_graph(&self, repo_name: &str) -> anyhow::Result<()>;

    /// MERGE a `:Repository` + `:Branch` node pair, setting `last_commit_hash`.
    ///
    /// Cypher moved verbatim from
    /// `akashic-server::gitlab::webhook::sync_branch` (A2a Task 9).
    async fn sync_branch(
        &self,
        repo_name: &str,
        branch_name: &str,
        commit_hash: &str,
    ) -> anyhow::Result<()>;

    /// MERGE a `:Repository` + `:Tag` node pair, setting `commit_hash`.
    ///
    /// Cypher moved verbatim from
    /// `akashic-server::gitlab::webhook::sync_tag` (A2a Task 9).
    async fn sync_tag(
        &self,
        repo_name: &str,
        tag_name: &str,
        commit_hash: &str,
    ) -> anyhow::Result<()>;
}

// ── IngestEdgeRepo ────────────────────────────────────────────────────────────

/// Repository contract for bulk edge creation during the ingestion pipeline.
///
/// All methods operate on pre-resolved `IngestEdge` slices — the caller is
/// responsible for resolving (src_path, tgt_path) → chunk ids before calling.
#[async_trait]
pub trait IngestEdgeRepo: Send + Sync {
    /// Delete existing CALLS edges for a repo, then UNWIND-MERGE the new set.
    ///
    /// Cypher moved verbatim from
    /// `akashic-ingestion::ingestion::store::create_call_edges` (A1 Task 8).
    async fn create_call_edges(&self, repo_name: &str, calls: &[IngestEdge]) -> anyhow::Result<()>;

    /// Delete existing REFERENCES edges for a repo, then UNWIND-MERGE.
    ///
    /// Cypher moved verbatim from
    /// `akashic-ingestion::ingestion::store::create_reference_edges` (A1 Task 8).
    async fn create_reference_edges(
        &self,
        repo_name: &str,
        refs: &[IngestEdge],
    ) -> anyhow::Result<()>;

    /// Delete existing IMPLEMENTS edges for a repo, then UNWIND-MERGE.
    ///
    /// Cypher moved verbatim from
    /// `akashic-ingestion::ingestion::store::create_implements_edges` (A1 Task 8).
    async fn create_implements_edges(
        &self,
        repo_name: &str,
        impls: &[IngestEdge],
    ) -> anyhow::Result<()>;

    /// Delete existing ROUTES_TO edges for a repo, then UNWIND-MERGE.
    ///
    /// Cypher moved verbatim from
    /// `akashic-ingestion::ingestion::store::create_routes_to_edges` (A1 Task 8).
    async fn create_routes_to_edges(
        &self,
        repo_name: &str,
        routes: &[IngestEdge],
    ) -> anyhow::Result<()>;

    /// Delete existing MAKES_HTTP_CALL edges for a repo, then UNWIND-MERGE.
    ///
    /// C1: `(caller:Chunk)-[:MAKES_HTTP_CALL]->(hc:Chunk {chunk_type:'http_call'})`.
    async fn create_makes_http_call_edges(
        &self,
        repo_name: &str,
        calls: &[IngestEdge],
    ) -> anyhow::Result<()>;

    /// Delete existing import REFERENCES edges, then UNWIND-MERGE new set.
    ///
    /// `imports` is a list of `(module_path, symbol_chunk_id)` pairs.
    ///
    /// Cypher moved verbatim from
    /// `akashic-ingestion::ingestion::store::create_symbol_import_edges` (A1 Task 8).
    async fn create_symbol_import_edges(
        &self,
        repo_name: &str,
        imports: &[(String, Uuid)],
    ) -> anyhow::Result<()>;

    /// UNWIND-MERGE IMPORTS_FROM edges between Module nodes.
    ///
    /// `edges` is a list of `(src_module_pg_id, tgt_module_pg_id)` pairs that
    /// have already been resolved from module paths by `ModuleRepo::resolve_module_paths`.
    ///
    /// Cypher moved verbatim from the Neo4j half of
    /// `akashic-ingestion::ingestion::store::create_import_edges` (A1 Task 8).
    async fn create_import_edges(
        &self,
        src_ids: &[String],
        tgt_ids: &[String],
    ) -> anyhow::Result<()>;
}

// ── FlowGraphRepo ─────────────────────────────────────────────────────────────

/// Repository contract for writing execution-flow nodes to Neo4j during Stage 7.
#[async_trait]
pub trait FlowGraphRepo: Send + Sync {
    /// DETACH DELETE all `:Flow` nodes for a repo, then CREATE the new set.
    ///
    /// Cypher moved verbatim from
    /// `akashic-ingestion::ingestion::flows::store_flows` (A1 Task 8).
    async fn store_flows(&self, repo_name: &str, flows: &[FlowRecord]) -> anyhow::Result<()>;
}
