//! Neo4j adapters for `RepoGraphRepo`, `IngestEdgeRepo`, and `FlowGraphRepo`.
//!
//! All Cypher is moved verbatim from the original call-sites in:
//! - `akashic-ingestion::ingestion::pipeline::run_inner` (MERGE Repository)
//! - `akashic-ingestion::ingestion::store` (CALLS, REFERENCES, IMPLEMENTS,
//!   ROUTES_TO, symbol-import REFERENCES, IMPORTS_FROM)

use anyhow::{Context, Result};
use async_trait::async_trait;
use neo4rs::query;
use tracing::info;
use uuid::Uuid;

use akashic_domain::ports::{FlowGraphRepo, IngestEdgeRepo, RepoGraphRepo};
use akashic_domain::types::{FlowRecord, IngestEdge};

use crate::Neo4jPool;

// ── Neo4jRepoGraphRepo ────────────────────────────────────────────────────────

/// Neo4j adapter implementing [`RepoGraphRepo`].
#[derive(Clone)]
pub struct Neo4jRepoGraphRepo {
    graph: Neo4jPool,
}

impl Neo4jRepoGraphRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl RepoGraphRepo for Neo4jRepoGraphRepo {
    async fn ensure_repository_node(&self, repo_name: &str) -> Result<()> {
        // Cypher moved verbatim from akashic-ingestion::ingestion::pipeline::run_inner Stage 3.
        self.graph
            .execute(query("MERGE (r:Repository {name: $name})").param("name", repo_name))
            .await?;
        Ok(())
    }

