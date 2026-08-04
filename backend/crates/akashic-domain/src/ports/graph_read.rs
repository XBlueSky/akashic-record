//! Port trait for Neo4j graph-topology reads.
//!
//! `GraphReadRepo` is the infra-free domain port for all D3 topology queries:
//! the 12 handlers in `graph/core.rs`, `graph/module.rs`, and `graph/doc.rs`
//! plus the `get_project_summary` MCP tool.
//!
//! **Domain boundary rules:**
//! - No `neo4rs`, no `sqlx`, no infra types.
//! - All returns use domain structs or plain Rust primitives.
//! - `anyhow::Result` throughout.
//!
//! The impl lives in `akashic-store-neo4j::repos::graph_read`.

use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use uuid::Uuid;

// ── Row types (domain-safe) ───────────────────────────────────────────────────

/// A Neo4j Branch node.
#[derive(Debug, Clone, Serialize)]
pub struct BranchRow {
    pub name: String,
}

/// A Neo4j Note node with optional branch + category.
#[derive(Debug, Clone, Serialize)]
pub struct GraphNoteRow {
    pub uuid: String,
    pub branch: Option<String>,
    pub category: Option<String>,
}

/// A Neo4j Module node (pg_id + path).
#[derive(Debug, Clone, Serialize)]
pub struct GraphModuleRow {
    pub pg_id: String,
    pub path: String,
}

/// A Neo4j Chunk node with its owning module_id.
#[derive(Debug, Clone, Serialize)]
pub struct GraphChunkRow {
    pub pg_id: String,
    pub name: String,
    pub module_id: String,
}

/// A directional edge between two module pg_ids.
#[derive(Debug, Clone, Serialize)]
pub struct ImportEdgeRow {
    pub src_id: String,
    pub tgt_id: String,
}

/// A module node (Neo4j pass). Structurally identical to [`GraphModuleRow`];
/// kept as an alias so the two `GraphReadRepo` module-fetch methods
/// (`get_module_nodes` / `get_graph_modules`) share one row type.
pub type ModuleNodeRow = GraphModuleRow;

/// Note-count per module.
#[derive(Debug, Clone, Serialize)]
pub struct NoteCountRow {
    pub pg_id: String,
    pub note_count: i64,
}

/// A saga group as returned from the Neo4j saga-group query.
#[derive(Debug, Clone, Serialize)]
pub struct SagaGroupRow {
    pub saga_id: String,
    pub saga_name: String,
    pub module_ids: Vec<String>,
}

/// Total CALLS count for a repo.
#[derive(Debug, Clone, Serialize)]
pub struct CallsCountRow {
    pub calls_count: i64,
}

/// Module-level god-node row.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleGodNodeRow {
    pub pg_id: String,
    pub path: String,
    pub import_deg: i64,
    pub chunk_count: i64,
    pub total_deg: i64,
}

/// Chunk-level god-node row.
#[derive(Debug, Clone, Serialize)]
pub struct ChunkGodNodeRow {
    pub pg_id: String,
    pub name: String,
    pub path: String,
    pub total_calls: i64,
    pub in_calls: i64,
    pub out_calls: i64,
}

/// Total-node-count for god-node context.
#[derive(Debug, Clone, Serialize)]
pub struct TotalNodeRow {
    pub total: i64,
}

/// Intra-module CALLS edge.
#[derive(Debug, Clone, Serialize)]
pub struct CallEdgeRow {
    pub source: String,
    pub target: String,
    pub confidence: f64,
    pub method: String,
}

/// Ghost (external) node row.
#[derive(Debug, Clone, Serialize)]
pub struct GhostNodeRow {
    pub id: String,
    pub name: String,
    pub direction: String,
}

/// One EXPLAINS edge from the batched UNWIND query.
#[derive(Debug, Clone, Serialize)]
pub struct SectionExplainsRow {
    pub section_id: String,
    pub target_id: String,
    pub confidence: f64,
    pub method: String,
    pub target_repo: String,
}

/// A cluster EXPLAINS row (cluster_id → target_repo).
#[derive(Debug, Clone, Serialize)]
pub struct ClusterExplainsRow {
    pub cluster_id: String,
    pub target_repo: String,
}

