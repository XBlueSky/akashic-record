/// Category names — closed enum enforced at the application level.
/// Moved from akashic-store::schema.
pub const CATEGORIES: &[&str] = &[
    "ARCHITECTURE",
    "BUG_FIX",
    "CONFIG",
    "ONBOARDING",
    "DECISION",
];

/// Knowledge-graph search space identifier.
///
/// Moved from `akashic-retrieval::graphrag::types` (A1 Task 2) so that
/// RRF and impact aggregation can live in `akashic-domain` without creating
/// a dependency on the DB-aware retrieval crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Space {
    Code,
    Doc,
    Human,
}

/// Weight balance between BM25 (low-level) and vector (high-level) retrieval.
///
/// Produced by `algos::ranking::analyze`. Moved from retrieval types (A1 Task 2).
#[derive(Debug, Clone)]
pub struct QueryAnalysis {
    /// Weight for BM25/keyword search (0.0 to 1.0).
    pub low_level_weight: f64,
    /// Weight for vector/semantic search (0.0 to 1.0). Sum with low = 1.0.
    pub high_level_weight: f64,
}

// ── Impact analysis value types ──────────────────────────────────────────────

/// Type of impact path from target to affected node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpactPathType {
    CalledBy,
    InFlow,
    ImportedBy,
    ExplainedBy,
    NotedBy,
}

impl ImpactPathType {
    pub fn weight(self) -> f64 {
        match self {
            ImpactPathType::CalledBy => 1.0,
            ImpactPathType::InFlow => 0.9,
            ImpactPathType::ImportedBy => 0.7,
            ImpactPathType::ExplainedBy => 0.3,
            ImpactPathType::NotedBy => 0.2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ImpactPathType::CalledBy => "CalledBy",
            ImpactPathType::InFlow => "InFlow",
            ImpactPathType::ImportedBy => "ImportedBy",
            ImpactPathType::ExplainedBy => "ExplainedBy",
            ImpactPathType::NotedBy => "NotedBy",
        }
    }
}

/// A single impact path from target to an affected node.
#[derive(Debug, Clone)]
pub struct ImpactPath {
    pub path_type: ImpactPathType,
    pub hops: u32,
    pub confidence: f64,
    pub score: f64,
}

/// An affected node with its aggregated impact score.
#[derive(Debug, Clone)]
pub struct ImpactNode {
    pub id: uuid::Uuid,
    pub name: String,
    pub module_path: String,
    pub entity_type: String,
    pub impact_score: f64,
    pub paths: Vec<ImpactPath>,
}

/// A flow affected by the change.
#[derive(Debug, Clone)]
pub struct AffectedFlow {
    pub flow_name: String,
    pub entry_point: String,
    pub entry_type: String,
    pub step_position: u32,
    pub total_steps: u32,
}

/// A suggested test target derived from affected flows.
#[derive(Debug, Clone)]
pub struct SuggestedTest {
    pub entry_point: String,
    pub entry_type: String,
    pub flow_name: String,
    pub reason: String,
}
