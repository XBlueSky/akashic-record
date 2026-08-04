//! Neo4j adapter for `GraphTraversalRepo`.
//!
//! All Cypher is moved verbatim from the original call-sites:
//! - `akashic-retrieval::graphrag::mod.rs` (`expand_chunk`, `expand_section`,
//!   `expand_note`, flow entry-point expansion)
//! - `akashic-retrieval::graphrag::symbol_resolution` (`find_references`,
//!   `find_implementations`)

use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use akashic_domain::ports::{
    CallGraphRow, FlowEntryPointRow, FlowListRow, FlowMatchRow, FlowStepRow, GraphTraversalRepo,
    ImpactCallRow, ImpactExplainsRow, ImpactFlowRow, ImpactImportRow, ImpactNoteRow,
    ModuleImportRef, ReferenceEdge,
};
use akashic_domain::types::{GraphNeighbour, Space};

use crate::Neo4jPool;

/// Neo4j adapter implementing [`GraphTraversalRepo`].
#[derive(Clone)]
pub struct Neo4jGraphTraversalRepo {
    graph: Neo4jPool,
}

impl Neo4jGraphTraversalRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl GraphTraversalRepo for Neo4jGraphTraversalRepo {
    async fn expand_chunk(&self, pg_id: Uuid) -> Result<Vec<GraphNeighbour>> {
        // Cypher moved verbatim from
        // akashic-retrieval::graphrag::mod.rs::expand_chunk
        // (pattern comprehension form).
        let rows = self
            .graph
            .query(
                neo4rs::query(
                    "MATCH (c:Chunk {pg_id: $id}) \
                     RETURN \
                       [(m:Module)-[:HAS_CHUNK]->(c) | m.pg_id] AS module_ids, \
                       [(n:Note)-[:ATTACHED_TO]->(c) | n.pg_id] AS note_ids, \
                       [(s:Section)-[:EXPLAINS]->(c) | s.pg_id] AS section_ids, \
                       [(f:Flow)-[:FLOW_STEP]->(c) | f.pg_id] AS flow_ids",
                )
                .param("id", pg_id.to_string()),
            )
            .await?;

        let mut neighbors: Vec<GraphNeighbour> = Vec::new();

        if let Some(row) = rows.first() {
            if let Ok(ids) = row.get::<Vec<String>>("module_ids") {
                for id_str in ids {
                    if let Ok(id) = Uuid::parse_str(&id_str) {
                        neighbors.push(GraphNeighbour {
                            pg_id: id,
                            space: Space::Code,
                            entity_type: "module".into(),
                            name: String::new(),
                            hop: 1,
                        });
                    }
                }
            }
            if let Ok(ids) = row.get::<Vec<String>>("note_ids") {
                for id_str in ids {
                    if let Ok(id) = Uuid::parse_str(&id_str) {
                        neighbors.push(GraphNeighbour {
                            pg_id: id,
                            space: Space::Human,
                            entity_type: "note".into(),
                            name: String::new(),
                            hop: 1,
                        });
                    }
                }
            }
            if let Ok(ids) = row.get::<Vec<String>>("section_ids") {
                for id_str in ids {
                    if let Ok(id) = Uuid::parse_str(&id_str) {
                        neighbors.push(GraphNeighbour {
                            pg_id: id,
                            space: Space::Doc,
                            entity_type: "section".into(),
                            name: String::new(),
                            hop: 1,
                        });
                    }
                }
            }
            // Return raw flow ids so the caller can fetch their entry points
            // via find_flow_entry_points(). We mark them Code/flow so the
            // caller can tell them apart.
            if let Ok(ids) = row.get::<Vec<String>>("flow_ids") {
                for id_str in ids.into_iter().take(2) {
                    if let Ok(id) = Uuid::parse_str(&id_str) {
                        neighbors.push(GraphNeighbour {
                            pg_id: id,
                            space: Space::Code,
                            entity_type: "__flow__".into(), // sentinel: caller calls find_flow_entry_points
                            name: String::new(),
                            hop: 1,
                        });
                    }
                }
            }
        }

        Ok(neighbors)
    }