    async fn delete_repo_graph(&self, repo_name: &str) -> Result<()> {
        // Cypher moved verbatim from
        // akashic-server::api::routes::repos::delete_repo_data (A2a Task 8).
        self.graph
            .execute(
                query(
                    "MATCH (r:Repository {name: $name}) \
                     OPTIONAL MATCH (r)-[*]->(n) \
                     DETACH DELETE r, n",
                )
                .param("name", repo_name.to_string()),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Neo4j delete_repo_graph failed: {e}"))?;
        Ok(())
    }

    async fn sync_branch(
        &self,
        repo_name: &str,
        branch_name: &str,
        commit_hash: &str,
    ) -> Result<()> {
        // Cypher moved verbatim from akashic-server::gitlab::webhook::sync_branch.
        self.graph
            .execute(
                query(
                    "MERGE (r:Repository {name: $repo_name})
                     ON CREATE SET r.last_synced_at = datetime()
                     SET r.last_synced_at = datetime()
                     MERGE (r)-[:HAS_BRANCH]->(b:Branch {name: $branch_name})
                     ON CREATE SET b.last_commit_hash = $commit_hash
                     SET b.last_commit_hash = $commit_hash",
                )
                .param("repo_name", repo_name)
                .param("branch_name", branch_name)
                .param("commit_hash", commit_hash),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Neo4j sync_branch failed: {e}"))?;
        Ok(())
    }

    async fn sync_tag(&self, repo_name: &str, tag_name: &str, commit_hash: &str) -> Result<()> {
        // Cypher moved verbatim from akashic-server::gitlab::webhook::sync_tag.
        self.graph
            .execute(
                query(
                    "MERGE (r:Repository {name: $repo_name})
                     ON CREATE SET r.last_synced_at = datetime()
                     SET r.last_synced_at = datetime()
                     MERGE (r)-[:HAS_TAG]->(t:Tag {name: $tag_name})
                     ON CREATE SET t.commit_hash = $commit_hash
                     SET t.commit_hash = $commit_hash",
                )
                .param("repo_name", repo_name)
                .param("tag_name", tag_name)
                .param("commit_hash", commit_hash),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Neo4j sync_tag failed: {e}"))?;
        Ok(())
    }
}

// ── Neo4jIngestEdgeRepo ───────────────────────────────────────────────────────

/// Neo4j adapter implementing [`IngestEdgeRepo`].
#[derive(Clone)]
pub struct Neo4jIngestEdgeRepo {
    graph: Neo4jPool,
}

impl Neo4jIngestEdgeRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl IngestEdgeRepo for Neo4jIngestEdgeRepo {
    async fn create_call_edges(&self, repo_name: &str, calls: &[IngestEdge]) -> Result<()> {
        // Cypher moved verbatim from akashic-ingestion::ingestion::store::create_call_edges.
        self.graph
            .execute(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(:Module)-[:HAS_CHUNK]->(c:Chunk) \
                     MATCH (c)-[call:CALLS]->() \
                     DELETE call",
                )
                .param("repo_name", repo_name),
            )
            .await
            .context("Failed to delete existing CALLS edges")?;

        if calls.is_empty() {
            return Ok(());
        }

        let mut src_ids: Vec<String> = Vec::with_capacity(calls.len());
        let mut tgt_ids: Vec<String> = Vec::with_capacity(calls.len());
        let mut confidences: Vec<f64> = Vec::with_capacity(calls.len());
        let mut methods: Vec<String> = Vec::with_capacity(calls.len());
        for call in calls {
            src_ids.push(call.src_chunk_id.to_string());
            tgt_ids.push(call.tgt_chunk_id.to_string());
            confidences.push(call.confidence as f64);
            methods.push(call.method.clone());
        }

        self.graph
            .execute(
                query(
                    "UNWIND range(0, size($src_ids)-1) AS i \
                     MATCH (src:Chunk {pg_id: $src_ids[i]}) \
                     MATCH (tgt:Chunk {pg_id: $tgt_ids[i]}) \
                     MERGE (src)-[r:CALLS]->(tgt) \
                     SET r.confidence = $confidences[i], r.method = $methods[i]",
                )
                .param("src_ids", src_ids)
                .param("tgt_ids", tgt_ids)
                .param("confidences", confidences)
                .param("methods", methods),
            )
            .await
            .context("Failed to batch-create CALLS edges in Neo4j")?;

        info!(repo_name, count = calls.len(), "Created CALLS edges");
        Ok(())
    }

    async fn create_reference_edges(&self, repo_name: &str, refs: &[IngestEdge]) -> Result<()> {
        // Cypher moved verbatim from akashic-ingestion::ingestion::store::create_reference_edges.
        self.graph
            .execute(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(:Module)-[:HAS_CHUNK]->(c:Chunk) \
                     MATCH (c)-[ref:REFERENCES]->() \
                     DELETE ref",
                )
                .param("repo_name", repo_name),
            )
            .await
            .context("Failed to delete existing REFERENCES edges")?;

        if refs.is_empty() {
            return Ok(());
        }

        let mut src_ids: Vec<String> = Vec::with_capacity(refs.len());
        let mut tgt_ids: Vec<String> = Vec::with_capacity(refs.len());
        let mut confidences: Vec<f64> = Vec::with_capacity(refs.len());
        let mut methods: Vec<String> = Vec::with_capacity(refs.len());
        let mut ref_kinds: Vec<String> = Vec::with_capacity(refs.len());
        for re in refs {
            src_ids.push(re.src_chunk_id.to_string());
            tgt_ids.push(re.tgt_chunk_id.to_string());
            confidences.push(re.confidence as f64);
            methods.push(re.method.clone());
            ref_kinds.push(re.ref_kind.clone().unwrap_or_else(|| "type".to_string()));
        }

        self.graph
            .execute(
                query(
                    "UNWIND range(0, size($src_ids)-1) AS i \
                     MATCH (src:Chunk {pg_id: $src_ids[i]}) \
                     MATCH (tgt:Chunk {pg_id: $tgt_ids[i]}) \
                     MERGE (src)-[r:REFERENCES {ref_kind: $ref_kinds[i]}]->(tgt) \
                     SET r.confidence = $confidences[i], r.method = $methods[i]",
                )
                .param("src_ids", src_ids)
                .param("tgt_ids", tgt_ids)
                .param("confidences", confidences)
                .param("methods", methods)
                .param("ref_kinds", ref_kinds),
            )
            .await
            .context("Failed to create REFERENCES edges")?;

        info!(repo_name, count = refs.len(), "Created REFERENCES edges");
        Ok(())
    }

    async fn create_implements_edges(&self, repo_name: &str, impls: &[IngestEdge]) -> Result<()> {
        // Cypher moved verbatim from akashic-ingestion::ingestion::store::create_implements_edges.
        self.graph
            .execute(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(:Module)-[:HAS_CHUNK]->(c:Chunk) \
                     MATCH (c)-[ref:IMPLEMENTS]->() \
                     DELETE ref",
                )
                .param("repo_name", repo_name),
            )
            .await
            .context("Failed to delete existing IMPLEMENTS edges")?;

        if impls.is_empty() {
            return Ok(());
        }

        let mut src_ids: Vec<String> = Vec::with_capacity(impls.len());
        let mut tgt_ids: Vec<String> = Vec::with_capacity(impls.len());
        let mut confidences: Vec<f64> = Vec::with_capacity(impls.len());
        let mut methods: Vec<String> = Vec::with_capacity(impls.len());
        let mut impl_kinds: Vec<String> = Vec::with_capacity(impls.len());
        for re in impls {
            src_ids.push(re.src_chunk_id.to_string());
            tgt_ids.push(re.tgt_chunk_id.to_string());
            confidences.push(re.confidence as f64);
            methods.push(re.method.clone());
            impl_kinds.push(
                re.ref_kind
                    .clone()
                    .unwrap_or_else(|| "implements".to_string()),
            );
        }

        self.graph
            .execute(
                query(
                    "UNWIND range(0, size($src_ids)-1) AS i \
                     MATCH (src:Chunk {pg_id: $src_ids[i]}) \
                     MATCH (tgt:Chunk {pg_id: $tgt_ids[i]}) \
                     MERGE (src)-[r:IMPLEMENTS {impl_kind: $impl_kinds[i]}]->(tgt) \
                     SET r.confidence = $confidences[i], r.method = $methods[i]",
                )
                .param("src_ids", src_ids)
                .param("tgt_ids", tgt_ids)
                .param("confidences", confidences)
                .param("methods", methods)
                .param("impl_kinds", impl_kinds),
            )
            .await
            .context("Failed to create IMPLEMENTS edges")?;

        info!(repo_name, count = impls.len(), "Created IMPLEMENTS edges");
        Ok(())
    }

