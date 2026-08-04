//! Neo4j adapter for [`GraphReadRepo`].
//!
//! All Cypher is moved VERBATIM from the original handlers in
//! `akashic-server::api::routes::graph::{core, module, doc}`.
//!
//! **Preserved invariants:**
//! - Finding #38/#39: `get_import_edges` / `get_module_import_edges` propagate
//!   Neo4j errors via `?` — NOT `unwrap_or_default()`.
//! - N+1 fix: `batch_section_explains` uses `UNWIND $section_ids` (one
//!   round-trip for all sections, not one per section).
//! - N+1 fix: saga-status batching is LEFT in the service layer (the repo
//!   returns raw saga group rows; the service batch-fetches statuses from PG).

use anyhow::Result;
use async_trait::async_trait;
use neo4rs::query;
use uuid::Uuid;

use akashic_domain::ports::graph_read::{
    BranchRow, CallEdgeRow, ChunkGodNodeRow, ChunkRefRow, ClusterExplainsRow, CommunityEdgeRow,
    DeadCodeCandidateRow, DecisionAttachmentRow, DocExplainsRow, GhostNodeRow, GraphChunkRow,
    GraphDocRow, GraphModuleRow, GraphNoteRow, GraphReadRepo, HttpCallSiteRow, ImportEdgeRow,
    ModuleGodNodeRow, ModuleNodeRow, NoteCountRow, RouteSiteRow, SagaGroupRow, SectionExplainsRow,
    SupersedeChainRow,
};

use crate::Neo4jPool;

/// Neo4j adapter implementing [`GraphReadRepo`].
#[derive(Clone)]
pub struct Neo4jGraphReadRepo {
    graph: Neo4jPool,
}

impl Neo4jGraphReadRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

/// Map Neo4j rows with `(source, target, confidence, method)` columns into
/// `CallEdgeRow`, defaulting missing confidence to 0.0 and method to "unknown".
/// Shared by get_intra_module_calls / get_caller_edges / get_callee_edges.
fn map_call_edge_rows(rows: &[neo4rs::Row]) -> Vec<CallEdgeRow> {
    rows.iter()
        .filter_map(
            |r| match (r.get::<String>("source"), r.get::<String>("target")) {
                (Ok(source), Ok(target)) => Some(CallEdgeRow {
                    source,
                    target,
                    confidence: r.get::<f64>("confidence").unwrap_or(0.0),
                    method: r
                        .get::<String>("method")
                        .unwrap_or_else(|_| "unknown".into()),
                }),
                _ => None,
            },
        )
        .collect()
}

#[async_trait]
impl GraphReadRepo for Neo4jGraphReadRepo {
    // ── get_graph / get_branches ─────────────────────────────────────────────

