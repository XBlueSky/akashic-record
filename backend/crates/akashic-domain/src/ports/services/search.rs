use super::*;

// ── SearchService (5 methods) ──────────────────────────────────────────────────

/// Use-case port for retrieval and search operations.
///
/// Covers: multi-space GraphRAG query, unified dual-level search,
/// detail fetch by IDs, community-level global query, and EXPLAINS re-linking.
#[async_trait]
pub trait SearchService: Send + Sync {
    /// Run the full GraphRAG scatter→expand→rank→assemble pipeline.
    ///
    /// HTTP: `POST /api/v1/graphrag/query`
    /// MCP: inner engine for `search_knowledge` (GraphRAG mode).
    async fn graphrag_query(&self, req: GraphRagQuery) -> crate::DomainResult<GraphRagResponse>;

    /// Unified dual-level (BM25 + vector) search across code, docs, and notes.
    ///
    /// HTTP: `GET /api/v1/search`
    /// MCP: `search_knowledge`
    async fn search_knowledge(
        &self,
        req: KnowledgeSearchRequest,
    ) -> crate::DomainResult<Vec<KnowledgeSearchItem>>;

    /// Fetch full content for a batch of entity IDs.
    ///
    /// Returns opaque JSON values keyed by ID (chunks, notes, sections each
    /// have different shapes). The `Vec<serde_json::Value>` return preserves
    /// the existing per-entity-type polymorphism without requiring a large sum
    /// type in the domain layer.
    ///
    /// HTTP: `GET /api/v1/details?ids=…`
    /// MCP: `get_details`
    async fn get_details(
        &self,
        ids: Vec<Uuid>,
        repo: Option<String>,
    ) -> crate::DomainResult<Vec<serde_json::Value>>;

    /// Community-level global query using Leiden community detection summaries.
    ///
    /// MCP: `global_query`
    async fn global_query(
        &self,
        query: String,
        repo: String,
        level: Option<u8>,
        include_examples: bool,
        max_communities: usize,
        prefer_space: Option<PreferSpace>,
    ) -> crate::DomainResult<Vec<CommunitySummaryItem>>;

    /// Re-run EXPLAINS edge linking for a repository and re-cluster sections.
    ///
    /// HTTP: `POST /api/v1/repos/:name/relink-explains`
    async fn relink_explains(
        &self,
        repo: String,
        user_token: Option<String>,
    ) -> crate::DomainResult<RelinkResult>;
}

/// A community summary result from `SearchService::global_query`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommunitySummaryItem {
    pub id: Uuid,
    pub name: String,
    pub summary: String,
    pub level: i16,
    pub member_count: i32,
    pub score: f64,
    /// Example chunk triples `(name, chunk_type, module_path)`.
    pub examples: Vec<(String, String, String)>,
}
