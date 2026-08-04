//! Neo4j adapter for `SnapshotGraphRepo` (Roadmap E2) — export (read) side only.
//!
//! Import reuses the EXISTING write ports the ingestion pipeline itself
//! calls (`ChunkGraphRepo`, `ModuleGraphRepo`, `IngestEdgeRepo`,
//! `FlowGraphRepo`, `CommunityGraphRepo`) directly from the CLI (Task 4) —
//! those already MERGE/CREATE by explicit `pg_id`, so no new Neo4j write
//! surface is needed here.
//!
//! **Ground truth cross-checked against `ingest_edge.rs`'s write side**: the
//! five `IngestEdge`-shaped relationship types do NOT all store their "kind"
//! tag under a `ref_kind` property. `CALLS` sets only `confidence`+`method`
//! (no kind at all). `REFERENCES` (Chunk→Chunk) sets `ref_kind`. `IMPLEMENTS`
//! sets `impl_kind`. `ROUTES_TO` and `MAKES_HTTP_CALL` both set `http_method`.
//! All four of those "kind" properties feed the SAME `IngestEdge.ref_kind`
//! domain field (the struct is reused across all 5 edge types, each write
//! method interpreting the field as its own tag) — so `export_ingest_edges`
//! takes the actual Neo4j property name to read as a parameter and maps it
//! back onto `ref_kind` uniformly, keeping export/re-import symmetric with
//! the write side's per-edge-type property naming.

use anyhow::{Context, Result};
use async_trait::async_trait;
use neo4rs::query;
use uuid::Uuid;

use akashic_domain::ports::{ChunkGraphNode, SnapshotGraphRepo};
use akashic_domain::types::{
    FlowRecord, FlowStepRecord, GraphCommunitySnapshot, GraphRepoSnapshot, IngestEdge,
};

use crate::Neo4jPool;

#[derive(Clone)]
pub struct Neo4jSnapshotRepo {
    graph: Neo4jPool,
}

impl Neo4jSnapshotRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }

    /// Export one `IngestEdge`-shaped Chunk→Chunk relationship type.
    ///
    /// `kind_prop` is the actual Neo4j property name that carries this edge
    /// type's "kind" tag on the write side (verified per-type against
    /// `ingest_edge.rs`): `"ref_kind"` for `REFERENCES`, `"impl_kind"` for
    /// `IMPLEMENTS`, `"http_method"` for `ROUTES_TO`/`MAKES_HTTP_CALL`. For
    /// `CALLS` (which sets no such property) any never-set name reads back
    /// `null` → `None`, which is correct since `create_call_edges` never
    /// reads/writes `IngestEdge.ref_kind` either.
    async fn export_ingest_edges(
        &self,
        repo_name: &str,
        rel_type: &str,
        kind_prop: &str,
    ) -> Result<Vec<IngestEdge>> {
        let cypher = format!(
            "MATCH (src:Chunk {{repo_name: $repo}})-[e:{rel_type}]->(tgt:Chunk) \
             RETURN src.pg_id AS src_id, tgt.pg_id AS tgt_id, e.confidence AS confidence, \
                    e.method AS method, e.{kind_prop} AS ref_kind"
        );
        let rows = self
            .graph
            .query(query(&cypher).param("repo", repo_name))
            .await
            .with_context(|| format!("Failed to export {rel_type} edges"))?;

        let mut edges = Vec::new();
        for row in rows {
            let src_id: String = row.get("src_id").unwrap_or_default();
            let tgt_id: String = row.get("tgt_id").unwrap_or_default();
            let confidence: f64 = row.get("confidence").unwrap_or(0.0);
            let method: String = row.get("method").unwrap_or_default();
            let ref_kind: Option<String> = row.get("ref_kind").ok();
            edges.push(IngestEdge {
                src_chunk_id: Uuid::parse_str(&src_id)
                    .with_context(|| format!("bad src pg_id in {rel_type} edge: {src_id}"))?,
                tgt_chunk_id: Uuid::parse_str(&tgt_id)
                    .with_context(|| format!("bad tgt pg_id in {rel_type} edge: {tgt_id}"))?,
                confidence: confidence as f32,
                method,
                ref_kind,
            });
        }
        Ok(edges)
    }
}

