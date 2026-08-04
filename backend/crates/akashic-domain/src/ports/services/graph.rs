use super::*;

// ── GraphService (9 methods) ───────────────────────────────────────────────────

/// Use-case port for graph-topology reads.
///
/// Covers: full repo graph, god-node analysis, module graph, module detail,
/// call graph, doc graph, document/cluster drill-down, and project summary.
#[async_trait]
pub trait GraphService: Send + Sync {
    /// Lightweight graph (nodes + edges) for D3 visualization.
    ///
    /// HTTP: `GET /api/v1/graph/:repo`
    async fn get_graph(&self, repo: String) -> crate::DomainResult<GraphView>;

    /// Top-N highest-degree entities (modules + chunks).
    ///
    /// HTTP: `GET /api/v1/god-nodes/:repo`
    async fn get_god_nodes(&self, repo: String) -> crate::DomainResult<GodNodesResult>;

    /// Module-only graph with IMPORTS_FROM edges and saga groups.
    ///
    /// HTTP: `GET /api/v1/graph/:repo/modules`
    async fn get_module_graph(&self, repo: String) -> crate::DomainResult<ModuleGraphView>;

    /// Paginated module list for a repo, with total module count.
    ///
    /// HTTP: `GET /api/v1/repos/:name/modules`
    async fn list_modules(
        &self,
        repo: String,
        limit: i64,
        offset: i64,
    ) -> crate::DomainResult<(i64, Vec<crate::types::ModuleRow>)>;

    /// Paginated chunk list within a module identified by `(repo, module_path)`.
    ///
    /// HTTP: `GET /api/v1/repos/:name/modules/:path/chunks`
    async fn list_module_chunks(
        &self,
        repo: String,
        module_path: String,
        limit: i64,
        offset: i64,
    ) -> crate::DomainResult<Vec<crate::types::ChunkRow>>;

    /// Single chunk detail scoped to `(repo, chunk_id)`. `Ok(None)` if absent.
    ///
    /// HTTP: `GET /api/v1/repos/:name/chunks/:id`
    async fn get_chunk_detail(
        &self,
        repo: String,
        chunk_id: Uuid,
    ) -> crate::DomainResult<Option<crate::types::ChunkDetailRow>>;

    /// Combined module drill-down — metadata + chunks (each with its attached
    /// notes) + module-level notes — serialised as a JSON document. `Ok(None)`
    /// when the module id does not exist (handler maps to HTTP 404).
    ///
    /// HTTP: `GET /api/v1/modules/:id/chunks`
    async fn get_module_detail(&self, module_id: Uuid) -> crate::DomainResult<Option<String>>;

    /// Chunk-level CALLS graph for a module (with external ghost nodes).
    ///
    /// HTTP: `GET /api/v1/modules/:id/call-graph`
    async fn get_module_call_graph(&self, module_id: Uuid) -> crate::DomainResult<String>;

    /// Document-constellation graph (cluster-based or document-based).
    ///
    /// HTTP: `GET /api/v1/doc-graph/:repo`
    async fn get_doc_graph(&self, repo: String) -> crate::DomainResult<String>;

    /// Section detail for a specific document, including EXPLAINS edges.
    ///
    /// HTTP: `GET /api/v1/documents/:id/sections`
    async fn get_document_detail(&self, doc_id: Uuid) -> crate::DomainResult<String>;

    /// Section detail for a topic cluster.
    ///
    /// HTTP: `GET /api/v1/clusters/:id/sections`
    async fn get_cluster_detail(&self, cluster_id: Uuid) -> crate::DomainResult<String>;

    /// Resolve a corpus page's `documents.id` by `(repo, path)` — the exact
    /// key corpus-derive writes (`doc_type = "corpus"`, `source_url = path`).
    /// `Ok(None)` when no such document exists yet (derive hasn't run, or
    /// hasn't reached this page).
    ///
    /// HTTP: `GET /api/v1/docs/:repo/:version/page/*path` (Task 10, via
    /// `AppState::graph_service`)
    async fn find_corpus_document_id(
        &self,
        repo: String,
        path: String,
    ) -> crate::DomainResult<Option<Uuid>>;

