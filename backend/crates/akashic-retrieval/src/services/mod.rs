//! Application-service implementations for `SearchService`, `NavigationService`,
//! and `GraphService`.
//!
//! All three traits are defined in `akashic-domain::ports::services` and
//! implemented here in `akashic-retrieval` — the crate that already owns the
//! GraphRAG orchestration (`GraphRagService`) and symbol-resolution free
//! functions.
//!
//! **A2a scope (T4):**
//! - `GraphService` methods → filled over `GraphReadRepo` + `ChunkRepo` /
//!   `ModuleRepo` / `DocClusterRepo` / `NoteRepo` / `NoteHealthRepo` /
//!   `SagaRepo`. Response types are serialized to JSON strings for the
//!   opaque-return methods (`get_doc_graph`, `get_document_detail`, etc.);
//!   structured types (`GraphView`, `GodNodesResult`, `ModuleGraphView`) use
//!   the domain port types.
//! - `SearchService` and `NavigationService` stubs remain as A1 (no change).
//!
//! `#[allow(dead_code)]` is removed from the graph methods because the
//! handlers now delegate to this service.

use std::sync::Arc;

use akashic_domain::{DomainError, DomainResult};
use async_trait::async_trait;
use uuid::Uuid;

use akashic_domain::algos::impact::{self as impact_algo, RawImpactEdge};
use akashic_domain::ports::services::{
    CommunitySummaryItem, GodNodeItem, GodNodesResult, GraphEdge, GraphNode, GraphService,
    GraphView, ModuleGraphNode, ModuleGraphView, NavigationService, SagaGroupItem, SearchService,
};
use akashic_domain::ports::{
    ChunkRepo, CommunityRepo, DocClusterRepo, DocumentRepo, EdgeRepo, GraphReadRepo,
    GraphTraversalRepo, GraphWriteRepo, LlmProvider, ModuleRepo, NoteHealthRepo, NoteRepo,
    SagaRepo, SymbolRepo,
};
use akashic_domain::types::{AffectedFlow, ImpactPathType, SuggestedTest};
use akashic_domain::types::{
    ChangeImpactReport, ChunkDetailRow, ChunkRow, GraphRagQuery, GraphRagResponse, ImpactReport,
    KnowledgeDocsRef, KnowledgeSearchItem, KnowledgeSearchRequest, ModuleRow, PreferSpace,
    ReferenceRow, RelinkResult, SymbolCandidate,
};
use akashic_embed::EmbeddingProvider;

use crate::graphrag;
use crate::graphrag::symbol_resolution as sym;

// ── RetrievalServices ─────────────────────────────────────────────────────────

/// Thin service struct that holds `Arc<dyn …Repo>` ports and delegates to
/// existing `GraphRagService` and symbol-resolution orchestration, plus the
/// new `GraphReadRepo` for D3-topology reads (A2a T4).
pub struct RetrievalServices {
    chunk_repo: Arc<dyn ChunkRepo>,
    doc_repo: Arc<dyn DocumentRepo>,
    note_repo: Arc<dyn NoteRepo>,
    note_health_repo: Arc<dyn NoteHealthRepo>,
    module_repo: Arc<dyn ModuleRepo>,
    traversal_repo: Arc<dyn GraphTraversalRepo>,
    symbol_repo: Arc<dyn SymbolRepo>,
    edge_repo: Arc<dyn EdgeRepo>,
    saga_repo: Arc<dyn SagaRepo>,
    doc_cluster_repo: Arc<dyn DocClusterRepo>,
    graph_read_repo: Arc<dyn GraphReadRepo>,
    graph_write_repo: Arc<dyn GraphWriteRepo>,
    community_repo: Arc<dyn CommunityRepo>,
    embedder: Arc<dyn EmbeddingProvider>,
    llm: Arc<dyn LlmProvider>,
}

impl RetrievalServices {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chunk_repo: Arc<dyn ChunkRepo>,
        doc_repo: Arc<dyn DocumentRepo>,
        note_repo: Arc<dyn NoteRepo>,
        note_health_repo: Arc<dyn NoteHealthRepo>,
        module_repo: Arc<dyn ModuleRepo>,
        traversal_repo: Arc<dyn GraphTraversalRepo>,
        symbol_repo: Arc<dyn SymbolRepo>,
        edge_repo: Arc<dyn EdgeRepo>,
        saga_repo: Arc<dyn SagaRepo>,
        doc_cluster_repo: Arc<dyn DocClusterRepo>,
        graph_read_repo: Arc<dyn GraphReadRepo>,
        graph_write_repo: Arc<dyn GraphWriteRepo>,
        community_repo: Arc<dyn CommunityRepo>,
        embedder: Arc<dyn EmbeddingProvider>,
        llm: Arc<dyn LlmProvider>,
    ) -> Self {
        Self {
            chunk_repo,
            doc_repo,
            note_repo,
            note_health_repo,
            module_repo,
            traversal_repo,
            symbol_repo,
            edge_repo,
            saga_repo,
            doc_cluster_repo,
            graph_read_repo,
            graph_write_repo,
            community_repo,
            embedder,
            llm,
        }
    }

    /// Build a `GraphRagService` from the stored repo ports.
    fn graphrag_service(&self) -> graphrag::GraphRagService {
        graphrag::GraphRagService::new(
            Arc::clone(&self.chunk_repo),
            Arc::clone(&self.doc_repo),
            Arc::clone(&self.note_repo),
            Arc::clone(&self.module_repo),
            Arc::clone(&self.traversal_repo),
            Arc::clone(&self.embedder) as Arc<dyn EmbeddingProvider>,
        )
    }
}

mod graph;
mod navigation;
mod search;