#[async_trait]
impl SnapshotGraphRepo for Neo4jSnapshotRepo {
    async fn export_repo_snapshot(&self, repo_name: &str) -> Result<GraphRepoSnapshot> {
        // ── Chunk nodes ──────────────────────────────────────────────────
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (c:Chunk {repo_name: $repo}) \
                     RETURN c.pg_id AS pg_id, c.name AS name, c.chunk_type AS chunk_type, \
                            c.module_path AS module_path, c.fqn AS fqn, c.parent_fqn AS parent_fqn, \
                            c.start_line AS start_line, c.end_line AS end_line, \
                            c.visibility AS visibility, c.is_async AS is_async, \
                            c.is_static AS is_static, c.is_exported AS is_exported, \
                            c.http_method AS http_method, c.http_path AS http_path",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to export Chunk nodes")?;

        let mut chunk_nodes = Vec::new();
        for row in rows {
            chunk_nodes.push(ChunkGraphNode {
                pg_id: row.get("pg_id").unwrap_or_default(),
                name: row.get("name").unwrap_or_default(),
                chunk_type: row.get("chunk_type").unwrap_or_default(),
                module_path: row.get("module_path").unwrap_or_default(),
                fqn: row.get("fqn").ok(),
                parent_fqn: row.get("parent_fqn").ok(),
                start_line: row.get("start_line").unwrap_or(0),
                end_line: row.get("end_line").unwrap_or(0),
                visibility: row.get("visibility").unwrap_or_default(),
                is_async: row.get("is_async").unwrap_or(false),
                is_static: row.get("is_static").unwrap_or(false),
                is_exported: row.get("is_exported").unwrap_or(false),
                http_method: row.get("http_method").ok(),
                http_path: row.get("http_path").ok(),
            });
        }

