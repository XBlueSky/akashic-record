use uuid::Uuid;

// Re-export types that have been relocated to akashic-domain (A1 Tasks 2 + 10).
// All downstream code importing from this module continues to compile unchanged.
pub use akashic_domain::types::{
    AffectedFlow, DedupMatch, DedupVerdict, GraphRagQuery, GraphRagResponse, ImpactNode,
    ImpactPath, ImpactPathType, NoteHealthEntry, NoteHealthSummary, PreferSpace, QueryAnalysis,
    ScoredNode, Space, SuggestedTest,
};

#[derive(Debug, Clone)]
pub struct SeedNode {
    pub pg_id: Uuid,
    pub space: Space,
    pub entity_type: String,
    pub name: String,
    pub score: f32,
}

#[derive(Debug, Clone)]
pub struct ExpandedNode {
    pub pg_id: Uuid,
    pub space: Space,
    pub entity_type: String,
    pub name: String,
    pub vector_score: Option<f32>,
    pub parent_score: Option<f32>,
    pub hop_distance: u8,
    pub content: String,
    pub module_path: Option<String>,
    pub language: Option<String>,
    pub document_title: Option<String>,
    pub heading: Option<String>,
    pub author: Option<String>,
    pub category: Option<String>,
}
