//! Repo-scoped ingestion snapshot types (Roadmap E2).
//!
//! These are full-fidelity row/node shapes for cross-database export/import
//! — distinct from the analysis-shaped ports (`ModuleRow`, `ChunkFullRow`,
//! `GraphReadRepo`'s projections), which omit fields (embeddings, repo_name,
//! is_virtual, git_ref) that round-trip fidelity requires.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ports::ChunkGraphNode;
use crate::types::{FlowRecord, IngestEdge};

/// Full `modules` row for one repo, keyed by its original UUID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleSnapshotRow {
    pub id: Uuid,
    pub path: String,
    pub language: Option<String>,
    pub summary: Option<String>,
    pub exports_count: Option<i32>,
    pub file_count: Option<i32>,
    pub is_virtual: bool,
    pub embedding: Option<Vec<f32>>,
    pub git_ref: Option<String>,
    pub ingested_at: Option<DateTime<Utc>>,
}

/// Full `chunks` row for one repo, keyed by its original UUID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkSnapshotRow {
    pub id: Uuid,
    pub module_path: String,
    pub chunk_type: String,
    pub name: String,
    pub signature: Option<String>,
    pub content: String,
    pub language: Option<String>,
    pub embedding: Vec<f32>,
    pub signature_embedding: Option<Vec<f32>>,
    pub git_ref: Option<String>,
    pub fqn: Option<String>,
    pub parent_fqn: Option<String>,
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
    pub visibility: Option<String>,
    pub is_async: bool,
    pub is_static: bool,
    pub is_exported: bool,
    pub doc: Option<String>,
    pub ingested_at: Option<DateTime<Utc>>,
}

/// Full `large_chunks` row for one repo, keyed by its original UUID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LargeChunkSnapshotRow {
    pub id: Uuid,
    pub module_path: String,
    pub chunk_ids: Vec<Uuid>,
    pub content: String,
    pub embedding: Vec<f32>,
    pub git_ref: Option<String>,
    pub ingested_at: Option<DateTime<Utc>>,
}

/// Full `communities` row for one repo, keyed by its original UUID.
/// `parent_id` (if present) refers to another `CommunitySnapshotRow.id` in the
/// same snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunitySnapshotRow {
    pub id: Uuid,
    pub level: i16,
    pub name: Option<String>,
    pub summary: Option<String>,
    pub member_count: Option<i32>,
    pub embedding: Option<Vec<f32>>,
    pub parent_id: Option<Uuid>,
    pub created_at: Option<DateTime<Utc>>,
}

/// One `community_members` row. Exactly one of `chunk_id`/`module_id` is `Some`,
/// matching the table's two nullable FK columns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityMemberSnapshotRow {
    pub community_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub module_id: Option<Uuid>,
}

/// Everything the ingestion pipeline wrote to Postgres for one repo.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PgRepoSnapshot {
    pub modules: Vec<ModuleSnapshotRow>,
    pub chunks: Vec<ChunkSnapshotRow>,
    pub large_chunks: Vec<LargeChunkSnapshotRow>,
    pub communities: Vec<CommunitySnapshotRow>,
    pub community_members: Vec<CommunityMemberSnapshotRow>,
}

/// One `:Community` node plus its `[:HAS_MEMBER]` edges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphCommunitySnapshot {
    pub community_id: Uuid,
    pub level: u8,
    pub member_count: usize,
    /// `(member_pg_id, member_label)` where `member_label` is `"Chunk"` or `"Module"`.
    pub members: Vec<(Uuid, String)>,
}