    async fn get_branches(&self, repo: &str) -> Result<Vec<BranchRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_BRANCH]->(b:Branch) \
                     RETURN b.name AS name",
                )
                .param("repo_name", repo),
            )
            .await?;

        Ok(rows
            .iter()
            .filter_map(|r| r.get::<String>("name").ok().map(|name| BranchRow { name }))
            .collect())
    }

    async fn get_graph_notes(&self, repo: &str) -> Result<Vec<GraphNoteRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (c:Note)-[:BELONGS_TO]->(r:Repository {name: $repo_name}) \
                     OPTIONAL MATCH (c)-[:LINKED_TO]->(b:Branch) \
                     OPTIONAL MATCH (c)-[:TAGGED_AS]->(cat:Category) \
                     RETURN c.pg_id AS pg_id, coalesce(c.uuid, c.pg_id) AS uuid, \
                            b.name AS branch, cat.name AS category",
                )
                .param("repo_name", repo),
            )
            .await?;

        Ok(rows
            .iter()
            .filter_map(|r| {
                r.get::<String>("uuid").ok().map(|uuid| GraphNoteRow {
                    uuid,
                    branch: r.get::<String>("branch").ok(),
                    category: r.get::<String>("category").ok(),
                })
            })
            .collect())
    }

    async fn get_graph_modules(&self, repo: &str) -> Result<Vec<GraphModuleRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(m:Module) \
                     RETURN m.pg_id AS pg_id, m.path AS path",
                )
                .param("repo_name", repo),
            )
            .await?;

        Ok(rows
            .iter()
            .filter_map(
                |r| match (r.get::<String>("pg_id"), r.get::<String>("path")) {
                    (Ok(pg_id), Ok(path)) => Some(GraphModuleRow { pg_id, path }),
                    _ => None,
                },
            )
            .collect())
    }

    async fn get_graph_chunks(&self, repo: &str) -> Result<Vec<GraphChunkRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(m:Module)-[:HAS_CHUNK]->(c:Chunk) \
                     RETURN c.pg_id AS pg_id, c.name AS name, m.pg_id AS module_id \
                     LIMIT 1000",
                )
                .param("repo_name", repo),
            )
            .await?;

        Ok(rows
            .iter()
            .filter_map(|r| {
                match (
                    r.get::<String>("pg_id"),
                    r.get::<String>("name"),
                    r.get::<String>("module_id"),
                ) {
                    (Ok(pg_id), Ok(name), Ok(module_id)) => Some(GraphChunkRow {
                        pg_id,
                        name,
                        module_id,
                    }),
                    _ => None,
                }
            })
            .collect())
    }

    /// Finding #38/#39: propagates Neo4j errors via `?` — NOT `unwrap_or_default()`.
    async fn get_import_edges(&self, repo: &str) -> Result<Vec<ImportEdgeRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(src:Module)-[:IMPORTS_FROM]->(tgt:Module) \
                     RETURN src.pg_id AS src_id, tgt.pg_id AS tgt_id",
                )
                .param("repo_name", repo),
            )
            .await?; // ← `?` — preserves finding #38/#39

        Ok(rows
            .iter()
            .filter_map(
                |r| match (r.get::<String>("src_id"), r.get::<String>("tgt_id")) {
                    (Ok(src_id), Ok(tgt_id)) => Some(ImportEdgeRow { src_id, tgt_id }),
                    _ => None,
                },
            )
            .collect())
    }

    // ── get_module_graph ─────────────────────────────────────────────────────

    async fn get_module_nodes(&self, repo: &str) -> Result<Vec<ModuleNodeRow>> {
        // Identical to get_graph_modules (same Cypher, same row type via the
        // ModuleNodeRow = GraphModuleRow alias) — delegate rather than duplicate.
        self.get_graph_modules(repo).await
    }

    async fn get_module_note_counts(&self, repo: &str) -> Result<Vec<NoteCountRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(m:Module) \
                     OPTIONAL MATCH (c:Note)-[:ATTACHED_TO]->(m) \
                     RETURN m.pg_id AS pg_id, count(c) AS note_count",
                )
                .param("repo_name", repo),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(
                |r| match (r.get::<String>("pg_id"), r.get::<i64>("note_count")) {
                    (Ok(pg_id), Ok(note_count)) => Some(NoteCountRow { pg_id, note_count }),
                    _ => None,
                },
            )
            .collect())
    }

    /// Finding #38/#39: propagates Neo4j errors via `?`. Identical to
    /// get_import_edges (same Cypher, same row type); it also propagates, and
    /// the sole caller (gs_get_module_graph) already fails hard on the sibling
    /// get_module_nodes error — delegate so the two stay in lockstep. (The body
    /// here previously used unwrap_or_default, silently rendering an empty
    /// module graph on a Neo4j failure while its own doc claimed propagation.)
    async fn get_module_import_edges(&self, repo: &str) -> Result<Vec<ImportEdgeRow>> {
        self.get_import_edges(repo).await
    }

    async fn get_calls_count(&self, repo: &str) -> Result<i64> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(m:Module)-[:HAS_CHUNK]->(src:Chunk)-[:CALLS]->(tgt:Chunk) \
                     RETURN count(*) AS calls_count",
                )
                .param("repo_name", repo),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .first()
            .and_then(|r| r.get::<i64>("calls_count").ok())
            .unwrap_or(0))
    }

    async fn get_saga_group_rows(&self, repo: &str) -> Result<Vec<SagaGroupRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(m:Module) \
                     MATCH (n:Note)-[:ATTACHED_TO]->(m) \
                     MATCH (n)-[:PART_OF]->(s:Saga) \
                     RETURN s.pg_id AS saga_id, s.name AS saga_name, collect(DISTINCT m.pg_id) AS module_ids",
                )
                .param("repo_name", repo),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(|r| {
                match (
                    r.get::<String>("saga_id"),
                    r.get::<String>("saga_name"),
                    r.get::<Vec<String>>("module_ids"),
                ) {
                    (Ok(saga_id), Ok(saga_name), Ok(module_ids)) => Some(SagaGroupRow {
                        saga_id,
                        saga_name,
                        module_ids,
                    }),
                    _ => None,
                }
            })
            .collect())
    }

    // ── get_god_nodes ────────────────────────────────────────────────────────

    async fn get_module_god_nodes(&self, repo: &str) -> Result<Vec<ModuleGodNodeRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(m:Module) \
                     OPTIONAL MATCH (m)-[out:IMPORTS_FROM]->() \
                     OPTIONAL MATCH ()-[inc:IMPORTS_FROM]->(m) \
                     OPTIONAL MATCH (m)-[:HAS_CHUNK]->(c:Chunk) \
                     WITH m, count(DISTINCT out) AS out_deg, count(DISTINCT inc) AS in_deg, count(DISTINCT c) AS chunk_count \
                     WITH m, out_deg + in_deg AS import_deg, chunk_count \
                     WHERE import_deg > 0 \
                     RETURN m.pg_id AS pg_id, m.path AS path, import_deg, chunk_count, \
                            import_deg + chunk_count AS total_deg \
                     ORDER BY total_deg DESC \
                     LIMIT 10",
                )
                .param("repo_name", repo),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(
                |r| match (r.get::<String>("pg_id"), r.get::<String>("path")) {
                    (Ok(pg_id), Ok(path)) => Some(ModuleGodNodeRow {
                        pg_id,
                        path,
                        import_deg: r.get::<i64>("import_deg").unwrap_or(0),
                        chunk_count: r.get::<i64>("chunk_count").unwrap_or(0),
                        total_deg: r.get::<i64>("total_deg").unwrap_or(0),
                    }),
                    _ => None,
                },
            )
            .collect())
    }

    async fn get_chunk_god_nodes(&self, repo: &str) -> Result<Vec<ChunkGodNodeRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(m:Module)-[:HAS_CHUNK]->(c:Chunk) \
                     OPTIONAL MATCH (caller:Chunk)-[call_in:CALLS]->(c) \
                     OPTIONAL MATCH (c)-[call_out:CALLS]->(callee:Chunk) \
                     WITH c, m, count(DISTINCT call_in) AS in_calls, count(DISTINCT call_out) AS out_calls \
                     WITH c, m, in_calls + out_calls AS total_calls, in_calls, out_calls \
                     WHERE total_calls >= 3 \
                     RETURN c.pg_id AS pg_id, c.name AS name, m.path AS path, \
                            total_calls, in_calls, out_calls \
                     ORDER BY total_calls DESC \
                     LIMIT 10",
                )
                .param("repo_name", repo),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(|r| {
                match (
                    r.get::<String>("pg_id"),
                    r.get::<String>("name"),
                    r.get::<String>("path"),
                ) {
                    (Ok(pg_id), Ok(name), Ok(path)) => Some(ChunkGodNodeRow {
                        pg_id,
                        name,
                        path,
                        total_calls: r.get::<i64>("total_calls").unwrap_or(0),
                        in_calls: r.get::<i64>("in_calls").unwrap_or(0),
                        out_calls: r.get::<i64>("out_calls").unwrap_or(0),
                    }),
                    _ => None,
                }
            })
            .collect())
    }

    async fn get_total_node_count(&self, repo: &str) -> Result<i64> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})-[:HAS_MODULE]->(m:Module) \
                     OPTIONAL MATCH (m)-[:HAS_CHUNK]->(c:Chunk) \
                     RETURN count(DISTINCT m) + count(DISTINCT c) AS total",
                )
                .param("repo_name", repo),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .first()
            .and_then(|r| r.get::<i64>("total").ok())
            .unwrap_or(0))
    }

    // ── get_module_call_graph ────────────────────────────────────────────────

    async fn get_intra_module_calls(&self, module_id: &str) -> Result<Vec<CallEdgeRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (m:Module {pg_id: $module_id})-[:HAS_CHUNK]->(src:Chunk)-[c:CALLS]->(tgt:Chunk)<-[:HAS_CHUNK]-(m) \
                     RETURN src.pg_id AS source, tgt.pg_id AS target, c.confidence AS confidence, c.method AS method",
                )
                .param("module_id", module_id),
            )
            .await
            .unwrap_or_default();

        Ok(map_call_edge_rows(&rows))
    }

    async fn get_chunk_note_ids(&self, module_id: &str) -> Result<Vec<(String, Vec<String>)>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (m:Module {pg_id: $module_id})-[:HAS_CHUNK]->(ch:Chunk) \
                     OPTIONAL MATCH (n:Note)-[:ATTACHED_TO]->(ch) \
                     RETURN ch.pg_id AS chunk_id, collect(n.pg_id) AS note_pg_ids",
                )
                .param("module_id", module_id),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(|r| {
                r.get::<String>("chunk_id").ok().map(|cid| {
                    let note_ids: Vec<String> = r
                        .get::<Vec<String>>("note_pg_ids")
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect();
                    (cid, note_ids)
                })
            })
            .collect())
    }

    async fn get_module_note_ids(&self, module_id: &str) -> Result<Vec<String>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (c:Note)-[:ATTACHED_TO]->(m:Module {pg_id: $module_id}) \
                     RETURN c.pg_id AS pg_id",
                )
                .param("module_id", module_id),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(|r| r.get::<String>("pg_id").ok())
            .collect())
    }

    async fn get_ghost_nodes(&self, module_id: &str) -> Result<Vec<GhostNodeRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (m:Module {pg_id: $module_id})-[:HAS_CHUNK]->(internal:Chunk) \
                     WITH m, collect(internal) AS internals \
                     UNWIND internals AS internal \
                     OPTIONAL MATCH (external:Chunk)-[c:CALLS]->(internal) \
                     WHERE NOT (m)-[:HAS_CHUNK]->(external) \
                     WITH external, 'caller' AS direction \
                     WHERE external IS NOT NULL \
                     RETURN DISTINCT external.pg_id AS id, external.name AS name, direction \
                     UNION \
                     MATCH (m:Module {pg_id: $module_id})-[:HAS_CHUNK]->(internal:Chunk)-[c:CALLS]->(external:Chunk) \
                     WHERE NOT (m)-[:HAS_CHUNK]->(external) \
                     RETURN DISTINCT external.pg_id AS id, external.name AS name, 'callee' AS direction",
                )
                .param("module_id", module_id),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(|r| {
                match (
                    r.get::<String>("id"),
                    r.get::<String>("name"),
                    r.get::<String>("direction"),
                ) {
                    (Ok(id), Ok(name), Ok(direction)) => Some(GhostNodeRow {
                        id,
                        name,
                        direction,
                    }),
                    _ => None,
                }
            })
            .collect())
    }

    async fn get_caller_edges(&self, module_id: &str) -> Result<Vec<CallEdgeRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (m:Module {pg_id: $module_id})-[:HAS_CHUNK]->(internal:Chunk) \
                     WITH m, collect(internal) AS internals \
                     UNWIND internals AS internal \
                     MATCH (external:Chunk)-[c:CALLS]->(internal) \
                     WHERE NOT (m)-[:HAS_CHUNK]->(external) \
                     RETURN external.pg_id AS source, internal.pg_id AS target, c.confidence AS confidence, c.method AS method",
                )
                .param("module_id", module_id),
            )
            .await
            .unwrap_or_default();

        Ok(map_call_edge_rows(&rows))
    }

    async fn get_callee_edges(&self, module_id: &str) -> Result<Vec<CallEdgeRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (m:Module {pg_id: $module_id})-[:HAS_CHUNK]->(internal:Chunk)-[c:CALLS]->(external:Chunk) \
                     WHERE NOT (m)-[:HAS_CHUNK]->(external) \
                     RETURN internal.pg_id AS source, external.pg_id AS target, c.confidence AS confidence, c.method AS method",
                )
                .param("module_id", module_id),
            )
            .await
            .unwrap_or_default();

        Ok(map_call_edge_rows(&rows))
    }

    // ── get_doc_graph ────────────────────────────────────────────────────────

    async fn get_cluster_explains(&self, repo: &str) -> Result<Vec<ClusterExplainsRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (tc:TopicCluster {repo_name: $repo_name})<-[:IN_CLUSTER]-(s:Section)\
                     -[e:EXPLAINS]->(target) \
                     RETURN tc.pg_id AS cluster_id, e.target_repo AS target_repo",
                )
                .param("repo_name", repo),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(|r| {
                match (
                    r.get::<String>("cluster_id"),
                    r.get::<String>("target_repo"),
                ) {
                    (Ok(cluster_id), Ok(target_repo)) => Some(ClusterExplainsRow {
                        cluster_id,
                        target_repo,
                    }),
                    _ => None,
                }
            })
            .collect())
    }

    async fn get_doc_nodes(&self, repo: &str) -> Result<Vec<GraphDocRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})<-[:BELONGS_TO]-(d:Document) \
                     RETURN d.pg_id AS pg_id, d.title AS title",
                )
                .param("repo_name", repo),
            )
            .await?;

        Ok(rows
            .iter()
            .filter_map(
                |r| match (r.get::<String>("pg_id"), r.get::<String>("title")) {
                    (Ok(pg_id), Ok(title)) => Some(GraphDocRow { pg_id, title }),
                    _ => None,
                },
            )
            .collect())
    }

    async fn get_doc_explains(&self, repo: &str) -> Result<Vec<DocExplainsRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})<-[:BELONGS_TO]-(d:Document)\
                     -[:HAS_SECTION]->(s:Section)-[e:EXPLAINS]->(target) \
                     RETURN d.pg_id AS doc_id, e.target_repo AS target_repo",
                )
                .param("repo_name", repo),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(
                |r| match (r.get::<String>("doc_id"), r.get::<String>("target_repo")) {
                    (Ok(doc_id), Ok(target_repo)) => Some(DocExplainsRow {
                        doc_id,
                        target_repo,
                    }),
                    _ => None,
                },
            )
            .collect())
    }

    // ── batch_section_explains (UNWIND — N+1 fix preserved) ─────────────────

    async fn batch_section_explains(
        &self,
        section_ids: Vec<String>,
    ) -> Result<Vec<SectionExplainsRow>> {
        if section_ids.is_empty() {
            return Ok(Vec::new());
        }

        let rows = self
            .graph
            .query(
                query(
                    "UNWIND $section_ids AS section_id \
                     MATCH (s:Section {pg_id: section_id})-[e:EXPLAINS]->(target) \
                     RETURN s.pg_id AS section_id, target.pg_id AS target_id, \
                            e.confidence AS confidence, e.method AS method, \
                            coalesce(e.target_repo, '') AS target_repo",
                )
                .param("section_ids", section_ids),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(|r| {
                match (
                    r.get::<String>("section_id"),
                    r.get::<String>("target_id"),
                    r.get::<f64>("confidence"),
                    r.get::<String>("method"),
                    r.get::<String>("target_repo"),
                ) {
                    (
                        Ok(section_id),
                        Ok(target_id),
                        Ok(confidence),
                        Ok(method),
                        Ok(target_repo),
                    ) => Some(SectionExplainsRow {
                        section_id,
                        target_id,
                        confidence,
                        method,
                        target_repo,
                    }),
                    _ => None,
                }
            })
            .collect())
    }

    // ── relink_explains ──────────────────────────────────────────────────────

    async fn explains_target_repos(&self, repo: &str) -> Result<Vec<String>> {
        // Cypher moved verbatim from `POST /api/v1/repos/:name/relink-explains` handler.
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (r:Repository {name: $repo_name})<-[:BELONGS_TO]-(d:Document)\
                     -[:HAS_SECTION]->(s:Section)-[e:EXPLAINS]->(target) \
                     RETURN DISTINCT e.target_repo AS target_repo",
                )
                .param("repo_name", repo),
            )
            .await
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(|r| r.get::<String>("target_repo").ok())
            .collect())
    }

    async fn fetch_call_edges(
        &self,
        repo: &str,
        min_confidence: f64,
    ) -> Result<Vec<CommunityEdgeRow>> {
        // module_path is NOT a Chunk property; the owning module is the parent
        // Module node reached via HAS_CHUNK. Chunks are 1:1 with modules, so the
        // two OPTIONAL MATCHes do not multiply the CALLS edge rows.
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (src:Chunk {repo_name: $repo})-[e:CALLS]->(tgt:Chunk {repo_name: $repo}) \
                     WHERE coalesce(e.confidence, 1.0) >= $min_conf \
                     OPTIONAL MATCH (sm:Module)-[:HAS_CHUNK]->(src) \
                     OPTIONAL MATCH (tm:Module)-[:HAS_CHUNK]->(tgt) \
                     RETURN src.pg_id AS source, src.name AS source_name, coalesce(sm.path, '') AS source_module, \
                            tgt.pg_id AS target, tgt.name AS target_name, coalesce(tm.path, '') AS target_module, \
                            coalesce(e.confidence, 1.0) AS weight",
                )
                .param("repo", repo)
                .param("min_conf", min_confidence),
            )
            .await?; // `?` preserves finding #38/#39

        let mut edges = Vec::with_capacity(rows.len());
        let mut dropped = 0usize;
        for r in &rows {
            match (r.get::<String>("source"), r.get::<String>("target")) {
                (Ok(source), Ok(target)) => edges.push(CommunityEdgeRow {
                    source,
                    target,
                    source_name: r.get::<String>("source_name").unwrap_or_default(),
                    source_module: r.get::<String>("source_module").unwrap_or_default(),
                    target_name: r.get::<String>("target_name").unwrap_or_default(),
                    target_module: r.get::<String>("target_module").unwrap_or_default(),
                    weight: r.get::<f64>("weight").unwrap_or(1.0),
                }),
                _ => dropped += 1,
            }
        }
        if dropped > 0 {
            tracing::warn!(
                repo,
                dropped,
                "fetch_call_edges: dropped CALLS rows with missing pg_id"
            );
        }
        Ok(edges)
    }

    async fn fetch_zero_caller_functions(&self, repo: &str) -> Result<Vec<DeadCodeCandidateRow>> {
        // Persisted-entry-point path: `is_entry_point` comes straight from the
        // graph via the IS_ENTRY_POINT edge Stage 7 wrote — no content re-fetch.
        // `module_path`/`visibility` live on the Chunk node (chunk_graph.rs).
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (c:Chunk {repo_name: $repo, chunk_type: 'function'}) \
                     WHERE NOT EXISTS { ()-[:CALLS]->(c) } \
                     RETURN c.name AS name, \
                            coalesce(c.module_path, '') AS module_path, \
                            c.fqn AS fqn, \
                            coalesce(c.visibility, 'unknown') AS visibility, \
                            EXISTS { (c)-[:IS_ENTRY_POINT]->(:Flow) } AS is_entry_point",
                )
                .param("repo", repo),
            )
            .await?; // `?` preserves finding #38/#39

        let mut candidates = Vec::with_capacity(rows.len());
        let mut dropped = 0usize;
        for r in &rows {
            match r.get::<String>("name") {
                Ok(name) => candidates.push(DeadCodeCandidateRow {
                    name,
                    module_path: r.get::<String>("module_path").unwrap_or_default(),
                    fqn: r.get::<String>("fqn").ok(),
                    visibility: r
                        .get::<String>("visibility")
                        .unwrap_or_else(|_| "unknown".into()),
                    is_entry_point: r.get::<bool>("is_entry_point").unwrap_or(false),
                }),
                Err(_) => dropped += 1,
            }
        }
        if dropped > 0 {
            tracing::warn!(
                repo,
                dropped,
                "fetch_zero_caller_functions: dropped Chunk rows with missing name"
            );
        }
        Ok(candidates)
    }

    async fn count_functions(&self, repo: &str) -> Result<i64> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (c:Chunk {repo_name: $repo, chunk_type: 'function'}) \
                     RETURN count(c) AS total",
                )
                .param("repo", repo),
            )
            .await?; // `?` preserves finding #38/#39

        Ok(rows
            .first()
            .and_then(|r| r.get::<i64>("total").ok())
            .unwrap_or(0))
    }

    async fn fetch_http_call_sites(&self) -> Result<Vec<HttpCallSiteRow>> {
        // GLOBAL (all repos): method/path live on the http_call chunk; the
        // caller function chunk carries pg_id/repo/fqn (fqn falls back to name).
        let rows = self
            .graph
            .query(query(
                "MATCH (caller:Chunk {chunk_type: 'function'})-[:MAKES_HTTP_CALL]->(hc:Chunk {chunk_type: 'http_call'}) \
                 RETURN caller.pg_id AS caller_pg_id, \
                        coalesce(caller.repo_name, '') AS caller_repo, \
                        coalesce(caller.fqn, caller.name, '') AS caller_fqn, \
                        coalesce(hc.http_method, 'ANY') AS http_method, \
                        coalesce(hc.http_path, '') AS http_path",
            ))
            .await?; // `?` preserves finding #38/#39

        let mut out = Vec::with_capacity(rows.len());
        let mut dropped = 0usize;
        for r in &rows {
            let path = r.get::<String>("http_path").unwrap_or_default();
            match r.get::<String>("caller_pg_id") {
                Ok(caller_pg_id) if !path.is_empty() => out.push(HttpCallSiteRow {
                    caller_pg_id,
                    caller_repo: r.get::<String>("caller_repo").unwrap_or_default(),
                    caller_fqn: r.get::<String>("caller_fqn").unwrap_or_default(),
                    http_method: r
                        .get::<String>("http_method")
                        .unwrap_or_else(|_| "ANY".into()),
                    http_path: path,
                }),
                _ => dropped += 1,
            }
        }
        if dropped > 0 {
            tracing::warn!(
                dropped,
                "fetch_http_call_sites: dropped rows with missing pg_id or empty http_path"
            );
        }
        Ok(out)
    }

    async fn fetch_route_sites(&self) -> Result<Vec<RouteSiteRow>> {
        // GLOBAL (all repos): method/path live on the route chunk; the handler
        // function chunk carries pg_id/repo/fqn (fqn falls back to name).
        let rows = self
            .graph
            .query(query(
                "MATCH (rt:Chunk {chunk_type: 'route'})-[:ROUTES_TO]->(handler:Chunk {chunk_type: 'function'}) \
                 RETURN handler.pg_id AS handler_pg_id, \
                        coalesce(handler.repo_name, '') AS handler_repo, \
                        coalesce(handler.fqn, handler.name, '') AS handler_fqn, \
                        coalesce(rt.http_method, 'ANY') AS http_method, \
                        coalesce(rt.http_path, '') AS http_path",
            ))
            .await?; // `?` preserves finding #38/#39

        let mut out = Vec::with_capacity(rows.len());
        let mut dropped = 0usize;
        for r in &rows {
            let path = r.get::<String>("http_path").unwrap_or_default();
            match r.get::<String>("handler_pg_id") {
                Ok(handler_pg_id) if !path.is_empty() => out.push(RouteSiteRow {
                    handler_pg_id,
                    handler_repo: r.get::<String>("handler_repo").unwrap_or_default(),
                    handler_fqn: r.get::<String>("handler_fqn").unwrap_or_default(),
                    http_method: r
                        .get::<String>("http_method")
                        .unwrap_or_else(|_| "ANY".into()),
                    http_path: path,
                }),
                _ => dropped += 1,
            }
        }
        if dropped > 0 {
            tracing::warn!(
                dropped,
                "fetch_route_sites: dropped rows with missing pg_id or empty http_path"
            );
        }
        Ok(out)
    }

    async fn fetch_decisions_for_symbol(
        &self,
        repo: &str,
        symbol: &str,
    ) -> Result<Vec<DecisionAttachmentRow>> {
        // Category is a (:Note)-[:TAGGED_AS]->(:Category {name}) edge, NOT an
        // n.category property — filter via the relationship. ATTACHED_TO points
        // Note→Chunk (verbatim direction from graph_traversal.rs NotedBy).
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (c:Chunk {repo_name: $repo})<-[:ATTACHED_TO]-(n:Note)-[:TAGGED_AS]->(:Category {name: 'DECISION'}) \
                     WHERE c.name CONTAINS $symbol OR coalesce(c.fqn, '') CONTAINS $symbol \
                     RETURN DISTINCT n.pg_id AS note_id, c.name AS chunk_name, c.fqn AS chunk_fqn",
                )
                .param("repo", repo)
                .param("symbol", symbol),
            )
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let id_str: String = r.get::<String>("note_id").unwrap_or_default();
            if let Ok(note_id) = Uuid::parse_str(&id_str) {
                out.push(DecisionAttachmentRow {
                    note_id,
                    chunk_name: r.get::<String>("chunk_name").unwrap_or_default(),
                    chunk_fqn: r.get::<String>("chunk_fqn").ok(),
                });
            }
        }
        Ok(out)
    }

    async fn fetch_supersede_chain(&self, note_id: Uuid) -> Result<Vec<SupersedeChainRow>> {
        // One UNION query for both directions. SUPERSEDES points NEWER→OLDER, so
        // ancestors (older notes the seed supersedes) are reached forward, and
        // descendants (newer notes that supersede the seed) backward. The seed
        // itself is emitted at hop 0 so a lone note still yields a 1-row chain.
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (n:Note {pg_id: $note_id}) \
                     RETURN n.pg_id AS note_id, 'self' AS direction, 0 AS hop_distance \
                     UNION \
                     MATCH p = (:Note {pg_id: $note_id})-[:SUPERSEDES*1..]->(older:Note) \
                     RETURN older.pg_id AS note_id, 'ancestor' AS direction, length(p) AS hop_distance \
                     UNION \
                     MATCH p = (newer:Note)-[:SUPERSEDES*1..]->(:Note {pg_id: $note_id}) \
                     RETURN newer.pg_id AS note_id, 'descendant' AS direction, length(p) AS hop_distance",
                )
                .param("note_id", note_id.to_string().as_str()),
            )
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let id_str: String = r.get::<String>("note_id").unwrap_or_default();
            if let Ok(nid) = Uuid::parse_str(&id_str) {
                out.push(SupersedeChainRow {
                    note_id: nid,
                    direction: r.get::<String>("direction").unwrap_or_default(),
                    hop_distance: r.get::<i64>("hop_distance").unwrap_or(0),
                });
            }
        }
        Ok(out)
    }

    async fn fetch_chunks_attached_to_note(&self, note_id: Uuid) -> Result<Vec<ChunkRefRow>> {
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (n:Note {pg_id: $note_id})-[:ATTACHED_TO]->(c:Chunk) \
                     RETURN c.name AS name, c.fqn AS fqn, \
                            c.module_path AS module_path, c.chunk_type AS chunk_type",
                )
                .param("note_id", note_id.to_string().as_str()),
            )
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            if let Ok(name) = r.get::<String>("name") {
                out.push(ChunkRefRow {
                    name,
                    fqn: r.get::<String>("fqn").ok(),
                    module_path: r.get::<String>("module_path").ok(),
                    chunk_type: r.get::<String>("chunk_type").ok(),
                });
            }
        }
        Ok(out)
    }
}
