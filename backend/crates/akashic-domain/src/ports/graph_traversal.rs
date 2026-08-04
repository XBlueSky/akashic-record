//! Port trait for graph traversal reads (Neo4j side).
//!
//! `GraphTraversalRepo` covers expand operations (hop-1/hop-2 neighbour
//! resolution) used by `GraphRagService::graph_expand`, plus the
//! Cypher-side of `find_references` and `find_implementations` from
//! `symbol_resolution.rs`.
//!
//! A2a additions: `traverse_calls_*`, `flow_*`, and `impact_*` methods
//! that back the three previously-stubbed NavigationService methods.
//!
//! Adapter implementations live in `akashic-store-neo4j`.

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::GraphNeighbour;

/// Reference edge returned from the Neo4j traversal side of
/// `find_references` / `find_implementations`.
///
/// The enrichment (module_path, chunk_type from PG) is applied by the caller
/// after this Cypher-only query returns.
#[derive(Debug, Clone)]
pub struct ReferenceEdge {
    pub caller_fqn: String,
    pub caller_name: String,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub confidence: f64,
    pub method: String,
    pub ref_kind: String,
}

/// A module-level import reference (Module → Chunk REFERENCES import).
#[derive(Debug, Clone)]
pub struct ModuleImportRef {
    pub module_path: String,
}

// ── A2a addition: row types for traverse_code_calls ─────────────────────────

/// One hop in a call-graph traversal (`traverse_code_calls`).
#[derive(Debug, Clone)]
pub struct CallGraphRow {
    pub name: String,
    pub module: String,
    pub ctype: String,
    pub depth: i64,
    pub conf: f64,
    pub method: String,
}

// ── A2a addition: row types for trace_execution_flow ────────────────────────

/// One entry in the "list all flows" response.
#[derive(Debug, Clone)]
pub struct FlowListRow {
    pub name: String,
    pub etype: String,
    pub steps: i64,
    pub trunc: bool,
}

/// One flow that contains a searched symbol.
#[derive(Debug, Clone)]
pub struct FlowMatchRow {
    pub flow_id: String,
    pub flow_name: String,
    pub flow_etype: String,
    pub flow_steps: i64,
    pub my_position: i64,
}

/// One step inside a flow (fetched per-flow for detail output).
#[derive(Debug, Clone)]
pub struct FlowStepRow {
    pub name: String,
    pub pos: i64,
    pub depth: i64,
}

// ── A2a addition: row types for analyze_impact traversals ───────────────────

/// One CALLS-upstream hit for the impact blast-radius traversal.
#[derive(Debug, Clone)]
pub struct ImpactCallRow {
    pub pg_id: uuid::Uuid,
    pub name: String,
    pub module: String,
    pub ctype: String,
    pub depth: i64,
    pub conf: f64,
}

/// One IMPORTS_FROM hit for the impact blast-radius traversal.
#[derive(Debug, Clone)]
pub struct ImpactImportRow {
    pub pg_id: uuid::Uuid,
    pub name: String,
    pub module: String,
    pub ctype: String,
}

/// One EXPLAINS edge hit for the impact blast-radius traversal.
#[derive(Debug, Clone)]
pub struct ImpactExplainsRow {
    pub pg_id: uuid::Uuid,
    pub name: String,
    pub module: String,
    pub ctype: String,
    pub conf: f64,
}

/// One ATTACHED_TO (Note) hit for the impact blast-radius traversal.
#[derive(Debug, Clone)]
pub struct ImpactNoteRow {
    pub pg_id: uuid::Uuid,
    pub name: String,
    pub module: String,
    pub ctype: String,
}

/// One Flow that is affected by a target change.
#[derive(Debug, Clone)]
pub struct ImpactFlowRow {
    pub flow_id: String,
    pub flow_name: String,
    pub flow_etype: String,
    pub flow_steps: i64,
    pub my_pos: i64,
}