/// A document EXPLAINS row (doc_id → target_repo).
#[derive(Debug, Clone, Serialize)]
pub struct DocExplainsRow {
    pub doc_id: String,
    pub target_repo: String,
}

/// A Neo4j document node (pg_id + title).
#[derive(Debug, Clone, Serialize)]
pub struct GraphDocRow {
    pub pg_id: String,
    pub title: String,
}

/// A CALLS edge with both endpoints' metadata, for community detection.
#[derive(Debug, Clone, Serialize)]
pub struct CommunityEdgeRow {
    pub source: String,
    pub source_name: String,
    pub source_module: String,
    pub target: String,
    pub target_name: String,
    pub target_module: String,
    pub weight: f64,
}

/// A zero-inbound-CALLS `function` chunk, flagged with whether it is a
/// persisted entry point. Feeds `algos::dead_code::rank`.
#[derive(Debug, Clone, Serialize)]
pub struct DeadCodeCandidateRow {
    pub name: String,
    pub module_path: String,
    pub fqn: Option<String>,
    pub visibility: String,
    pub is_entry_point: bool,
}

/// A client HTTP call site for cross-service linking: one row per
/// `(caller:function)-[:MAKES_HTTP_CALL]->(hc:http_call)`. Global (all repos).
/// method/path come from `hc`; pg_id/repo/fqn from the caller function chunk.
#[derive(Debug, Clone, Serialize)]
pub struct HttpCallSiteRow {
    pub caller_pg_id: String,
    pub caller_repo: String,
    pub caller_fqn: String,
    pub http_method: String,
    pub http_path: String,
}

/// A server route site for cross-service linking: one row per
/// `(rt:route)-[:ROUTES_TO]->(handler:function)`. Global (all repos).
/// method/path come from `rt`; pg_id/repo/fqn from the handler function chunk.
#[derive(Debug, Clone, Serialize)]
pub struct RouteSiteRow {
    pub handler_pg_id: String,
    pub handler_repo: String,
    pub handler_fqn: String,
    pub http_method: String,
    pub http_path: String,
}

/// A DECISION note attached (via `ATTACHED_TO`) to a chunk matching a searched
/// symbol. One row per (note, matching-chunk). `note_id` is parsed from the
/// Note node's `pg_id`. Feeds `algos::decision_lineage`.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionAttachmentRow {
    pub note_id: Uuid,
    pub chunk_name: String,
    pub chunk_fqn: Option<String>,
}

/// One member of a note's `SUPERSEDES` chain relative to the seed note it was
/// walked from. `direction` ∈ {"self","ancestor","descendant"}; `hop_distance`
/// is the number of `SUPERSEDES` hops from the seed (0 for the seed itself).
#[derive(Debug, Clone, Serialize)]
pub struct SupersedeChainRow {
    pub note_id: Uuid,
    pub direction: String,
    pub hop_distance: i64,
}

/// A chunk a note is directly `ATTACHED_TO` (get_decision_lineage).
#[derive(Debug, Clone, Serialize)]
pub struct ChunkRefRow {
    pub name: String,
    pub fqn: Option<String>,
    pub module_path: Option<String>,
    pub chunk_type: Option<String>,
}

// ── Port trait ────────────────────────────────────────────────────────────────

/// Neo4j read port for all D3 graph-topology queries.
///
/// One method per logical query group. Cypher is VERBATIM from the original
/// handlers; N+1 fixes (UNWIND batch, batched saga-status) and finding #38/#39
/// (no `unwrap_or_default()` on import_rows) are preserved.
#[async_trait]
pub trait GraphReadRepo: Send + Sync {
    // ── get_graph (core.rs) ──────────────────────────────────────────────────

    /// Branches for a repo.
    async fn get_branches(&self, repo: &str) -> Result<Vec<BranchRow>>;

    /// Notes (with branch + category) for a repo.
    async fn get_graph_notes(&self, repo: &str) -> Result<Vec<GraphNoteRow>>;