/// Everything the ingestion pipeline wrote to Neo4j for one repo.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GraphRepoSnapshot {
    pub chunk_nodes: Vec<ChunkGraphNode>,
    /// `(pg_id, path)` pairs — matches `ModuleGraphRepo::get_module_nodes`'s
    /// existing shape (a `:Module` node has no properties beyond these two).
    pub module_nodes: Vec<(String, String)>,
    pub calls_edges: Vec<IngestEdge>,
    pub reference_edges: Vec<IngestEdge>,
    pub implements_edges: Vec<IngestEdge>,
    pub routes_to_edges: Vec<IngestEdge>,
    pub http_call_edges: Vec<IngestEdge>,
    /// `(chunk_pg_id, tag_name)` pairs — one row per `[:TAGGED_WITH]` edge a chunk has.
    /// A chunk_type-derived tag (at minimum) is written for every chunk during Stage 4.
    pub chunk_tags: Vec<(String, String)>,
    /// `(module_path, chunk_id)` — Module→Chunk import `REFERENCES {ref_kind:'import'}` edges.
    pub symbol_import_edges: Vec<(String, Uuid)>,
    /// `(src_module_pg_id, tgt_module_pg_id)` — Module `IMPORTS_FROM` edges.
    pub module_import_edges: Vec<(String, String)>,
    pub flows: Vec<FlowRecord>,
    pub communities: Vec<GraphCommunitySnapshot>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_repo_snapshot_round_trips_through_json() {
        let now = Utc::now();
        let snap = PgRepoSnapshot {
            modules: vec![ModuleSnapshotRow {
                id: Uuid::nil(),
                path: "src/main.rs".into(),
                language: Some("rust".into()),
                summary: Some("entry point".into()),
                exports_count: Some(1),
                file_count: Some(1),
                is_virtual: false,
                embedding: Some(vec![0.1, 0.2]),
                git_ref: Some("main".into()),
                ingested_at: Some(now),
            }],
            chunks: vec![ChunkSnapshotRow {
                id: Uuid::nil(),
                module_path: "src/main.rs".into(),
                chunk_type: "function".into(),
                name: "main".into(),
                signature: None,
                content: "fn main() {}".into(),
                language: Some("rust".into()),
                embedding: vec![0.1, 0.2],
                signature_embedding: None,
                git_ref: Some("main".into()),
                fqn: None,
                parent_fqn: None,
                start_line: Some(1),
                end_line: Some(1),
                visibility: None,
                is_async: false,
                is_static: false,
                is_exported: false,
                doc: None,
                ingested_at: Some(now),
            }],
            large_chunks: vec![LargeChunkSnapshotRow {
                id: Uuid::nil(),
                module_path: "src/main.rs".into(),
                chunk_ids: vec![Uuid::nil()],
                content: "fn main() {}".into(),
                embedding: vec![0.1, 0.2],
                git_ref: Some("main".into()),
                ingested_at: Some(now),
            }],
            communities: vec![CommunitySnapshotRow {
                id: Uuid::nil(),
                level: 0,
                name: Some("root".into()),
                summary: None,
                member_count: Some(1),
                embedding: None,
                parent_id: None,
                created_at: Some(now),
            }],
            ..Default::default()
        };

        let json = serde_json::to_string(&snap).expect("serialize");
        let back: PgRepoSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.modules.len(), 1);
        assert_eq!(back.modules[0].path, "src/main.rs");
        assert_eq!(back.modules[0].embedding, Some(vec![0.1, 0.2]));
        assert_eq!(back.modules[0].ingested_at, Some(now));
        assert_eq!(back.chunks.len(), 1);
        assert_eq!(back.chunks[0].ingested_at, Some(now));
        assert_eq!(back.large_chunks.len(), 1);
        assert_eq!(back.large_chunks[0].ingested_at, Some(now));
        assert_eq!(back.communities.len(), 1);
        assert_eq!(back.communities[0].created_at, Some(now));
    }

    #[test]
    fn graph_repo_snapshot_round_trips_through_json() {
        let snap = GraphRepoSnapshot {
            module_nodes: vec![("pg-id-1".into(), "src/main.rs".into())],
            chunk_tags: vec![("chunk-pg-id-1".into(), "function".into())],
            symbol_import_edges: vec![("src/main.rs".into(), Uuid::nil())],
            module_import_edges: vec![("pg-id-1".into(), "pg-id-2".into())],
            ..Default::default()
        };

        let json = serde_json::to_string(&snap).expect("serialize");
        let back: GraphRepoSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.module_nodes, snap.module_nodes);
        assert_eq!(back.chunk_tags, snap.chunk_tags);
        assert_eq!(back.symbol_import_edges, snap.symbol_import_edges);
    }
}
