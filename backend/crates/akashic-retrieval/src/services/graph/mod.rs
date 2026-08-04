use super::*;

// ── GraphService impl ─────────────────────────────────────────────────────────
//
// Twelve methods orchestrating GraphReadRepo (Neo4j topology) + PG repos. The
// method bodies were carved (verbatim) into per-topic submodules as inherent
// `gs_*` methods on `RetrievalServices`; this trait impl is a thin delegating
// shim (a single trait impl can't be split across files, so the bodies live in
// `view`/`modules`/`docs`/`summary` and are reached via `self.gs_*`).

mod analytics;
mod docs;
mod modules;
mod summary;
mod view;

#[async_trait]
impl GraphService for RetrievalServices {
    async fn get_graph(&self, repo: String) -> DomainResult<GraphView> {
        self.gs_get_graph(repo).await
    }

    async fn get_god_nodes(&self, repo: String) -> DomainResult<GodNodesResult> {
        self.gs_get_god_nodes(repo).await
    }

    async fn get_module_graph(&self, repo: String) -> DomainResult<ModuleGraphView> {
        self.gs_get_module_graph(repo).await
    }

    async fn list_modules(
        &self,
        repo: String,
        limit: i64,
        offset: i64,
    ) -> DomainResult<(i64, Vec<ModuleRow>)> {
        self.gs_list_modules(repo, limit, offset).await
    }

    async fn list_module_chunks(
        &self,
        repo: String,
        module_path: String,
        limit: i64,
        offset: i64,
    ) -> DomainResult<Vec<ChunkRow>> {
        self.gs_list_module_chunks(repo, module_path, limit, offset)
            .await
    }

    async fn get_chunk_detail(
        &self,
        repo: String,
        chunk_id: Uuid,
    ) -> DomainResult<Option<ChunkDetailRow>> {
        self.gs_get_chunk_detail(repo, chunk_id).await
    }

    async fn get_module_detail(&self, module_id: Uuid) -> DomainResult<Option<String>> {
        self.gs_get_module_detail(module_id).await
    }

    async fn get_module_call_graph(&self, module_id: Uuid) -> DomainResult<String> {
        self.gs_get_module_call_graph(module_id).await
    }

    async fn get_doc_graph(&self, repo: String) -> DomainResult<String> {
        self.gs_get_doc_graph(repo).await
    }

    async fn get_document_detail(&self, doc_id: Uuid) -> DomainResult<String> {
        self.gs_get_document_detail(doc_id).await
    }

    async fn get_cluster_detail(&self, cluster_id: Uuid) -> DomainResult<String> {
        self.gs_get_cluster_detail(cluster_id).await
    }

    async fn find_corpus_document_id(
        &self,
        repo: String,
        path: String,
    ) -> DomainResult<Option<Uuid>> {
        self.gs_find_corpus_document_id(repo, path).await
    }

    async fn get_project_summary(
        &self,
        repo: String,
        branch: Option<String>,
    ) -> DomainResult<String> {
        self.gs_get_project_summary(repo, branch).await
    }

    async fn detect_code_communities(
        &self,
        repo: String,
        min_confidence: f64,
    ) -> DomainResult<Vec<akashic_domain::algos::code_community::Community>> {
        self.gs_detect_code_communities(repo, min_confidence).await
    }

    async fn detect_dead_code(
        &self,
        repo: String,
    ) -> DomainResult<akashic_domain::algos::dead_code::DeadCodeReport> {
        self.gs_detect_dead_code(repo).await
    }

    async fn trace_decision_history(
        &self,
        repo: String,
        symbol: String,
    ) -> DomainResult<Vec<akashic_domain::algos::decision_lineage::DecisionTimelineEntry>> {
        self.gs_trace_decision_history(repo, symbol).await
    }

    async fn get_decision_lineage(
        &self,
        note_id: Uuid,
    ) -> DomainResult<akashic_domain::algos::decision_lineage::DecisionLineageReport> {
        self.gs_get_decision_lineage(note_id).await
    }

    async fn link_cross_service_calls(
        &self,
    ) -> DomainResult<akashic_domain::algos::http_link::CrossServiceLinkReport> {
        self.gs_link_cross_service_calls().await
    }
}