    /// Module nodes (pg_id + path) for a repo.
    async fn get_graph_modules(&self, repo: &str) -> Result<Vec<GraphModuleRow>>;

    /// Chunk nodes (pg_id + name + module_id) for a repo — limited to 1000.
    async fn get_graph_chunks(&self, repo: &str) -> Result<Vec<GraphChunkRow>>;

    /// IMPORTS_FROM edges between modules in a repo.
    ///
    /// **Finding #38/#39**: propagates Neo4j errors via `?` — does NOT
    /// `unwrap_or_default()`.
    async fn get_import_edges(&self, repo: &str) -> Result<Vec<ImportEdgeRow>>;

    // ── get_module_graph (module.rs) ─────────────────────────────────────────

    /// Module nodes for the module graph (pg_id + path).
    async fn get_module_nodes(&self, repo: &str) -> Result<Vec<ModuleNodeRow>>;

    /// Note count per module via ATTACHED_TO.
    async fn get_module_note_counts(&self, repo: &str) -> Result<Vec<NoteCountRow>>;

    /// IMPORTS_FROM edges for the module graph (propagates errors — #38/#39).
    async fn get_module_import_edges(&self, repo: &str) -> Result<Vec<ImportEdgeRow>>;

    /// Total CALLS edge count for a repo.
    async fn get_calls_count(&self, repo: &str) -> Result<i64>;

    /// Saga groups (sagas spanning ≥2 modules) — raw rows; caller filters by size.
    async fn get_saga_group_rows(&self, repo: &str) -> Result<Vec<SagaGroupRow>>;

    // ── get_god_nodes (core.rs) ──────────────────────────────────────────────

    /// Module-level god nodes (import degree + chunk count).
    async fn get_module_god_nodes(&self, repo: &str) -> Result<Vec<ModuleGodNodeRow>>;

    /// Chunk-level god nodes (call degree).
    async fn get_chunk_god_nodes(&self, repo: &str) -> Result<Vec<ChunkGodNodeRow>>;

    /// Total node count (modules + chunks) for god-node context.
    async fn get_total_node_count(&self, repo: &str) -> Result<i64>;

    // ── get_module_call_graph (module.rs) ────────────────────────────────────

    /// Intra-module CALLS edges.
    async fn get_intra_module_calls(&self, module_id: &str) -> Result<Vec<CallEdgeRow>>;

    /// Chunk notes (chunk_id → Vec<note_pg_id>) via ATTACHED_TO.
    async fn get_chunk_note_ids(&self, module_id: &str) -> Result<Vec<(String, Vec<String>)>>;

    /// Note pg_ids attached to a module directly.
    async fn get_module_note_ids(&self, module_id: &str) -> Result<Vec<String>>;

    /// Ghost nodes one hop out from a module via CALLS.
    async fn get_ghost_nodes(&self, module_id: &str) -> Result<Vec<GhostNodeRow>>;

    /// Cross-module CALLS edges: external→internal (callers).
    async fn get_caller_edges(&self, module_id: &str) -> Result<Vec<CallEdgeRow>>;

    /// Cross-module CALLS edges: internal→external (callees).
    async fn get_callee_edges(&self, module_id: &str) -> Result<Vec<CallEdgeRow>>;

    // ── get_doc_graph (doc.rs) ───────────────────────────────────────────────

    /// EXPLAINS edges for cluster-based doc graph.
    async fn get_cluster_explains(&self, repo: &str) -> Result<Vec<ClusterExplainsRow>>;

    /// Document nodes (pg_id + title) from Neo4j.
    async fn get_doc_nodes(&self, repo: &str) -> Result<Vec<GraphDocRow>>;

    /// EXPLAINS edges for document-based doc graph.
    async fn get_doc_explains(&self, repo: &str) -> Result<Vec<DocExplainsRow>>;

    // ── batch_section_explains (doc.rs — UNWIND, shared) ────────────────────

    /// Batch EXPLAINS lookup for a list of section pg_ids (single round-trip).
    ///
    /// Uses `UNWIND $section_ids` — preserves the N+1 fix from the handlers.
    /// Returns empty vec when `section_ids` is empty.
    async fn batch_section_explains(
        &self,
        section_ids: Vec<String>,
    ) -> Result<Vec<SectionExplainsRow>>;