    async fn create_routes_to_edges(&self, repo_name: &str, routes: &[IngestEdge]) -> Result<()> {
        // Cypher moved verbatim from akashic-ingestion::ingestion::store::create_routes_to_edges.
        self.graph
            .execute(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(:Module)-[:HAS_CHUNK]->(c:Chunk) \
                     MATCH (c)-[ref:ROUTES_TO]->() \
                     DELETE ref",
                )
                .param("repo_name", repo_name),
            )
            .await
            .context("Failed to delete existing ROUTES_TO edges")?;

        if routes.is_empty() {
            return Ok(());
        }

        let mut src_ids: Vec<String> = Vec::with_capacity(routes.len());
        let mut tgt_ids: Vec<String> = Vec::with_capacity(routes.len());
        let mut confidences: Vec<f64> = Vec::with_capacity(routes.len());
        let mut methods: Vec<String> = Vec::with_capacity(routes.len());
        let mut http_methods: Vec<String> = Vec::with_capacity(routes.len());
        for re in routes {
            src_ids.push(re.src_chunk_id.to_string());
            tgt_ids.push(re.tgt_chunk_id.to_string());
            confidences.push(re.confidence as f64);
            methods.push(re.method.clone());
            http_methods.push(re.ref_kind.clone().unwrap_or_else(|| "ANY".to_string()));
        }

        self.graph
            .execute(
                query(
                    "UNWIND range(0, size($src_ids)-1) AS i \
                     MATCH (src:Chunk {pg_id: $src_ids[i]}) \
                     MATCH (tgt:Chunk {pg_id: $tgt_ids[i]}) \
                     MERGE (src)-[r:ROUTES_TO {http_method: $http_methods[i]}]->(tgt) \
                     SET r.confidence = $confidences[i], r.method = $methods[i]",
                )
                .param("src_ids", src_ids)
                .param("tgt_ids", tgt_ids)
                .param("confidences", confidences)
                .param("methods", methods)
                .param("http_methods", http_methods),
            )
            .await
            .context("Failed to create ROUTES_TO edges")?;