    /// Project-level L0/L1 memory summary (repo statistics + top notes).
    ///
    /// MCP: `get_project_summary`
    async fn get_project_summary(
        &self,
        repo: String,
        branch: Option<String>,
    ) -> crate::DomainResult<String>;

    /// Call-graph communities for a repo via Leiden clustering.
    ///
    /// MCP: `detect_code_communities`
    async fn detect_code_communities(
        &self,
        repo: String,
        min_confidence: f64,
    ) -> crate::DomainResult<Vec<crate::algos::code_community::Community>>;

    /// Ranked dead-code candidates for a repo: zero-inbound-CALLS `function`
    /// chunks, entry points excluded, tiered by visibility.
    ///
    /// MCP: `detect_dead_code`
    async fn detect_dead_code(
        &self,
        repo: String,
    ) -> crate::DomainResult<crate::algos::dead_code::DeadCodeReport>;

    /// Trace the DECISION-note history for a code symbol: find DECISION notes
    /// attached to matching chunks, walk each one's `SUPERSEDES` chain, and
    /// return one merged, distance-ordered (nearest-first) decision timeline.
    ///
    /// MCP: `trace_decision_history`
    async fn trace_decision_history(
        &self,
        repo: String,
        symbol: String,
    ) -> crate::DomainResult<Vec<crate::algos::decision_lineage::DecisionTimelineEntry>>;

    /// Full `SUPERSEDES` lineage for one decision note (the decisions it
    /// replaced + the decisions that replaced it) plus the chunks the note
    /// itself is directly attached to.
    ///
    /// MCP: `get_decision_lineage`
    async fn get_decision_lineage(
        &self,
        note_id: Uuid,
    ) -> crate::DomainResult<crate::algos::decision_lineage::DecisionLineageReport>;

    /// Rebuild ALL cross-service `HTTP_CALLS` edges across the whole graph:
    /// match every client `http_call` `(method, path)` against every server
    /// `route`, persist one edge per match (global delete-then-MERGE), and
    /// return a report. Write operation.
    ///
    /// MCP: `link_cross_service_calls`
    async fn link_cross_service_calls(
        &self,
    ) -> crate::DomainResult<crate::algos::http_link::CrossServiceLinkReport>;
}

// ── GraphService domain response types ────────────────────────────────────────

/// A single node in the graph visualization.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    pub node_type: String,
}

/// A single edge in the graph visualization.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub edge_type: String,
}

/// Lightweight graph response (nodes + edges).
#[derive(Debug, serde::Serialize)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// A high-degree entity (module or chunk).
#[derive(Debug, Clone, serde::Serialize)]
pub struct GodNodeItem {
    pub id: String,
    pub name: String,
    pub path: String,
    pub degree: i64,
    pub node_type: String,
    pub connectivity: i64,
    pub composition: i64,
}

/// Response for `get_god_nodes`.
#[derive(Debug, serde::Serialize)]
pub struct GodNodesResult {
    pub god_nodes: Vec<GodNodeItem>,
    pub total_nodes: i64,
}

/// A module node in the module graph.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModuleGraphNode {
    pub id: String,
    pub label: String,
    pub path: String,
    pub chunk_count: i64,
    pub note_count: i64,
    pub is_virtual: bool,
    pub language: String,
}

/// A saga group (cross-module note cluster) in the module graph.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SagaGroupItem {
    pub saga_id: String,
    pub name: String,
    pub status: String,
    pub module_ids: Vec<String>,
}

/// Module graph response.
#[derive(Debug, serde::Serialize)]
pub struct ModuleGraphView {
    pub nodes: Vec<ModuleGraphNode>,
    pub edges: Vec<GraphEdge>,
    pub calls_count: i64,
    pub saga_groups: Vec<SagaGroupItem>,
}