    // ── relink_explains (search.rs) ──────────────────────────────────────────

    /// Collect all distinct target-repo names reachable via EXPLAINS edges from
    /// a repository's sections.
    ///
    /// Cypher (verbatim from `POST /api/v1/repos/:name/relink-explains` handler):
    /// ```cypher
    /// MATCH (r:Repository {name: $repo_name})<-[:BELONGS_TO]-(d:Document)
    ///       -[:HAS_SECTION]->(s:Section)-[e:EXPLAINS]->(target)
    /// RETURN DISTINCT e.target_repo AS target_repo
    /// ```
    ///
    /// Used by `SearchService::relink_explains` after the edge-creation pass.
    async fn explains_target_repos(&self, repo: &str) -> Result<Vec<String>>;

    // ── analytics (codebase-memory borrow: community detection) ──────────────

    /// All intra-repo `CALLS` edges (with endpoint metadata) at or above a
    /// confidence floor. Propagates Neo4j errors via `?` (#38/#39).
    async fn fetch_call_edges(
        &self,
        repo: &str,
        min_confidence: f64,
    ) -> Result<Vec<CommunityEdgeRow>>;

    /// Zero-inbound-CALLS `function` chunks for a repo, each flagged with
    /// whether a persisted `(:Chunk)-[:IS_ENTRY_POINT]->(:Flow)` edge exists.
    /// Propagates Neo4j errors via `?` (#38/#39).
    async fn fetch_zero_caller_functions(&self, repo: &str) -> Result<Vec<DeadCodeCandidateRow>>;

    /// Total count of `function` chunks in a repo (for the dead-code summary).
    async fn count_functions(&self, repo: &str) -> Result<i64>;

    /// Every client HTTP call site across ALL repos (global), for C2
    /// cross-service linking. One row per
    /// `(caller:Chunk {chunk_type:'function'})-[:MAKES_HTTP_CALL]->(hc:Chunk {chunk_type:'http_call'})`.
    /// Rows with a null/empty `http_path` are dropped (defensive). Propagates
    /// Neo4j errors via `?`.
    async fn fetch_http_call_sites(&self) -> Result<Vec<HttpCallSiteRow>>;

    /// Every server route site across ALL repos (global), for C2 cross-service
    /// linking. One row per
    /// `(rt:Chunk {chunk_type:'route'})-[:ROUTES_TO]->(handler:Chunk {chunk_type:'function'})`.
    /// Rows with a null/empty `http_path` are dropped (defensive). Propagates
    /// Neo4j errors via `?`.
    async fn fetch_route_sites(&self) -> Result<Vec<RouteSiteRow>>;

    // ── ADR-as-graph (Roadmap I: decision lineage) ───────────────────────────

    /// DECISION notes attached (via `ATTACHED_TO`) to a chunk whose `name`/`fqn`
    /// contains `symbol`. Category is filtered through the
    /// `(:Note)-[:TAGGED_AS]->(:Category {name:'DECISION'})` edge (there is NO
    /// `n.category` property on the node). `symbol` is a forgiving substring
    /// match (CONTAINS), like `traverse_calls`. Empty when nothing matches.
    async fn fetch_decisions_for_symbol(
        &self,
        repo: &str,
        symbol: &str,
    ) -> Result<Vec<DecisionAttachmentRow>>;

    /// Both directions of a note's `SUPERSEDES` chain in one query: the seed at
    /// hop 0 (`direction="self"`), older notes it supersedes
    /// (`direction="ancestor"`), and newer notes that supersede it
    /// (`direction="descendant"`). Empty when the note has no Neo4j node.
    async fn fetch_supersede_chain(&self, note_id: Uuid) -> Result<Vec<SupersedeChainRow>>;

    /// Chunks a note is directly `ATTACHED_TO` (get_decision_lineage). Empty
    /// when the note is attached to nothing.
    async fn fetch_chunks_attached_to_note(&self, note_id: Uuid) -> Result<Vec<ChunkRefRow>>;
}