/// One entry-point chunk for a given flow (IS_ENTRY_POINT edge).
#[derive(Debug, Clone)]
pub struct FlowEntryPointRow {
    pub ep_name: String,
}

/// Repository contract for graph traversal reads in Neo4j.
///
/// All methods return `anyhow::Result` — error-type unification deferred.
/// Implementations are `Send + Sync` so they can be held behind
/// `Arc<dyn GraphTraversalRepo>`.
#[async_trait]
pub trait GraphTraversalRepo: Send + Sync {
    /// Expand a `:Chunk` seed: return parent Module, attached Notes,
    /// explaining Sections, and flow entry-point chunks.
    ///
    /// Cypher moved verbatim from
    /// `akashic-retrieval::graphrag::mod.rs::expand_chunk` (A1 Task 7).
    async fn expand_chunk(&self, pg_id: Uuid) -> anyhow::Result<Vec<GraphNeighbour>>;

    /// Expand a `:Section` seed: return targets it EXPLAINS and parent sections.
    ///
    /// Cypher moved verbatim from
    /// `akashic-retrieval::graphrag::mod.rs::expand_section` (A1 Task 7).
    async fn expand_section(&self, pg_id: Uuid) -> anyhow::Result<Vec<GraphNeighbour>>;

    /// Expand a `:Note` seed: return targets it is ATTACHED_TO.
    ///
    /// Cypher moved verbatim from
    /// `akashic-retrieval::graphrag::mod.rs::expand_note` (A1 Task 7).
    async fn expand_note(&self, pg_id: Uuid) -> anyhow::Result<Vec<GraphNeighbour>>;

    /// Find the entry-point Chunk for a Flow node.
    ///
    /// Cypher moved verbatim from
    /// `akashic-retrieval::graphrag::mod.rs::expand_chunk` flow-expansion
    /// inner loop (A1 Task 7).
    async fn find_flow_entry_points(&self, flow_id: Uuid) -> anyhow::Result<Vec<Uuid>>;

    /// Find direct (1-hop) incoming CALLS|REFERENCES references to a `fqn`.
    ///
    /// Returns raw edge rows; the caller enriches module_path/chunk_type
    /// from PG.  Cypher moved verbatim from
    /// `akashic-retrieval::graphrag::symbol_resolution::find_references`
    /// (A1 Task 7).
    async fn find_references_by_fqn(
        &self,
        fqn: &str,
        repo: Option<&str>,
        min_confidence: f64,
    ) -> anyhow::Result<Vec<ReferenceEdge>>;

    /// Find Module-level import references (Module→Chunk REFERENCES import).
    ///
    /// Cypher moved verbatim from
    /// `akashic-retrieval::graphrag::symbol_resolution::find_references`
    /// module-import branch (A1 Task 7).
    async fn find_module_imports_by_fqn(
        &self,
        fqn: &str,
        repo: Option<&str>,
    ) -> anyhow::Result<Vec<ModuleImportRef>>;

    /// Find IMPLEMENTS edges for a fqn.
    ///
    /// `implementors=true` → incoming (who implements `fqn`).
    /// `implementors=false` → outgoing (what `fqn` implements).
    ///
    /// Cypher moved verbatim from
    /// `akashic-retrieval::graphrag::symbol_resolution::find_implementations`
    /// (A1 Task 7).
    async fn find_implementations_by_fqn(
        &self,
        fqn: &str,
        repo: Option<&str>,
        implementors: bool,
    ) -> anyhow::Result<Vec<ReferenceEdge>>;

    // ── A2a: traverse_code_calls ─────────────────────────────────────────────

    /// Traverse CALLS edges downstream (`start -[:CALLS*1..N]-> callee`).
    ///
    /// Cypher moved verbatim from `mcp/tools.rs::traverse_code_calls`
    /// downstream branch (A2a).
    async fn traverse_calls_downstream(
        &self,
        symbol: &str,
        repo: Option<&str>,
        max_depth: u8,
        min_confidence: f64,
    ) -> anyhow::Result<Vec<CallGraphRow>>;