        // ── Module nodes (same shape as ModuleGraphRepo::get_module_nodes) ──
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (m:Module {repo_name: $repo}) RETURN m.pg_id AS pg_id, m.path AS path",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to export Module nodes")?;
        let mut module_nodes = Vec::new();
        for row in rows {
            let pg_id: String = row.get("pg_id").unwrap_or_default();
            let path: String = row.get("path").unwrap_or_default();
            module_nodes.push((pg_id, path));
        }

        // ── The five IngestEdge-shaped edge types ───────────────────────
        // kind_prop per type verified against ingest_edge.rs's SET/MERGE clauses:
        // CALLS sets no kind property at all (placeholder name reads back null);
        // REFERENCES -> ref_kind; IMPLEMENTS -> impl_kind; ROUTES_TO and
        // MAKES_HTTP_CALL -> http_method.
        let calls_edges = self
            .export_ingest_edges(repo_name, "CALLS", "ref_kind")
            .await?;
        let reference_edges = self
            .export_ingest_edges(repo_name, "REFERENCES", "ref_kind")
            .await?;
        let implements_edges = self
            .export_ingest_edges(repo_name, "IMPLEMENTS", "impl_kind")
            .await?;
        let routes_to_edges = self
            .export_ingest_edges(repo_name, "ROUTES_TO", "http_method")
            .await?;
        let http_call_edges = self
            .export_ingest_edges(repo_name, "MAKES_HTTP_CALL", "http_method")
            .await?;

        // ── Chunk `[:TAGGED_WITH]` edges (Stage 4 writes at least one tag —
        //    the chunk_type itself — for EVERY chunk via
        //    `ChunkGraphRepo::create_tagged_with_edges`; omitting this from the
        //    snapshot silently drops real graph data). ─────────────────────
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (c:Chunk {repo_name: $repo})-[:TAGGED_WITH]->(t:Tag) \
                     RETURN c.pg_id AS chunk_pg_id, t.name AS tag_name",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to export chunk tags")?;
        let mut chunk_tags = Vec::new();
        for row in rows {
            let chunk_pg_id: String = row.get("chunk_pg_id").unwrap_or_default();
            let tag_name: String = row.get("tag_name").unwrap_or_default();
            chunk_tags.push((chunk_pg_id, tag_name));
        }

        // ── Module→Chunk import REFERENCES edges (distinct from the Chunk→Chunk
        //    REFERENCES edges above — distinguished by the :Module source label) ──
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (m:Module {repo_name: $repo})-[:REFERENCES {ref_kind: 'import'}]->(s:Chunk) \
                     RETURN m.path AS module_path, s.pg_id AS chunk_id",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to export symbol-import edges")?;
        let mut symbol_import_edges = Vec::new();
        for row in rows {
            let module_path: String = row.get("module_path").unwrap_or_default();
            let chunk_id: String = row.get("chunk_id").unwrap_or_default();
            symbol_import_edges.push((
                module_path,
                Uuid::parse_str(&chunk_id).context("bad chunk pg_id in symbol-import edge")?,
            ));
        }

        // ── Module IMPORTS_FROM edges ────────────────────────────────────
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (src:Module {repo_name: $repo})-[:IMPORTS_FROM]->(tgt:Module) \
                     RETURN src.pg_id AS src_id, tgt.pg_id AS tgt_id",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to export Module IMPORTS_FROM edges")?;
        let mut module_import_edges = Vec::new();
        for row in rows {
            let src_id: String = row.get("src_id").unwrap_or_default();
            let tgt_id: String = row.get("tgt_id").unwrap_or_default();
            module_import_edges.push((src_id, tgt_id));
        }

        // ── Flows (Flow nodes + IS_ENTRY_POINT + ordered FLOW_STEP edges) ──
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (ep:Chunk)-[:IS_ENTRY_POINT]->(f:Flow {repo_name: $repo}) \
                     RETURN f.pg_id AS flow_id, f.name AS name, f.entry_type AS entry_type, \
                            ep.pg_id AS entry_chunk_id, f.step_count AS step_count, \
                            f.truncated AS truncated",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to export Flow nodes")?;

        let mut flows = Vec::new();
        for row in rows {
            let flow_id: String = row.get("flow_id").unwrap_or_default();
            let entry_chunk_id: String = row.get("entry_chunk_id").unwrap_or_default();

            let step_rows = self
                .graph
                .query(
                    query(
                        "MATCH (f:Flow {pg_id: $flow_id})-[step:FLOW_STEP]->(c:Chunk) \
                         RETURN c.pg_id AS chunk_id, step.position AS position, step.depth AS depth \
                         ORDER BY step.position",
                    )
                    .param("flow_id", flow_id.as_str()),
                )
                .await
                .with_context(|| format!("Failed to export FLOW_STEP edges for flow {flow_id}"))?;

            let mut steps = Vec::new();
            for step_row in step_rows {
                let chunk_id: String = step_row.get("chunk_id").unwrap_or_default();
                let position: i64 = step_row.get("position").unwrap_or(0);
                let depth: i64 = step_row.get("depth").unwrap_or(0);
                steps.push(FlowStepRecord {
                    chunk_id: Uuid::parse_str(&chunk_id).context("bad chunk pg_id in FLOW_STEP")?,
                    position: position as u32,
                    depth: depth as u32,
                });
            }

            let step_count: i64 = row.get("step_count").unwrap_or(0);
            flows.push(FlowRecord {
                flow_id: flow_id.clone(),
                entry_chunk_id: Uuid::parse_str(&entry_chunk_id)
                    .context("bad entry chunk pg_id in Flow node")?,
                display_name: row.get("name").unwrap_or_default(),
                entry_type: row.get("entry_type").unwrap_or_default(),
                repo_name: repo_name.to_string(),
                step_count: step_count as usize,
                truncated: row.get("truncated").unwrap_or(false),
                steps,
            });
        }

        // ── Communities + HAS_MEMBER edges ──────────────────────────────
        let rows = self
            .graph
            .query(
                query(
                    "MATCH (c:Community {repo_name: $repo}) \
                     RETURN c.pg_id AS pg_id, c.level AS level, c.member_count AS member_count",
                )
                .param("repo", repo_name),
            )
            .await
            .context("Failed to export Community nodes")?;

        let mut communities = Vec::new();
        for row in rows {
            let pg_id: String = row.get("pg_id").unwrap_or_default();
            let level: i64 = row.get("level").unwrap_or(0);
            let member_count: i64 = row.get("member_count").unwrap_or(0);

            let member_rows = self
                .graph
                .query(
                    query(
                        "MATCH (c:Community {pg_id: $cid})-[:HAS_MEMBER]->(m) \
                         RETURN m.pg_id AS member_pg_id, labels(m)[0] AS member_label",
                    )
                    .param("cid", pg_id.as_str()),
                )
                .await
                .with_context(|| {
                    format!("Failed to export HAS_MEMBER edges for community {pg_id}")
                })?;

            let mut members = Vec::new();
            for member_row in member_rows {
                let member_pg_id: String = member_row.get("member_pg_id").unwrap_or_default();
                let member_label: String = member_row.get("member_label").unwrap_or_default();
                members.push((
                    Uuid::parse_str(&member_pg_id)
                        .context("bad member pg_id in HAS_MEMBER edge")?,
                    member_label,
                ));
            }

            communities.push(GraphCommunitySnapshot {
                community_id: Uuid::parse_str(&pg_id).context("bad pg_id in Community node")?,
                level: level as u8,
                member_count: member_count as usize,
                members,
            });
        }

        Ok(GraphRepoSnapshot {
            chunk_nodes,
            module_nodes,
            calls_edges,
            reference_edges,
            implements_edges,
            routes_to_edges,
            http_call_edges,
            chunk_tags,
            symbol_import_edges,
            module_import_edges,
            flows,
            communities,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use akashic_domain::ports::ChunkGraphRepo;

    /// `Neo4jPool` exposes only `connect(cfg: &Config)` (verified against
    /// `lib.rs` — no `connect_raw`/bare-URI constructor exists), so the live
    /// test builds a minimal `Config` inline, mirroring the same pattern
    /// `akashic-ingestion::ingestion::e2e_test::test_config` uses. Pulling in
    /// `akashic-test-support::test_config_minimal` instead is not an option:
    /// that crate already depends on `akashic-store-neo4j`, so adding it here
    /// (even as a dev-dependency) would be a cross-crate cycle.
    fn test_config() -> akashic_config::Config {
        use akashic_config::{
            AiProvider, AlertsConfig, Config, EmbeddingConfig, EmbeddingPrecision, LlmConfig,
        };
        use secrecy::SecretString;

        Config {
            neo4j_uri: test_neo4j_uri(),
            neo4j_user: "neo4j".into(),
            neo4j_password: SecretString::from(test_neo4j_password()),
            database_url: "postgres://unused@localhost/unused".into(),
            embedding: EmbeddingConfig {
                provider: AiProvider::Local,
                api_key: None,
                model: "fake".into(),
                base_url: None,
            },
            llm: LlmConfig {
                provider: AiProvider::Local,
                api_key: None,
                model: "fake".into(),
                base_url: None,
            },
            alerts: AlertsConfig::default(),
            module_max_files: 1000,
            module_min_files: 1,
            mcp_sse_host: "127.0.0.1".into(),
            mcp_sse_port: 8080,
            gitlab_webhook_secret: None,
            gitlab_url: "http://unused".into(),
            gitlab_app_id: "unused".into(),
            gitlab_app_secret: SecretString::from("unused".to_string()),
            gitlab_redirect_uri: "http://unused/callback".into(),
            gitlab_web_redirect_uri: "http://unused/web/callback".into(),
            auth_code_ttl_secs: 300,
            api_key_ttl_secs: 86400,
            gitlab_service_token: None,
            frontend_url: "http://unused".into(),
            public_base_url: "http://unused".into(),
            cors_extra_origins: vec![],
            cookie_secure: false,
            api_port: 8081,
            ingest_clone_dir: "/tmp/akashic-ingest-test".into(),
            ingest_max_file_size: 102_400,
            ingest_max_lines: 2000,
            ingest_chunk_max_size: 5120,
            ingest_concurrent_jobs: 1,
            ingest_skip_patterns: vec![],
            ingest_presets_path: None,
            ingest_completeness_threshold: 1.0,
            admin_users: vec![],
            ingest_crawl_max_pages: 100,
            ingest_crawl_delay_ms: 200,
            embedding_precision: EmbeddingPrecision::Float32,
            rate_limit_enabled: false,
            rate_limit_trusted_proxies: vec![],
            rate_limit_allowlist: vec![],
            mcp_quota_tokens_per_window: 1000,
            mcp_quota_window_secs: 60,
            mcp_quota_enabled: false,
            mcp_passthrough_user_cache_ttl_secs: 60,
            oauth_validation_mode: "disabled".into(),
            migrate_on_boot: "off".into(),
            ingest_quota_tokens_per_window: 1000,
            ingest_quota_window_secs: 60,
            ingest_quota_enabled: false,
        }
    }

    fn test_neo4j_uri() -> String {
        std::env::var("TEST_NEO4J_URI").unwrap_or_else(|_| "bolt://localhost:7687".to_string())
    }
    fn test_neo4j_password() -> String {
        std::env::var("TEST_NEO4J_PASSWORD").unwrap_or_else(|_| "akashic_secret".to_string())
    }

    async fn connect() -> Neo4jPool {
        Neo4jPool::connect(&test_config())
            .await
            .expect("connect to test Neo4j")
    }

    #[tokio::test]
    #[ignore = "requires live Neo4j; run with --ignored"]
    async fn export_reads_back_chunk_nodes_and_calls_edge() {
        let graph = connect().await;
        let repo = "e2e-snapshot-neo4j-task3";

        graph
            .execute(
                query("MATCH (c:Chunk {repo_name: $repo}) DETACH DELETE c").param("repo", repo),
            )
            .await
            .unwrap();

        let chunk_repo = crate::repos::chunk_graph::Neo4jChunkGraphRepo::new(graph.clone());
        let src_id = Uuid::new_v4().to_string();
        let tgt_id = Uuid::new_v4().to_string();
        chunk_repo
            .create_chunk_nodes_batch(
                repo,
                &[
                    ChunkGraphNode {
                        pg_id: src_id.clone(),
                        name: "caller".into(),
                        chunk_type: "function".into(),
                        module_path: "src/lib.rs".into(),
                        fqn: None,
                        parent_fqn: None,
                        start_line: 1,
                        end_line: 2,
                        visibility: "public".into(),
                        is_async: false,
                        is_static: false,
                        is_exported: true,
                        http_method: None,
                        http_path: None,
                    },
                    ChunkGraphNode {
                        pg_id: tgt_id.clone(),
                        name: "callee".into(),
                        chunk_type: "function".into(),
                        module_path: "src/lib.rs".into(),
                        fqn: None,
                        parent_fqn: None,
                        start_line: 3,
                        end_line: 4,
                        visibility: "public".into(),
                        is_async: false,
                        is_static: false,
                        is_exported: true,
                        http_method: None,
                        http_path: None,
                    },
                ],
            )
            .await
            .unwrap();

        graph
            .execute(
                query(
                    "MATCH (src:Chunk {pg_id: $src}) MATCH (tgt:Chunk {pg_id: $tgt}) \
                     MERGE (src)-[r:CALLS]->(tgt) SET r.confidence = 0.9, r.method = 'direct'",
                )
                .param("src", src_id.as_str())
                .param("tgt", tgt_id.as_str()),
            )
            .await
            .unwrap();

        let snapshot_repo = Neo4jSnapshotRepo::new(graph.clone());
        let snapshot = snapshot_repo.export_repo_snapshot(repo).await.unwrap();

        assert_eq!(snapshot.chunk_nodes.len(), 2);
        assert_eq!(snapshot.calls_edges.len(), 1);
        assert_eq!(snapshot.calls_edges[0].method, "direct");
        assert_eq!(snapshot.calls_edges[0].ref_kind, None);

        graph
            .execute(
                query("MATCH (c:Chunk {repo_name: $repo}) DETACH DELETE c").param("repo", repo),
            )
            .await
            .unwrap();
    }

    /// Regression test for the property-name divergence found while
    /// cross-checking `ingest_edge.rs`: `IMPLEMENTS`/`ROUTES_TO` store their
    /// "kind" tag under `impl_kind`/`http_method`, NOT `ref_kind` like
    /// `REFERENCES` does. A naive export using `e.ref_kind` uniformly for
    /// every edge type would silently read back `None` for these two.
    #[tokio::test]
    #[ignore = "requires live Neo4j; run with --ignored"]
    async fn export_reads_back_implements_and_routes_to_kind_from_their_real_property_names() {
        let graph = connect().await;
        let repo = "e2e-snapshot-neo4j-task3-kinds";

        graph
            .execute(
                query("MATCH (c:Chunk {repo_name: $repo}) DETACH DELETE c").param("repo", repo),
            )
            .await
            .unwrap();

        let chunk_repo = crate::repos::chunk_graph::Neo4jChunkGraphRepo::new(graph.clone());
        let a = Uuid::new_v4().to_string();
        let b = Uuid::new_v4().to_string();
        let c = Uuid::new_v4().to_string();
        let make_node = |pg_id: String, name: &str| ChunkGraphNode {
            pg_id,
            name: name.into(),
            chunk_type: "function".into(),
            module_path: "src/lib.rs".into(),
            fqn: None,
            parent_fqn: None,
            start_line: 1,
            end_line: 2,
            visibility: "public".into(),
            is_async: false,
            is_static: false,
            is_exported: true,
            http_method: None,
            http_path: None,
        };
        chunk_repo
            .create_chunk_nodes_batch(
                repo,
                &[
                    make_node(a.clone(), "impl_src"),
                    make_node(b.clone(), "impl_tgt"),
                    make_node(c.clone(), "route_tgt"),
                ],
            )
            .await
            .unwrap();

        graph
            .execute(
                query(
                    "MATCH (src:Chunk {pg_id: $a}) MATCH (tgt:Chunk {pg_id: $b}) \
                     MERGE (src)-[r:IMPLEMENTS {impl_kind: 'trait_impl'}]->(tgt) \
                     SET r.confidence = 0.8, r.method = 'static'",
                )
                .param("a", a.as_str())
                .param("b", b.as_str()),
            )
            .await
            .unwrap();

        graph
            .execute(
                query(
                    "MATCH (src:Chunk {pg_id: $a}) MATCH (tgt:Chunk {pg_id: $c}) \
                     MERGE (src)-[r:ROUTES_TO {http_method: 'GET'}]->(tgt) \
                     SET r.confidence = 0.7, r.method = 'axum'",
                )
                .param("a", a.as_str())
                .param("c", c.as_str()),
            )
            .await
            .unwrap();

        let snapshot_repo = Neo4jSnapshotRepo::new(graph.clone());
        let snapshot = snapshot_repo.export_repo_snapshot(repo).await.unwrap();

        assert_eq!(snapshot.implements_edges.len(), 1);
        assert_eq!(
            snapshot.implements_edges[0].ref_kind,
            Some("trait_impl".to_string())
        );
        assert_eq!(snapshot.routes_to_edges.len(), 1);
        assert_eq!(
            snapshot.routes_to_edges[0].ref_kind,
            Some("GET".to_string())
        );

        graph
            .execute(
                query("MATCH (c:Chunk {repo_name: $repo}) DETACH DELETE c").param("repo", repo),
            )
            .await
            .unwrap();
    }

    /// Regression test for the whole-slice review finding: `export_repo_snapshot`
    /// silently omitted `:Tag` nodes / `[:TAGGED_WITH]` edges, even though Stage 4
    /// (`akashic-ingestion::ingestion::store.rs`) writes at least one tag for
    /// EVERY chunk via `ChunkGraphRepo::create_tagged_with_edges`. Seeds tags
    /// through that same existing write port (mirrors how the other tests in
    /// this file seed via `Neo4jChunkGraphRepo::create_chunk_nodes_batch`
    /// before exporting), then proves `GraphRepoSnapshot.chunk_tags` round-trips
    /// the `(chunk_pg_id, tag_name)` pairs correctly.
    #[tokio::test]
    #[ignore = "requires live Neo4j; run with --ignored"]
    async fn export_reads_back_chunk_tags() {
        let graph = connect().await;
        let repo = "e2e-snapshot-neo4j-task-final-tags";

        graph
            .execute(
                query("MATCH (c:Chunk {repo_name: $repo}) DETACH DELETE c").param("repo", repo),
            )
            .await
            .unwrap();

        let chunk_repo = crate::repos::chunk_graph::Neo4jChunkGraphRepo::new(graph.clone());
        let a = Uuid::new_v4().to_string();
        let b = Uuid::new_v4().to_string();
        chunk_repo
            .create_chunk_nodes_batch(
                repo,
                &[
                    ChunkGraphNode {
                        pg_id: a.clone(),
                        name: "get_userName".into(),
                        chunk_type: "function".into(),
                        module_path: "src/lib.rs".into(),
                        fqn: None,
                        parent_fqn: None,
                        start_line: 1,
                        end_line: 2,
                        visibility: "public".into(),
                        is_async: false,
                        is_static: false,
                        is_exported: true,
                        http_method: None,
                        http_path: None,
                    },
                    ChunkGraphNode {
                        pg_id: b.clone(),
                        name: "render".into(),
                        chunk_type: "render".into(),
                        module_path: "src/lib.rs".into(),
                        fqn: None,
                        parent_fqn: None,
                        start_line: 3,
                        end_line: 4,
                        visibility: "public".into(),
                        is_async: false,
                        is_static: false,
                        is_exported: true,
                        http_method: None,
                        http_path: None,
                    },
                ],
            )
            .await
            .unwrap();

        // Two tags on chunk `a` (mirrors extract_chunk_tags's real output for a
        // camelCase name like "get_userName" — split-word tags plus the
        // chunk_type tag), one tag on chunk `b`.
        chunk_repo
            .create_tagged_with_edges(
                vec![a.clone(), a.clone(), b.clone()],
                vec![
                    "function".to_string(),
                    "user".to_string(),
                    "render".to_string(),
                ],
            )
            .await
            .unwrap();

        let snapshot_repo = Neo4jSnapshotRepo::new(graph.clone());
        let snapshot = snapshot_repo.export_repo_snapshot(repo).await.unwrap();

        assert_eq!(snapshot.chunk_tags.len(), 3);
        let mut got = snapshot.chunk_tags.clone();
        got.sort();
        let mut want = vec![
            (a.clone(), "function".to_string()),
            (a.clone(), "user".to_string()),
            (b.clone(), "render".to_string()),
        ];
        want.sort();
        assert_eq!(got, want);

        graph
            .execute(
                query("MATCH (c:Chunk {repo_name: $repo}) DETACH DELETE c").param("repo", repo),
            )
            .await
            .unwrap();
    }
}