        info!(repo_name, count = routes.len(), "Created ROUTES_TO edges");
        Ok(())
    }

    async fn create_makes_http_call_edges(
        &self,
        repo_name: &str,
        calls: &[IngestEdge],
    ) -> Result<()> {
        self.graph
            .execute(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(:Module)-[:HAS_CHUNK]->(c:Chunk) \
                     MATCH (c)-[ref:MAKES_HTTP_CALL]->() \
                     DELETE ref",
                )
                .param("repo_name", repo_name),
            )
            .await
            .context("Failed to delete existing MAKES_HTTP_CALL edges")?;

        if calls.is_empty() {
            return Ok(());
        }

        let mut src_ids: Vec<String> = Vec::with_capacity(calls.len());
        let mut tgt_ids: Vec<String> = Vec::with_capacity(calls.len());
        let mut confidences: Vec<f64> = Vec::with_capacity(calls.len());
        let mut methods: Vec<String> = Vec::with_capacity(calls.len());
        let mut http_methods: Vec<String> = Vec::with_capacity(calls.len());
        for re in calls {
            src_ids.push(re.src_chunk_id.to_string());
            tgt_ids.push(re.tgt_chunk_id.to_string());
            confidences.push(re.confidence as f64);
            methods.push(re.method.clone());
            http_methods.push(re.ref_kind.clone().unwrap_or_else(|| "ANY".to_string()));
        }

        self.graph
            .execute(
                query(
                    "UNWIND range(0, size($src_ids)-1) AS i \
                     MATCH (src:Chunk {pg_id: $src_ids[i]}) \
                     MATCH (tgt:Chunk {pg_id: $tgt_ids[i]}) \
                     MERGE (src)-[r:MAKES_HTTP_CALL {http_method: $http_methods[i]}]->(tgt) \
                     SET r.confidence = $confidences[i], r.method = $methods[i]",
                )
                .param("src_ids", src_ids)
                .param("tgt_ids", tgt_ids)
                .param("confidences", confidences)
                .param("methods", methods)
                .param("http_methods", http_methods),
            )
            .await
            .context("Failed to create MAKES_HTTP_CALL edges")?;

        info!(
            repo_name,
            count = calls.len(),
            "Created MAKES_HTTP_CALL edges"
        );
        Ok(())
    }

    async fn create_symbol_import_edges(
        &self,
        repo_name: &str,
        imports: &[(String, Uuid)],
    ) -> Result<()> {
        // Cypher moved verbatim from akashic-ingestion::ingestion::store::create_symbol_import_edges.
        self.graph
            .execute(
                query(
                    "MATCH (m:Module {repo_name: $repo_name})-[ref:REFERENCES {ref_kind: 'import'}]->(:Chunk) \
                     DELETE ref",
                )
                .param("repo_name", repo_name),
            )
            .await
            .context("Failed to delete existing import REFERENCES edges")?;

        if imports.is_empty() {
            return Ok(());
        }

        let mut module_paths: Vec<String> = Vec::with_capacity(imports.len());
        let mut symbol_ids: Vec<String> = Vec::with_capacity(imports.len());
        for (mp, id) in imports {
            module_paths.push(mp.clone());
            symbol_ids.push(id.to_string());
        }

        self.graph
            .execute(
                query(
                    "UNWIND range(0, size($module_paths)-1) AS i \
                     MATCH (m:Module {repo_name: $repo_name, path: $module_paths[i]}) \
                     MATCH (s:Chunk {pg_id: $symbol_ids[i]}) \
                     MERGE (m)-[r:REFERENCES {ref_kind: 'import'}]->(s)",
                )
                .param("repo_name", repo_name)
                .param("module_paths", module_paths)
                .param("symbol_ids", symbol_ids),
            )
            .await
            .context("Failed to create import REFERENCES edges")?;

        info!(
            repo_name,
            count = imports.len(),
            "Created import REFERENCES edges"
        );
        Ok(())
    }

    async fn create_import_edges(&self, src_ids: &[String], tgt_ids: &[String]) -> Result<()> {
        // Cypher moved verbatim from the Neo4j half of
        // akashic-ingestion::ingestion::store::create_import_edges.
        if src_ids.is_empty() {
            return Ok(());
        }

        self.graph
            .execute(
                query(
                    "UNWIND range(0, size($src_ids)-1) AS i \
                     MATCH (src:Module {pg_id: $src_ids[i]}) \
                     MATCH (tgt:Module {pg_id: $tgt_ids[i]}) \
                     MERGE (src)-[:IMPORTS_FROM]->(tgt)",
                )
                .param("src_ids", src_ids.to_vec())
                .param("tgt_ids", tgt_ids.to_vec()),
            )
            .await
            .context("Failed to batch-create IMPORTS_FROM edges in Neo4j")?;

        Ok(())
    }
}