    /// Traverse CALLS edges upstream (`caller -[:CALLS*1..N]-> start`).
    ///
    /// Cypher moved verbatim from `mcp/tools.rs::traverse_code_calls`
    /// upstream branch (A2a).
    async fn traverse_calls_upstream(
        &self,
        symbol: &str,
        repo: Option<&str>,
        max_depth: u8,
        min_confidence: f64,
    ) -> anyhow::Result<Vec<CallGraphRow>>;

    // ── A2a: trace_execution_flow ────────────────────────────────────────────

    /// List all Flow nodes for a repo.
    ///
    /// Cypher moved verbatim from `mcp/tools.rs::trace_execution_flow`
    /// `list_all=true` branch (A2a).
    async fn list_flows_by_repo(&self, repo: &str) -> anyhow::Result<Vec<FlowListRow>>;

    /// Find flows containing a symbol (1-hop via FLOW_STEP).
    ///
    /// Cypher moved verbatim from `mcp/tools.rs::trace_execution_flow`
    /// symbol-search branch (A2a).
    async fn find_flows_by_symbol(
        &self,
        symbol: &str,
        repo: Option<&str>,
        entry_type: Option<&str>,
    ) -> anyhow::Result<Vec<FlowMatchRow>>;

    /// Fetch all steps of a specific flow (ordered by position).
    ///
    /// Cypher moved verbatim from `mcp/tools.rs::trace_execution_flow`
    /// per-flow step-fetch inner query (A2a).
    async fn get_flow_steps(
        &self,
        flow_id: &str,
        max_depth: u8,
    ) -> anyhow::Result<Vec<FlowStepRow>>;

    // ── A2a: analyze_impact traversals ───────────────────────────────────────

    /// CALLS upstream traversal for impact blast-radius.
    ///
    /// Cypher moved verbatim from `mcp/tools.rs::analyze_impact`
    /// traversal 1 (CalledBy / CALLS upstream) (A2a).
    async fn impact_callers(
        &self,
        target: &str,
        repo: Option<&str>,
        max_depth: u8,
        limit: usize,
    ) -> anyhow::Result<Vec<ImpactCallRow>>;

    /// IMPORTS_FROM traversal for impact blast-radius.
    ///
    /// Cypher moved verbatim from `mcp/tools.rs::analyze_impact`
    /// traversal 2 (ImportedBy / IMPORTS_FROM) (A2a).
    async fn impact_importers(
        &self,
        target: &str,
        repo: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<ImpactImportRow>>;

    /// EXPLAINS traversal for impact blast-radius.
    ///
    /// Cypher moved verbatim from `mcp/tools.rs::analyze_impact`
    /// traversal 3 (ExplainedBy / EXPLAINS) (A2a).
    async fn impact_explains(
        &self,
        target: &str,
        repo: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<ImpactExplainsRow>>;

    /// ATTACHED_TO (Note) traversal for impact blast-radius.
    ///
    /// Cypher moved verbatim from `mcp/tools.rs::analyze_impact`
    /// traversal 4 (NotedBy / ATTACHED_TO) (A2a).
    async fn impact_notes(
        &self,
        target: &str,
        repo: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<ImpactNoteRow>>;

    /// Flow step traversal for impact blast-radius (InFlow).
    ///
    /// Cypher moved verbatim from `mcp/tools.rs::analyze_impact`
    /// traversal 5 (InFlow / FLOW_STEP) (A2a).
    async fn impact_flows(
        &self,
        target: &str,
        repo: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<ImpactFlowRow>>;

    /// Entry-point chunks for a flow (IS_ENTRY_POINT edge).
    ///
    /// Cypher moved verbatim from `mcp/tools.rs::analyze_impact`
    /// per-flow IS_ENTRY_POINT inner query (A2a).
    async fn get_flow_entry_points_by_id(
        &self,
        flow_id: &str,
    ) -> anyhow::Result<Vec<FlowEntryPointRow>>;
}
