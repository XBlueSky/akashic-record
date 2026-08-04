//! Neo4j adapter for [`GraphWriteRepo`] — global cross-repo edge writes.
//!
//! `create_http_calls_edges` is the C2 writer. Unlike the repo-scoped
//! `IngestEdgeRepo` writers (which delete-then-MERGE within one repo), this is
//! GLOBAL: it deletes EVERY `HTTP_CALLS` edge then MERGEs the fresh set, so the
//! whole cross-service link set is rebuilt idempotently in one pass.

use anyhow::{Context, Result};
use async_trait::async_trait;
use neo4rs::query;
use tracing::info;

use akashic_domain::algos::http_link::CrossServiceLink;
use akashic_domain::ports::graph_write::GraphWriteRepo;

use crate::Neo4jPool;

/// Neo4j adapter implementing [`GraphWriteRepo`].
#[derive(Clone)]
pub struct Neo4jGraphWriteRepo {
    graph: Neo4jPool,
}

impl Neo4jGraphWriteRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl GraphWriteRepo for Neo4jGraphWriteRepo {
    async fn create_http_calls_edges(&self, links: &[CrossServiceLink]) -> Result<()> {
        // GLOBAL delete — no repo filter (the HTTP_CALLS edge is cross-repo).
        self.graph
            .execute(query("MATCH (:Chunk)-[h:HTTP_CALLS]->() DELETE h"))
            .await
            .context("Failed to delete existing HTTP_CALLS edges")?;

        if links.is_empty() {
            info!("HTTP_CALLS: delete-only (0 links)");
            return Ok(());
        }

        let mut callers: Vec<String> = Vec::with_capacity(links.len());
        let mut handlers: Vec<String> = Vec::with_capacity(links.len());
        let mut http_methods: Vec<String> = Vec::with_capacity(links.len());
        let mut http_paths: Vec<String> = Vec::with_capacity(links.len());
        let mut matched_vias: Vec<String> = Vec::with_capacity(links.len());
        let mut cross_repos: Vec<bool> = Vec::with_capacity(links.len());
        for l in links {
            callers.push(l.caller_pg_id.clone());
            handlers.push(l.handler_pg_id.clone());
            http_methods.push(l.http_method.clone());
            http_paths.push(l.http_path.clone());
            matched_vias.push(l.matched_via.clone());
            cross_repos.push(l.cross_repo);
        }

        self.graph
            .execute(
                query(
                    "UNWIND range(0, size($callers)-1) AS i \
                     MATCH (src:Chunk {pg_id: $callers[i]}) \
                     MATCH (tgt:Chunk {pg_id: $handlers[i]}) \
                     MERGE (src)-[r:HTTP_CALLS {http_method: $http_methods[i], http_path: $http_paths[i]}]->(tgt) \
                     SET r.matched_via = $matched_vias[i], r.cross_repo = $cross_repos[i]",
                )
                .param("callers", callers)
                .param("handlers", handlers)
                .param("http_methods", http_methods)
                .param("http_paths", http_paths)
                .param("matched_vias", matched_vias)
                .param("cross_repos", cross_repos),
            )
            .await
            .context("Failed to create HTTP_CALLS edges")?;

        info!(
            count = links.len(),
            "Created HTTP_CALLS edges (global rebuild)"
        );
        Ok(())
    }
}