    async fn expand_section(&self, pg_id: Uuid) -> Result<Vec<GraphNeighbour>> {
        // Cypher moved verbatim from
        // akashic-retrieval::graphrag::mod.rs::expand_section.
        let rows = self
            .graph
            .query(
                neo4rs::query(
                    "MATCH (s:Section {pg_id: $id}) \
                     RETURN \
                       [(s)-[:EXPLAINS]->(target) | target.pg_id] AS target_ids, \
                       [(parent:Section)-[:HAS_SUBSECTION]->(s) | parent.pg_id] AS parent_ids",
                )
                .param("id", pg_id.to_string()),
            )
            .await?;

        let mut neighbors: Vec<GraphNeighbour> = Vec::new();

        if let Some(row) = rows.first() {
            if let Ok(ids) = row.get::<Vec<String>>("target_ids") {
                for id_str in ids {
                    if let Ok(id) = Uuid::parse_str(&id_str) {
                        neighbors.push(GraphNeighbour {
                            pg_id: id,
                            space: Space::Code,
                            entity_type: "chunk".into(),
                            name: String::new(),
                            hop: 1,
                        });
                    }
                }
            }
            if let Ok(ids) = row.get::<Vec<String>>("parent_ids") {
                for id_str in ids {
                    if let Ok(id) = Uuid::parse_str(&id_str) {
                        neighbors.push(GraphNeighbour {
                            pg_id: id,
                            space: Space::Doc,
                            entity_type: "section".into(),
                            name: String::new(),
                            hop: 1,
                        });
                    }
                }
            }
        }

        Ok(neighbors)
    }

    async fn expand_note(&self, pg_id: Uuid) -> Result<Vec<GraphNeighbour>> {
        // Cypher moved verbatim from
        // akashic-retrieval::graphrag::mod.rs::expand_note.
        let rows = self
            .graph
            .query(
                neo4rs::query(
                    "MATCH (n:Note {pg_id: $id}) \
                     RETURN [(n)-[:ATTACHED_TO]->(target) | target.pg_id] AS target_ids",
                )
                .param("id", pg_id.to_string()),
            )
            .await?;

        let mut neighbors: Vec<GraphNeighbour> = Vec::new();

        if let Some(row) = rows.first()
            && let Ok(ids) = row.get::<Vec<String>>("target_ids")
        {
            for id_str in ids {
                if let Ok(id) = Uuid::parse_str(&id_str) {
                    neighbors.push(GraphNeighbour {
                        pg_id: id,
                        space: Space::Code,
                        entity_type: "chunk".into(),
                        name: String::new(),
                        hop: 1,
                    });
                }
            }
        }

        Ok(neighbors)
    }