// ── Neo4jFlowGraphRepo ───────────────────────────────────────────────────────

/// Neo4j adapter implementing [`FlowGraphRepo`].
///
/// Cypher moved verbatim from
/// `akashic-ingestion::ingestion::flows::store_flows` (A1 Task 8).
#[derive(Clone)]
pub struct Neo4jFlowGraphRepo {
    graph: Neo4jPool,
}

impl Neo4jFlowGraphRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl FlowGraphRepo for Neo4jFlowGraphRepo {
    async fn store_flows(&self, repo_name: &str, flows: &[FlowRecord]) -> anyhow::Result<()> {
        // Delete old Flow nodes for this repo.
        self.graph
            .execute(
                query(
                    "MATCH (f:Flow {repo_name: $repo}) \
                     DETACH DELETE f",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to delete old Flow nodes")?;

        for flow in flows {
            self.graph
                .execute(
                    query(
                        "CREATE (f:Flow { \
                           pg_id: $id, \
                           name: $name, \
                           entry_type: $entry_type, \
                           repo_name: $repo, \
                           step_count: $step_count, \
                           truncated: $truncated \
                         })",
                    )
                    .param("id", flow.flow_id.as_str())
                    .param("name", flow.display_name.as_str())
                    .param("entry_type", flow.entry_type.as_str())
                    .param("repo", flow.repo_name.as_str())
                    .param("step_count", flow.step_count as i64)
                    .param("truncated", flow.truncated),
                )
                .await
                .context("Failed to create Flow node")?;

            self.graph
                .execute(
                    query(
                        "MATCH (c:Chunk {pg_id: $chunk_id}) \
                         MATCH (f:Flow {pg_id: $flow_id}) \
                         CREATE (c)-[:IS_ENTRY_POINT {entry_type: $entry_type}]->(f)",
                    )
                    .param("chunk_id", flow.entry_chunk_id.to_string().as_str())
                    .param("flow_id", flow.flow_id.as_str())
                    .param("entry_type", flow.entry_type.as_str()),
                )
                .await
                .context("Failed to create IS_ENTRY_POINT edge")?;

            for step in &flow.steps {
                self.graph
                    .execute(
                        query(
                            "MATCH (f:Flow {pg_id: $flow_id}) \
                             MATCH (c:Chunk {pg_id: $chunk_id}) \
                             CREATE (f)-[:FLOW_STEP {position: $pos, depth: $depth}]->(c)",
                        )
                        .param("flow_id", flow.flow_id.as_str())
                        .param("chunk_id", step.chunk_id.to_string().as_str())
                        .param("pos", step.position as i64)
                        .param("depth", step.depth as i64),
                    )
                    .await
                    .context("Failed to create FLOW_STEP edge")?;
            }
        }

        info!(
            repo = repo_name,
            flows = flows.len(),
            "Stored execution flows in Neo4j"
        );
        Ok(())
    }
}