    async fn find_flow_entry_points(&self, flow_id: Uuid) -> Result<Vec<Uuid>> {
        // Cypher moved verbatim from
        // akashic-retrieval::graphrag::mod.rs::expand_chunk flow inner loop.
        let ep_rows = self
            .graph
            .query(
                neo4rs::query(
                    "MATCH (ep:Chunk)-[:IS_ENTRY_POINT]->(f:Flow {pg_id: $fid}) \
                     RETURN ep.pg_id AS ep_id",
                )
                .param("fid", flow_id.to_string()),
            )
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "Flow entry point query failed");
                vec![]
            });

        let mut result = Vec::new();
        for row in &ep_rows {
            if let Ok(ep_str) = row.get::<String>("ep_id")
                && let Ok(ep_uuid) = Uuid::parse_str(&ep_str)
            {
                result.push(ep_uuid);
            }
        }
        Ok(result)
    }

    async fn find_references_by_fqn(
        &self,
        fqn: &str,
        repo: Option<&str>,
        min_confidence: f64,
    ) -> Result<Vec<ReferenceEdge>> {
        // Cypher moved verbatim from
        // akashic-retrieval::graphrag::symbol_resolution::find_references.
        let repo_filter = if repo.is_some() {
            "AND target.repo_name = $repo"
        } else {
            ""
        };
        let cypher = format!(
            "MATCH (caller:Chunk)-[r:CALLS|REFERENCES]->(target:Chunk {{fqn: $fqn}}) \
             WHERE r.confidence >= $min_conf {repo_filter} \
             RETURN DISTINCT caller.fqn AS fqn, caller.name AS name, \
                    caller.start_line AS start_line, caller.end_line AS end_line, \
                    r.confidence AS confidence, r.method AS method, \
                    coalesce(r.ref_kind, 'call') AS ref_kind \
             ORDER BY confidence DESC, fqn"
        );
        let mut q = neo4rs::query(&cypher)
            .param("fqn", fqn)
            .param("min_conf", min_confidence);
        if let Some(r) = repo {
            q = q.param("repo", r);
        }
        let rows = self.graph.query(q).await?;

        let mut refs: Vec<ReferenceEdge> = Vec::new();
        for row in &rows {
            let ref_kind: String = row.get("ref_kind").unwrap_or_else(|_| "call".to_string());
            let caller_fqn: String = row.get("fqn").unwrap_or_default();
            let caller_name: String = row.get("name").unwrap_or_default();
            let start_line: Option<i64> = row.get::<i64>("start_line").ok();
            let end_line: Option<i64> = row.get::<i64>("end_line").ok();
            let confidence: f64 = row.get("confidence").unwrap_or(0.0);
            let method: String = row.get("method").unwrap_or_default();
            refs.push(ReferenceEdge {
                caller_fqn,
                caller_name,
                start_line,
                end_line,
                confidence,
                method,
                ref_kind,
            });
        }

        Ok(refs)
    }

    async fn find_module_imports_by_fqn(
        &self,
        fqn: &str,
        repo: Option<&str>,
    ) -> Result<Vec<ModuleImportRef>> {
        // Cypher moved verbatim from
        // akashic-retrieval::graphrag::symbol_resolution::find_references
        // module-import branch.
        let repo_filter = if repo.is_some() {
            "AND target.repo_name = $repo"
        } else {
            ""
        };
        let mcypher = format!(
            "MATCH (m:Module)-[r:REFERENCES {{ref_kind: 'import'}}]->(target:Chunk {{fqn: $fqn}}) \
             WHERE true {repo_filter} \
             RETURN DISTINCT m.path AS path ORDER BY path"
        );
        let mut mq = neo4rs::query(&mcypher).param("fqn", fqn);
        if let Some(r) = repo {
            mq = mq.param("repo", r);
        }
        let mrows = self.graph.query(mq).await?;

        let mut result = Vec::new();
        for row in &mrows {
            let path: String = row.get("path").unwrap_or_default();
            if !path.is_empty() {
                result.push(ModuleImportRef { module_path: path });
            }
        }
        Ok(result)
    }

    async fn find_implementations_by_fqn(
        &self,
        fqn: &str,
        repo: Option<&str>,
        implementors: bool,
    ) -> Result<Vec<ReferenceEdge>> {
        // Cypher moved verbatim from
        // akashic-retrieval::graphrag::symbol_resolution::find_implementations.
        let cypher = if implementors {
            let repo_filter = if repo.is_some() {
                "AND target.repo_name = $repo"
            } else {
                ""
            };
            format!(
                "MATCH (other:Chunk)-[r:IMPLEMENTS]->(target:Chunk {{fqn: $fqn}}) \
                 WHERE true {repo_filter} \
                 RETURN DISTINCT other.fqn AS fqn, other.name AS name, \
                        other.start_line AS start_line, other.end_line AS end_line, \
                        coalesce(r.impl_kind, 'implements') AS ref_kind \
                 ORDER BY fqn"
            )
        } else {
            let repo_filter = if repo.is_some() {
                "AND src.repo_name = $repo"
            } else {
                ""
            };
            format!(
                "MATCH (src:Chunk {{fqn: $fqn}})-[r:IMPLEMENTS]->(other:Chunk) \
                 WHERE true {repo_filter} \
                 RETURN DISTINCT other.fqn AS fqn, other.name AS name, \
                        other.start_line AS start_line, other.end_line AS end_line, \
                        coalesce(r.impl_kind, 'implements') AS ref_kind \
                 ORDER BY fqn"
            )
        };
        let mut q = neo4rs::query(&cypher).param("fqn", fqn);
        if let Some(r) = repo {
            q = q.param("repo", r);
        }
        let rows = self.graph.query(q).await?;

        let mut refs: Vec<ReferenceEdge> = Vec::new();
        for row in &rows {
            let caller_fqn: String = row.get("fqn").unwrap_or_default();
            let caller_name: String = row.get("name").unwrap_or_default();
            let ref_kind: String = row
                .get("ref_kind")
                .unwrap_or_else(|_| "implements".to_string());
            let start_line: Option<i64> = row.get::<i64>("start_line").ok();
            let end_line: Option<i64> = row.get::<i64>("end_line").ok();
            refs.push(ReferenceEdge {
                caller_fqn,
                caller_name,
                start_line,
                end_line,
                confidence: 1.0,
                method: "implements".to_string(),
                ref_kind,
            });
        }

        Ok(refs)
    }

    // ── A2a: traverse_code_calls ─────────────────────────────────────────────

    async fn traverse_calls_downstream(
        &self,
        symbol: &str,
        repo: Option<&str>,
        max_depth: u8,
        min_confidence: f64,
    ) -> Result<Vec<CallGraphRow>> {
        // Cypher moved VERBATIM from mcp/tools.rs::traverse_code_calls downstream branch.
        let repo_filter = if repo.is_some() {
            "AND start.repo_name = $repo"
        } else {
            ""
        };
        let cypher = format!(
            "MATCH (start:Chunk) WHERE start.name CONTAINS $symbol {repo_filter} \
             WITH start \
             MATCH path = (start)-[:CALLS*1..{max_depth}]->(callee:Chunk) \
             WHERE ALL(r IN relationships(path) WHERE r.confidence >= $min_confidence) \
             WITH callee, length(path) AS depth, \
                  last(relationships(path)).confidence AS conf, \
                  last(relationships(path)).method AS method \
             RETURN DISTINCT callee.name AS name, callee.module_path AS module, \
                    callee.chunk_type AS ctype, depth, conf, method \
             ORDER BY depth, name"
        );
        let mut q = neo4rs::query(&cypher)
            .param("symbol", symbol)
            .param("min_confidence", min_confidence);
        if let Some(r) = repo {
            q = q.param("repo", r);
        }
        let rows = self.graph.query(q).await?;
        let mut result = Vec::new();
        for row in &rows {
            result.push(CallGraphRow {
                name: row.get("name").unwrap_or_default(),
                module: row.get("module").unwrap_or_default(),
                ctype: row.get("ctype").unwrap_or_default(),
                depth: row.get("depth").unwrap_or(1),
                conf: row.get("conf").unwrap_or(0.0),
                method: row.get("method").unwrap_or_default(),
            });
        }
        Ok(result)
    }

    async fn traverse_calls_upstream(
        &self,
        symbol: &str,
        repo: Option<&str>,
        max_depth: u8,
        min_confidence: f64,
    ) -> Result<Vec<CallGraphRow>> {
        // Cypher moved VERBATIM from mcp/tools.rs::traverse_code_calls upstream branch.
        let repo_filter = if repo.is_some() {
            "AND start.repo_name = $repo"
        } else {
            ""
        };
        let cypher = format!(
            "MATCH (start:Chunk) WHERE start.name CONTAINS $symbol {repo_filter} \
             WITH start \
             MATCH path = (caller:Chunk)-[:CALLS*1..{max_depth}]->(start) \
             WHERE ALL(r IN relationships(path) WHERE r.confidence >= $min_confidence) \
             WITH caller, length(path) AS depth, \
                  head(relationships(path)).confidence AS conf, \
                  head(relationships(path)).method AS method \
             RETURN DISTINCT caller.name AS name, caller.module_path AS module, \
                    caller.chunk_type AS ctype, depth, conf, method \
             ORDER BY depth, name"
        );
        let mut q = neo4rs::query(&cypher)
            .param("symbol", symbol)
            .param("min_confidence", min_confidence);
        if let Some(r) = repo {
            q = q.param("repo", r);
        }
        let rows = self.graph.query(q).await?;
        let mut result = Vec::new();
        for row in &rows {
            result.push(CallGraphRow {
                name: row.get("name").unwrap_or_default(),
                module: row.get("module").unwrap_or_default(),
                ctype: row.get("ctype").unwrap_or_default(),
                depth: row.get("depth").unwrap_or(1),
                conf: row.get("conf").unwrap_or(0.0),
                method: row.get("method").unwrap_or_default(),
            });
        }
        Ok(result)
    }

    // ── A2a: trace_execution_flow ────────────────────────────────────────────

    async fn list_flows_by_repo(&self, repo: &str) -> Result<Vec<FlowListRow>> {
        // Cypher moved VERBATIM from mcp/tools.rs::trace_execution_flow list_all branch.
        let rows = self
            .graph
            .query(
                neo4rs::query(
                    "MATCH (f:Flow {repo_name: $repo}) \
                     RETURN f.pg_id AS id, f.name AS name, f.entry_type AS etype, \
                            f.step_count AS steps, f.truncated AS trunc \
                     ORDER BY f.entry_type, f.name",
                )
                .param("repo", repo),
            )
            .await?;
        let mut result = Vec::new();
        for row in &rows {
            result.push(FlowListRow {
                name: row.get("name").unwrap_or_default(),
                etype: row.get("etype").unwrap_or_default(),
                steps: row.get("steps").unwrap_or(0),
                trunc: row.get("trunc").unwrap_or(false),
            });
        }
        Ok(result)
    }

    async fn find_flows_by_symbol(
        &self,
        symbol: &str,
        repo: Option<&str>,
        entry_type: Option<&str>,
    ) -> Result<Vec<FlowMatchRow>> {
        // Cypher moved VERBATIM from mcp/tools.rs::trace_execution_flow symbol-search branch.
        let mut cypher = String::from(
            "MATCH (c:Chunk) WHERE c.name CONTAINS $symbol \
             MATCH (f:Flow)-[fs:FLOW_STEP]->(c) ",
        );
        if repo.is_some() {
            cypher.push_str("WHERE f.repo_name = $repo ");
        }
        if entry_type.is_some() {
            if repo.is_some() {
                cypher.push_str("AND f.entry_type = $etype ");
            } else {
                cypher.push_str("WHERE f.entry_type = $etype ");
            }
        }
        cypher.push_str(
            "RETURN DISTINCT f.pg_id AS flow_id, f.name AS flow_name, \
                    f.entry_type AS flow_etype, f.step_count AS flow_steps, \
                    fs.position AS my_position \
             ORDER BY flow_name",
        );
        let mut q = neo4rs::query(&cypher).param("symbol", symbol);
        if let Some(r) = repo {
            q = q.param("repo", r);
        }
        if let Some(e) = entry_type {
            q = q.param("etype", e);
        }
        let rows = self.graph.query(q).await?;
        let mut result = Vec::new();
        for row in &rows {
            result.push(FlowMatchRow {
                flow_id: row.get("flow_id").unwrap_or_default(),
                flow_name: row.get("flow_name").unwrap_or_default(),
                flow_etype: row.get("flow_etype").unwrap_or_default(),
                flow_steps: row.get("flow_steps").unwrap_or(0),
                my_position: row.get("my_position").unwrap_or(-1),
            });
        }
        Ok(result)
    }

    async fn get_flow_steps(&self, flow_id: &str, max_depth: u8) -> Result<Vec<FlowStepRow>> {
        // Cypher moved VERBATIM from mcp/tools.rs::trace_execution_flow per-flow step fetch.
        let rows = self
            .graph
            .query(
                neo4rs::query(
                    "MATCH (f:Flow {pg_id: $fid})-[fs:FLOW_STEP]->(c:Chunk) \
                     WHERE fs.depth <= $max_depth \
                     RETURN c.name AS name, c.module_path AS module, \
                            fs.position AS pos, fs.depth AS depth \
                     ORDER BY fs.position",
                )
                .param("fid", flow_id)
                .param("max_depth", max_depth as i64),
            )
            .await
            .unwrap_or_default();
        let mut result = Vec::new();
        for row in &rows {
            result.push(FlowStepRow {
                name: row.get("name").unwrap_or_default(),
                pos: row.get("pos").unwrap_or(0),
                depth: row.get("depth").unwrap_or(0),
            });
        }
        Ok(result)
    }

    // ── A2a: analyze_impact traversals ───────────────────────────────────────

    async fn impact_callers(
        &self,
        target: &str,
        repo: Option<&str>,
        max_depth: u8,
        limit: usize,
    ) -> Result<Vec<ImpactCallRow>> {
        // Cypher moved VERBATIM from mcp/tools.rs::analyze_impact traversal 1 (CalledBy).
        let match_clause =
            "MATCH (c:Chunk) WHERE c.name CONTAINS $target OR c.module_path CONTAINS $target";
        let repo_filter = if repo.is_some() {
            "AND c.repo_name = $repo"
        } else {
            ""
        };
        let cypher = format!(
            "{match_clause} {repo_filter} \
             WITH c \
             MATCH path = (caller:Chunk)-[:CALLS*1..{max_depth}]->(c) \
             WITH caller, length(path) AS depth, \
                  [r IN relationships(path) | r.confidence] AS confs \
             RETURN DISTINCT caller.pg_id AS id, caller.name AS name, \
                    caller.module_path AS module, caller.chunk_type AS ctype, \
                    depth, reduce(m = 1.0, c IN confs | m * c) AS conf \
             LIMIT {limit}"
        );
        let mut q = neo4rs::query(&cypher).param("target", target);
        if let Some(r) = repo {
            q = q.param("repo", r);
        }
        let rows = self.graph.query(q).await?;
        let mut result = Vec::new();
        for row in &rows {
            let id_str: String = row.get("id").unwrap_or_default();
            if let Ok(id) = Uuid::parse_str(&id_str) {
                result.push(ImpactCallRow {
                    pg_id: id,
                    name: row.get("name").unwrap_or_default(),
                    module: row.get("module").unwrap_or_default(),
                    ctype: row.get("ctype").unwrap_or_default(),
                    depth: row.get("depth").unwrap_or(1),
                    conf: row.get("conf").unwrap_or(1.0),
                });
            }
        }
        Ok(result)
    }

    async fn impact_importers(
        &self,
        target: &str,
        repo: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ImpactImportRow>> {
        // Cypher moved VERBATIM from mcp/tools.rs::analyze_impact traversal 2 (ImportedBy).
        let match_clause =
            "MATCH (c:Chunk) WHERE c.name CONTAINS $target OR c.module_path CONTAINS $target";
        let repo_filter = if repo.is_some() {
            "AND c.repo_name = $repo"
        } else {
            ""
        };
        let cypher = format!(
            "{match_clause} {repo_filter} \
             WITH c \
             MATCH (m:Module)-[:HAS_CHUNK]->(c) \
             MATCH (importer:Module)-[:IMPORTS_FROM]->(m) \
             RETURN DISTINCT importer.pg_id AS id, importer.path AS name, \
                    importer.path AS module, 'module' AS ctype \
             LIMIT {limit}"
        );
        let mut q = neo4rs::query(&cypher).param("target", target);
        if let Some(r) = repo {
            q = q.param("repo", r);
        }
        let rows = self.graph.query(q).await?;
        let mut result = Vec::new();
        for row in &rows {
            let id_str: String = row.get("id").unwrap_or_default();
            if let Ok(id) = Uuid::parse_str(&id_str) {
                result.push(ImpactImportRow {
                    pg_id: id,
                    name: row.get("name").unwrap_or_default(),
                    module: row.get("module").unwrap_or_default(),
                    ctype: row.get("ctype").unwrap_or_default(),
                });
            }
        }
        Ok(result)
    }

    async fn impact_explains(
        &self,
        target: &str,
        repo: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ImpactExplainsRow>> {
        // Cypher moved VERBATIM from mcp/tools.rs::analyze_impact traversal 3 (ExplainedBy).
        let match_clause =
            "MATCH (c:Chunk) WHERE c.name CONTAINS $target OR c.module_path CONTAINS $target";
        let repo_filter = if repo.is_some() {
            "AND c.repo_name = $repo"
        } else {
            ""
        };
        let cypher = format!(
            "{match_clause} {repo_filter} \
             WITH c \
             MATCH (s:Section)-[e:EXPLAINS]->(c) \
             RETURN DISTINCT s.pg_id AS id, s.heading AS name, \
                    'doc' AS module, 'section' AS ctype, \
                    e.confidence AS conf \
             LIMIT {limit}"
        );
        let mut q = neo4rs::query(&cypher).param("target", target);
        if let Some(r) = repo {
            q = q.param("repo", r);
        }
        let rows = self.graph.query(q).await?;
        let mut result = Vec::new();
        for row in &rows {
            let id_str: String = row.get("id").unwrap_or_default();
            if let Ok(id) = Uuid::parse_str(&id_str) {
                result.push(ImpactExplainsRow {
                    pg_id: id,
                    name: row.get("name").unwrap_or_default(),
                    module: row.get("module").unwrap_or_default(),
                    ctype: row.get("ctype").unwrap_or_default(),
                    conf: row.get("conf").unwrap_or(1.0),
                });
            }
        }
        Ok(result)
    }

    async fn impact_notes(
        &self,
        target: &str,
        repo: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ImpactNoteRow>> {
        // Cypher moved VERBATIM from mcp/tools.rs::analyze_impact traversal 4 (NotedBy).
        let match_clause =
            "MATCH (c:Chunk) WHERE c.name CONTAINS $target OR c.module_path CONTAINS $target";
        let repo_filter = if repo.is_some() {
            "AND c.repo_name = $repo"
        } else {
            ""
        };
        let cypher = format!(
            "{match_clause} {repo_filter} \
             WITH c \
             MATCH (n:Note)-[:ATTACHED_TO]->(c) \
             RETURN DISTINCT n.pg_id AS id, n.title AS name, \
                    n.repo_name AS module, 'note' AS ctype \
             LIMIT {limit}"
        );
        let mut q = neo4rs::query(&cypher).param("target", target);
        if let Some(r) = repo {
            q = q.param("repo", r);
        }
        let rows = self.graph.query(q).await?;
        let mut result = Vec::new();
        for row in &rows {
            let id_str: String = row.get("id").unwrap_or_default();
            if let Ok(id) = Uuid::parse_str(&id_str) {
                result.push(ImpactNoteRow {
                    pg_id: id,
                    name: row.get("name").unwrap_or_default(),
                    module: row.get("module").unwrap_or_default(),
                    ctype: row.get("ctype").unwrap_or_default(),
                });
            }
        }
        Ok(result)
    }

    async fn impact_flows(
        &self,
        target: &str,
        repo: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ImpactFlowRow>> {
        // Cypher moved VERBATIM from mcp/tools.rs::analyze_impact traversal 5 (InFlow).
        let match_clause =
            "MATCH (c:Chunk) WHERE c.name CONTAINS $target OR c.module_path CONTAINS $target";
        let repo_filter = if repo.is_some() {
            "AND c.repo_name = $repo"
        } else {
            ""
        };
        let cypher = format!(
            "{match_clause} {repo_filter} \
             WITH c \
             MATCH (f:Flow)-[fs:FLOW_STEP]->(c) \
             RETURN DISTINCT f.pg_id AS flow_id, f.name AS flow_name, \
                    f.entry_type AS flow_etype, f.step_count AS flow_steps, \
                    fs.position AS my_pos \
             LIMIT {limit}"
        );
        let mut q = neo4rs::query(&cypher).param("target", target);
        if let Some(r) = repo {
            q = q.param("repo", r);
        }
        let rows = self.graph.query(q).await?;
        let mut result = Vec::new();
        for row in &rows {
            result.push(ImpactFlowRow {
                flow_id: row.get("flow_id").unwrap_or_default(),
                flow_name: row.get("flow_name").unwrap_or_default(),
                flow_etype: row.get("flow_etype").unwrap_or_default(),
                flow_steps: row.get("flow_steps").unwrap_or(0),
                my_pos: row.get("my_pos").unwrap_or(0),
            });
        }
        Ok(result)
    }

    async fn get_flow_entry_points_by_id(&self, flow_id: &str) -> Result<Vec<FlowEntryPointRow>> {
        // Cypher moved VERBATIM from mcp/tools.rs::analyze_impact IS_ENTRY_POINT inner query.
        let rows = self
            .graph
            .query(
                neo4rs::query(
                    "MATCH (ep:Chunk)-[:IS_ENTRY_POINT]->(f:Flow {pg_id: $fid}) \
                     RETURN ep.name AS ep_name",
                )
                .param("fid", flow_id),
            )
            .await
            .unwrap_or_default();
        let mut result = Vec::new();
        for row in &rows {
            result.push(FlowEntryPointRow {
                ep_name: row.get("ep_name").unwrap_or_default(),
            });
        }
        Ok(result)
    }
}
