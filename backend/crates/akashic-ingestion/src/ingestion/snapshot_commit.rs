//! Atomic commit of an `IngestAccumulator` into Postgres + Neo4j (Roadmap F).
//!
//! `import_graph_snapshot` is HOISTED from the `akashic-ingest` CLI binary
//! (Roadmap E2 Task 4, extended by E2's final-review Tag/TAGGED_WITH fix) —
//! it is byte-identical to that function, just moved into the lib so both
//! the CLI's `import` subcommand AND the RAM-first pipeline's commit gate
//! call the SAME code, rather than maintaining two copies of "reconstruct a
//! Neo4j graph from a GraphRepoSnapshot by replaying the existing write
//! ports." This also closes a Minor finding from E2's final whole-slice
//! review, which flagged exactly this duplication risk.

use akashic_domain::ports::{
    ChunkGraphRepo, CommunityGraphRepo, FlowGraphRepo, IngestEdgeRepo, ModuleGraphRepo,
    RepoGraphRepo, SnapshotPgRepo,
};
use akashic_domain::types::{GraphRepoSnapshot, PgRepoSnapshot};
use akashic_store_neo4j::Neo4jPool;
use akashic_store_neo4j::repos::chunk_graph::Neo4jChunkGraphRepo;
use akashic_store_neo4j::repos::community_graph::Neo4jCommunityGraphRepo;
use akashic_store_neo4j::repos::ingest_edge::{
    Neo4jFlowGraphRepo, Neo4jIngestEdgeRepo, Neo4jRepoGraphRepo,
};
use akashic_store_neo4j::repos::module_graph::Neo4jModuleGraphRepo;
use akashic_store_pg::repos::snapshot::PgSnapshotRepo;

/// Rebuilds the Neo4j graph from an exported/accumulated snapshot by calling
/// the SAME write ports the ingestion pipeline itself calls (Stages 5/6/7/9)
/// — no new Neo4j write surface, since those already MERGE/CREATE by
/// explicit `pg_id`. HOISTED VERBATIM from `bin/akashic-ingest.rs` (Roadmap
/// E2 Task 4, extended by E2's final-review Tag/TAGGED_WITH fix) — do not let
/// this drift from that copy; the bin's call site now calls this function
/// instead of its own (deleted) local copy.
///
/// `pub` (not `pub(crate)`): the `akashic-ingest` binary is a SEPARATE
/// compilation unit from this lib crate even though they share a Cargo
/// package, so `pub(crate)` would not resolve across that boundary — this
/// must be externally visible for `bin/akashic-ingest.rs` to call it as
/// `akashic_ingestion::ingestion::snapshot_commit::import_graph_snapshot`.
pub async fn import_graph_snapshot(
    neo4j: &Neo4jPool,
    repo_name: &str,
    snapshot: &GraphRepoSnapshot,
) -> anyhow::Result<()> {
    let repo_graph = Neo4jRepoGraphRepo::new(neo4j.clone());
    repo_graph.ensure_repository_node(repo_name).await?;

    let module_graph = Neo4jModuleGraphRepo::new(neo4j.clone());
    for (pg_id, path) in &snapshot.module_nodes {
        let module_pg_id = uuid::Uuid::parse_str(pg_id)?;
        module_graph
            .upsert_module_node(repo_name, module_pg_id, path)
            .await?;
        module_graph
            .create_has_module_edge(repo_name, module_pg_id)
            .await?;
    }

    let chunk_graph = Neo4jChunkGraphRepo::new(neo4j.clone());
    chunk_graph
        .create_chunk_nodes_batch(repo_name, &snapshot.chunk_nodes)
        .await?;

    let mut chunks_by_module: std::collections::HashMap<String, Vec<uuid::Uuid>> =
        std::collections::HashMap::new();
    for c in &snapshot.chunk_nodes {
        let id = uuid::Uuid::parse_str(&c.pg_id)?;
        chunks_by_module
            .entry(c.module_path.clone())
            .or_default()
            .push(id);
    }
    for (pg_id, path) in &snapshot.module_nodes {
        let module_pg_id = uuid::Uuid::parse_str(pg_id)?;
        if let Some(chunk_ids) = chunks_by_module.get(path) {
            module_graph
                .create_has_chunk_edges(module_pg_id, chunk_ids)
                .await?;
        }
    }

    let ingest_edges = Neo4jIngestEdgeRepo::new(neo4j.clone());
    ingest_edges
        .create_call_edges(repo_name, &snapshot.calls_edges)
        .await?;
    ingest_edges
        .create_reference_edges(repo_name, &snapshot.reference_edges)
        .await?;
    ingest_edges
        .create_implements_edges(repo_name, &snapshot.implements_edges)
        .await?;
    ingest_edges
        .create_routes_to_edges(repo_name, &snapshot.routes_to_edges)
        .await?;
    ingest_edges
        .create_makes_http_call_edges(repo_name, &snapshot.http_call_edges)
        .await?;
    ingest_edges
        .create_symbol_import_edges(repo_name, &snapshot.symbol_import_edges)
        .await?;

    let (src_ids, tgt_ids): (Vec<String>, Vec<String>) =
        snapshot.module_import_edges.iter().cloned().unzip();
    ingest_edges.create_import_edges(&src_ids, &tgt_ids).await?;

    if !snapshot.chunk_tags.is_empty() {
        let (tag_pg_ids, tag_names): (Vec<String>, Vec<String>) =
            snapshot.chunk_tags.iter().cloned().unzip();
        chunk_graph
            .create_tagged_with_edges(tag_pg_ids, tag_names)
            .await?;
    }

    let flow_graph = Neo4jFlowGraphRepo::new(neo4j.clone());
    flow_graph.store_flows(repo_name, &snapshot.flows).await?;

    let community_graph = Neo4jCommunityGraphRepo::new(neo4j.clone());
    for c in &snapshot.communities {
        community_graph
            .create_community_node(c.community_id, c.level, repo_name, c.member_count)
            .await?;
        for (member_pg_id, member_label) in &c.members {
            community_graph
                .create_has_member_edge(c.community_id, *member_pg_id, member_label)
                .await?;
        }
    }

    Ok(())
}

/// Stamp `ingested_at` on every accumulated module/chunk/large_chunk row
/// (nothing has a real timestamp yet — Task 2/3's resolver deliberately left
/// these `None`, since "ingested" means "landed," and nothing lands until
/// THIS function runs), then commit the accumulator into Postgres + Neo4j.
///
/// `preserve_existing` selects the commit strategy — the ONE place the two
/// ingest paths diverge (Roadmap F Task 9.5, which folded the old standalone
/// `clean_old_data` commit-gate call into here so the wipe and the insert
/// share a transaction):
///
/// * **Production (`preserve_existing == false`)** — the repo is REPLACED:
///   `import_repo_snapshot_replacing` deletes the repo's existing PG rows and
///   inserts the new snapshot in ONE transaction (atomic wipe+insert; a crash
///   before that tx commits rolls back to the intact old graph), and THEN —
///   strictly after the PG tx is durable — the Neo4j nodes for the repo are
///   wiped and the graph replayed. The only reachable partial state is
///   therefore "PG-complete + Neo4j-partial", which self-heals on the next
///   ingest.
/// * **`run_sync` (`preserve_existing == true`)** — NOTHING is wiped: the
///   CLI's repeated-measurement path relies on Stage 4's content-match skip,
///   so it keeps byte-matched rows that were never re-staged into `acc`. Only
///   the touched module rows are delete-then-reinserted by id (see below), via
///   the no-wipe `import_repo_snapshot`; Neo4j is left un-wiped because its
///   node MERGEs are idempotent and its edge creators already do repo-scoped
///   delete-then-recreate.
///
/// No communities/community_members are stamped or committed here — Stage 9
/// runs AFTER this commit (it needs a durable graph to read) and writes its
/// own rows through its own existing, unchanged path.
///
/// Returns the stamped `PgRepoSnapshot` (the same data that was just
/// committed) so a caller can report post-commit counts without a second
/// query.
pub(crate) async fn commit_snapshot(
    pg: &sqlx::PgPool,
    neo4j: &Neo4jPool,
    repo_name: &str,
    preserve_existing: bool,
    mut acc: crate::ingestion::accumulator::IngestAccumulator,
) -> anyhow::Result<PgRepoSnapshot> {
    let now = chrono::Utc::now();
    for m in &mut acc.pg.modules {
        m.ingested_at = Some(now);
    }
    for c in &mut acc.pg.chunks {
        c.ingested_at = Some(now);
    }
    for lc in &mut acc.pg.large_chunks {
        lc.ingested_at = Some(now);
    }

    let pg_snapshot_repo = PgSnapshotRepo::new(pg.clone());

    if preserve_existing {
        // ── run_sync path — UNCHANGED from before Task 9.5 ────────────────
        //
        // `import_repo_snapshot` does a plain explicit-id `INSERT` (no
        // `ON CONFLICT`) and its own contract requires the target to have no
        // existing row for the ids it inserts. `run_sync`
        // (`preserve_existing=true`) deliberately does NOT wipe (see its doc
        // comment) — but once a module's CONTENT changes on a re-ingest,
        // Stage 4 reuses that module's EXISTING Postgres id (see
        // `stage4_embed_store`'s judgment-call comment on module id stability,
        // needed so `EdgeRepo::merge_explains_to_module` /
        // `GraphReadRepo::get_module_note_ids` don't get orphaned), so
        // `acc.pg.modules` can contain a row whose `id` ALREADY has a row in
        // the `modules` table. A blind `INSERT` would violate the primary key.
        // Chunks/large_chunks never have this problem — `resolve_chunks` only
        // ever mints a BRAND NEW `Uuid::new_v4()` for a row it stages (a
        // byte-matched, reused-id chunk is never staged into `acc` at all), so
        // every id `import_repo_snapshot` inserts for those two tables is
        // guaranteed never to already exist. Delete-then-reinsert-by-id for
        // JUST the touched module rows is a safe, lossless "replace" (same id,
        // fresh content) — NOT a repo-wide wipe, which is exactly why it must
        // NOT delete byte-matched rows that live only in the DB (never in
        // `acc`). `e2e_run_sync_preserves_existing_chunks_across_calls` and
        // `e2e_run_sync_no_duplicate_chunks_after_second_call` guard this.
        //
        // This delete runs outside `import_repo_snapshot`'s transaction — a
        // crash between it and the subsequent insert could transiently lose a
        // module row, matching `clean_old_data`'s own documented non-atomicity
        // trade-off (idempotent, repo-scoped statements that self-heal on the
        // next ingest) rather than introducing a new risk class. The
        // production path does NOT take this branch; it uses the atomic
        // `import_repo_snapshot_replacing` below.
        if !acc.pg.modules.is_empty() {
            let module_ids: Vec<uuid::Uuid> = acc.pg.modules.iter().map(|m| m.id).collect();

            // Review finding 1 (Task 7 fix-up): `community_members.module_id`
            // FK-references `modules(id)` with NO `ON DELETE CASCADE`, so if a
            // module already has a `community_members` row (from a prior Stage 9
            // run) the module DELETE below would FK-violate. Delete the
            // dependent `community_members` rows FIRST. Lossless in context —
            // Stage 9 (which runs after this commit) recomputes fresh community
            // structure from the new graph state on its next run.
            sqlx::query("DELETE FROM community_members WHERE module_id = ANY($1)")
                .bind(&module_ids)
                .execute(pg)
                .await?;

            sqlx::query("DELETE FROM modules WHERE id = ANY($1)")
                .bind(&module_ids)
                .execute(pg)
                .await?;
        }

        pg_snapshot_repo
            .import_repo_snapshot(repo_name, &acc.pg)
            .await?;

        // Neo4j: NO wipe on this path. Its node MERGEs are idempotent and its
        // edge creators already do repo-scoped delete-then-recreate, so a wipe
        // here would delete byte-matched Chunk nodes that aren't in `acc` —
        // the same data-loss class the delete above is careful to avoid.
        import_graph_snapshot(neo4j, repo_name, &acc.graph).await?;
    } else {
        // ── Production path — atomic PG replace, THEN Neo4j replace ────────
        //
        // ONE transaction deletes the repo's existing PG rows (FK-safe order)
        // and inserts the new snapshot. A crash/error before that tx commits
        // rolls the wipe back, leaving the OLD graph fully intact — the whole
        // point of Task 9.5 (the previous standalone `clean_old_data` at the
        // commit gate wiped PG non-transactionally BEFORE the insert, so a
        // crash in that window destroyed the prior graph).
        pg_snapshot_repo
            .import_repo_snapshot_replacing(repo_name, &acc.pg)
            .await?;

        // Neo4j wipe of this repo's Community/Chunk/Module nodes (same
        // repo-scoped grouping as the PG wipe above). Runs STRICTLY AFTER the
        // PG tx is durable, so the sole reachable partial state is
        // "PG-complete + Neo4j-partial" — self-healing on the next ingest.
        // (Neo4j and PG are separate stores; no cross-store transaction is
        // possible, so this residual window is inherent and acknowledged, not
        // a regression.)
        Neo4jCommunityGraphRepo::new(neo4j.clone())
            .delete_communities_by_repo(repo_name)
            .await?;
        Neo4jChunkGraphRepo::new(neo4j.clone())
            .delete_chunks_by_repo(repo_name)
            .await?;
        Neo4jModuleGraphRepo::new(neo4j.clone())
            .delete_modules_by_repo(repo_name)
            .await?;

        import_graph_snapshot(neo4j, repo_name, &acc.graph).await?;
    }

    Ok(acc.pg)
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use uuid::Uuid;

    use akashic_domain::ports::ChunkGraphNode;
    use akashic_domain::types::{ChunkSnapshotRow, ModuleSnapshotRow};
    use akashic_store_neo4j::Neo4jPool;

    use crate::ingestion::accumulator::IngestAccumulator;
    use crate::ingestion::snapshot_commit::commit_snapshot;

    /// Width of the live `chunks.embedding` column (`vector(1536)`).
    const DIM: usize = 1536;

    const REPO: &str = "roadmap-f-commit-snapshot-test";

    /// Copy of `snapshot_e2e_test.rs::test_config` (module-private there, not
    /// reusable across sibling test modules — same re-declare-rather-than-
    /// widen-visibility idiom already established in this crate, e.g.
    /// `accumulator.rs`'s `resolve_chunks_tests`).
    fn test_config(database_url: &str, neo4j_uri: &str) -> akashic_config::Config {
        use akashic_config::{
            AiProvider, AlertsConfig, Config, EmbeddingConfig, EmbeddingPrecision, LlmConfig,
        };
        Config {
            neo4j_uri: neo4j_uri.to_string(),
            neo4j_user: std::env::var("TEST_NEO4J_USER").unwrap_or_else(|_| "neo4j".into()),
            neo4j_password: SecretString::from(
                std::env::var("TEST_NEO4J_PASSWORD").unwrap_or_else(|_| "changeme".into()),
            ),
            database_url: database_url.to_string(),
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
            module_max_files: 1000,
            module_min_files: 1,
            api_host: "127.0.0.1".into(),
            gitlab_webhook_secret: None,
            gitlab_url: "http://localhost".into(),
            gitlab_app_id: String::new(),
            gitlab_app_secret: SecretString::from(String::new()),
            gitlab_redirect_uri: "http://localhost/cb".into(),
            gitlab_web_redirect_uri: "http://localhost/wcb".into(),
            auth_code_ttl_secs: 300,
            api_key_ttl_secs: 86400,
            gitlab_service_token: None,
            frontend_url: "http://localhost:3000".into(),
            cors_extra_origins: vec![],
            cookie_secure: false,
            api_port: 8081,
            ingest_clone_dir: std::env::temp_dir()
                .join("akashic-commit-snapshot-test-clone")
                .to_string_lossy()
                .into_owned(),
            ingest_max_file_size: 1_048_576,
            ingest_max_lines: 5000,
            ingest_chunk_max_size: 5120,
            ingest_concurrent_jobs: 1,
            ingest_skip_patterns: vec!["node_modules".into(), ".git".into(), "target".into()],
            ingest_presets_path: None,
            ingest_completeness_threshold: 1.0,
            admin_users: vec![],
            ingest_crawl_max_pages: 10,
            ingest_crawl_delay_ms: 0,
            embedding_precision: EmbeddingPrecision::Float32,
            rate_limit_enabled: false,
            rate_limit_trusted_proxies: vec![],
            rate_limit_allowlist: vec![],
            alerts: AlertsConfig::default(),
            public_base_url: "http://127.0.0.1:0".to_string(),
            mcp_quota_tokens_per_window: 100_000,
            mcp_quota_window_secs: 3600,
            mcp_quota_enabled: false,
            mcp_passthrough_user_cache_ttl_secs: 60,
            oauth_validation_mode: "off".to_string(),
            migrate_on_boot: "false".to_string(),
            ingest_quota_tokens_per_window: 5_000_000,
            ingest_quota_window_secs: 3600,
            ingest_quota_enabled: false,
            mcp_cimd_allow_loopback: false,
        }
    }

    async fn connect_and_init_schema() -> (sqlx::PgPool, Neo4jPool) {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://akashic@localhost:5432/akashic".into());
        let neo4j_uri =
            std::env::var("TEST_NEO4J_URI").unwrap_or_else(|_| "bolt://localhost:7687".into());
        let cfg = test_config(&database_url, &neo4j_uri);

        let pg = akashic_store_pg::connect(&database_url)
            .await
            .expect("connect Postgres");
        let neo4j = Neo4jPool::connect(&cfg).await.expect("connect Neo4j");

        let vec_type = cfg.vector_type(DIM);
        let cos_ops = cfg.cosine_ops();
        akashic_store_pg::init_schema(&pg, DIM, &vec_type, cos_ops)
            .await
            .expect("pg init_schema");
        akashic_store_neo4j::schema::init_schema(&neo4j, DIM)
            .await
            .expect("neo4j init_schema");

        (pg, neo4j)
    }

    async fn clean_repo(pg: &sqlx::PgPool, neo4j: &Neo4jPool, repo: &str) {
        // Communities/community_members FIRST: `community_members` has no
        // `ON DELETE CASCADE` back to `modules`/`chunks`, so leftover rows
        // from the module-id-reuse test below would otherwise block the
        // chunk/module deletes right below this.
        sqlx::query(
            "DELETE FROM community_members WHERE \
             community_id IN (SELECT id FROM communities WHERE repo_name = $1) \
             OR module_id IN (SELECT id FROM modules WHERE repo_name = $1)",
        )
        .bind(repo)
        .execute(pg)
        .await
        .unwrap();
        sqlx::query("DELETE FROM communities WHERE repo_name = $1")
            .bind(repo)
            .execute(pg)
            .await
            .unwrap();

        sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
            .bind(repo)
            .execute(pg)
            .await
            .unwrap();
        sqlx::query("DELETE FROM modules WHERE repo_name = $1")
            .bind(repo)
            .execute(pg)
            .await
            .unwrap();

        use neo4rs::query;
        neo4j
            .execute(
                query("MATCH (c:Chunk {repo_name: $repo}) DETACH DELETE c").param("repo", repo),
            )
            .await
            .unwrap();
        neo4j
            .execute(
                query("MATCH (m:Module {repo_name: $repo}) DETACH DELETE m").param("repo", repo),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    #[ignore = "requires live Postgres + Neo4j; run with --ignored"]
    async fn commit_snapshot_writes_accumulator_state_atomically() {
        let (pg, neo4j) = connect_and_init_schema().await;
        clean_repo(&pg, &neo4j, REPO).await;

        let module_id = Uuid::new_v4();
        let chunk_id = Uuid::new_v4();
        let module_path = "src/lib.rs".to_string();

        let mut acc = IngestAccumulator::new();
        acc.pg.modules.push(ModuleSnapshotRow {
            id: module_id,
            path: module_path.clone(),
            language: Some("rust".into()),
            summary: Some("a test module for commit_snapshot".into()),
            exports_count: Some(1),
            file_count: Some(1),
            is_virtual: false,
            embedding: None,
            git_ref: Some("main".into()),
            ingested_at: None,
        });
        acc.pg.chunks.push(ChunkSnapshotRow {
            id: chunk_id,
            module_path: module_path.clone(),
            chunk_type: "function".into(),
            name: "test_fn".into(),
            signature: None,
            content: "fn test_fn() {}".into(),
            language: Some("rust".into()),
            embedding: vec![0.001; DIM],
            signature_embedding: None,
            git_ref: Some("main".into()),
            fqn: Some("test_fn".into()),
            parent_fqn: None,
            start_line: Some(1),
            end_line: Some(1),
            visibility: Some("public".into()),
            is_async: false,
            is_static: false,
            is_exported: true,
            doc: None,
            ingested_at: None,
        });
        acc.graph
            .module_nodes
            .push((module_id.to_string(), module_path.clone()));
        acc.graph.chunk_nodes.push(ChunkGraphNode {
            pg_id: chunk_id.to_string(),
            name: "test_fn".into(),
            chunk_type: "function".into(),
            module_path: module_path.clone(),
            fqn: Some("test_fn".into()),
            parent_fqn: None,
            start_line: 1,
            end_line: 1,
            visibility: "public".into(),
            is_async: false,
            is_static: false,
            is_exported: true,
            http_method: None,
            http_path: None,
        });

        let before = chrono::Utc::now();
        // `preserve_existing = true`: exercises the no-wipe path (the exact
        // behavior `commit_snapshot` had unconditionally before Task 9.5). The
        // repo is pre-cleaned by `clean_repo`, so no rows are wiped either way.
        let committed = commit_snapshot(&pg, &neo4j, REPO, true, acc)
            .await
            .expect("commit_snapshot must succeed");
        let after = chrono::Utc::now();

        // ── Returned snapshot proves stamping happened in-memory ──────────
        assert_eq!(committed.modules.len(), 1);
        assert_eq!(committed.chunks.len(), 1);
        let module_stamp = committed.modules[0]
            .ingested_at
            .expect("module ingested_at must be stamped, not None");
        let chunk_stamp = committed.chunks[0]
            .ingested_at
            .expect("chunk ingested_at must be stamped, not None");
        assert!(
            module_stamp >= before && module_stamp <= after,
            "module ingested_at must be a real, recent timestamp"
        );
        assert!(
            chunk_stamp >= before && chunk_stamp <= after,
            "chunk ingested_at must be a real, recent timestamp"
        );

        // ── Directly against Postgres: the row really landed, stamped ────
        let (pg_id, pg_ingested_at): (Uuid, Option<chrono::DateTime<chrono::Utc>>) =
            sqlx::query_as("SELECT id, ingested_at FROM chunks WHERE id = $1")
                .bind(chunk_id)
                .fetch_one(&pg)
                .await
                .expect("chunk row must exist in Postgres after commit_snapshot");
        assert_eq!(pg_id, chunk_id);
        assert!(
            pg_ingested_at.is_some(),
            "persisted chunk row's ingested_at must not be NULL"
        );

        let (module_pg_id,): (Uuid,) = sqlx::query_as("SELECT id FROM modules WHERE id = $1")
            .bind(module_id)
            .fetch_one(&pg)
            .await
            .expect("module row must exist in Postgres after commit_snapshot");
        assert_eq!(module_pg_id, module_id);

        // ── Directly against Neo4j: the Chunk node exists with the same pg_id ──
        use neo4rs::query;
        let rows = neo4j
            .query(
                query("MATCH (c:Chunk {pg_id: $pg_id}) RETURN c.pg_id AS pg_id")
                    .param("pg_id", chunk_id.to_string()),
            )
            .await
            .expect("Neo4j query for Chunk node must succeed");
        assert_eq!(
            rows.len(),
            1,
            "exactly one Neo4j Chunk node must exist with this pg_id"
        );
        let found_pg_id: String = rows[0].get("pg_id").unwrap_or_default();
        assert_eq!(found_pg_id, chunk_id.to_string());

        clean_repo(&pg, &neo4j, REPO).await;
    }

    /// Review finding 1/2 (Roadmap F Task 7 fix-up): the module-id-reuse
    /// delete path (`DELETE FROM modules WHERE id = ANY($1)`, right before
    /// `import_repo_snapshot`) must not FK-violate against a stale
    /// `community_members` row left over from a prior Stage 9 run.
    ///
    /// Seeds a module, commits it once, then — mimicking what Stage 9 does
    /// on a real run — inserts a `community_members` row that references
    /// that module's id directly (no need to run the real community
    /// pipeline; the FK is what matters). A second `commit_snapshot` call
    /// then reuses the SAME module id with CHANGED content (the exact
    /// "module content changed on a re-ingest" case `commit_snapshot`'s own
    /// doc comment describes). Before the fix this panics with a real
    /// Postgres foreign-key-violation error from the `DELETE FROM modules`
    /// statement; after the fix it must succeed AND leave the stale
    /// membership row gone (not orphaned, not pointing at stale content).
    const REPO2: &str = "roadmap-f-commit-snapshot-reuse-test";

    #[tokio::test]
    #[serial_test::serial]
    #[ignore = "requires live Postgres + Neo4j; run with --ignored"]
    async fn commit_snapshot_reuses_module_id_and_clears_stale_community_membership() {
        let (pg, neo4j) = connect_and_init_schema().await;
        clean_repo(&pg, &neo4j, REPO2).await;

        let module_id = Uuid::new_v4();
        let module_path = "src/reuse.rs".to_string();

        // ── First commit: seeds the module with its ORIGINAL content ─────
        let mut acc1 = IngestAccumulator::new();
        acc1.pg.modules.push(ModuleSnapshotRow {
            id: module_id,
            path: module_path.clone(),
            language: Some("rust".into()),
            summary: Some("v1 summary — original content".into()),
            exports_count: Some(1),
            file_count: Some(1),
            is_virtual: false,
            embedding: None,
            git_ref: Some("main".into()),
            ingested_at: None,
        });
        acc1.graph
            .module_nodes
            .push((module_id.to_string(), module_path.clone()));

        // `preserve_existing = true`: this test guards the module-id-reuse
        // delete path, which lives ONLY in the no-wipe branch.
        commit_snapshot(&pg, &neo4j, REPO2, true, acc1)
            .await
            .expect("first (seed) commit_snapshot must succeed");

        // ── Simulate a prior Stage 9 run: a community + a membership row
        // that references this module's id directly, exactly the shape
        // `community_members.module_id UUID REFERENCES modules(id)` (no
        // ON DELETE CASCADE) leaves behind. ────────────────────────────
        let community_id = Uuid::new_v4();
        sqlx::query("INSERT INTO communities (id, repo_name, level) VALUES ($1, $2, $3)")
            .bind(community_id)
            .bind(REPO2)
            .bind(0_i16)
            .execute(&pg)
            .await
            .expect("seed communities row");
        sqlx::query("INSERT INTO community_members (community_id, module_id) VALUES ($1, $2)")
            .bind(community_id)
            .bind(module_id)
            .execute(&pg)
            .await
            .expect("seed stale community_members row referencing the module");

        let (stale_count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM community_members WHERE module_id = $1")
                .bind(module_id)
                .fetch_one(&pg)
                .await
                .expect("count stale community_members before second commit");
        assert_eq!(
            stale_count, 1,
            "sanity: stale community_members row must exist before the reuse commit"
        );

        // ── Second commit: SAME module id, CHANGED content — the delete
        // path this fix-up targets. Must succeed (no FK violation). ─────
        let mut acc2 = IngestAccumulator::new();
        acc2.pg.modules.push(ModuleSnapshotRow {
            id: module_id,
            path: module_path.clone(),
            language: Some("rust".into()),
            summary: Some("v2 summary — content changed on re-ingest".into()),
            exports_count: Some(2),
            file_count: Some(1),
            is_virtual: false,
            embedding: None,
            git_ref: Some("main".into()),
            ingested_at: None,
        });
        acc2.graph
            .module_nodes
            .push((module_id.to_string(), module_path.clone()));

        commit_snapshot(&pg, &neo4j, REPO2, true, acc2)
            .await
            .expect(
                "second commit_snapshot (module id reuse) must succeed — a stale \
                 community_members row must not FK-block the module delete",
            );

        // (a) commit succeeded is proven by the `.expect` above not panicking.

        // (b) the module's content is genuinely updated, not left stale.
        let (persisted_summary,): (Option<String>,) =
            sqlx::query_as("SELECT summary FROM modules WHERE id = $1")
                .bind(module_id)
                .fetch_one(&pg)
                .await
                .expect("module row must exist after the reuse commit");
        assert_eq!(
            persisted_summary.as_deref(),
            Some("v2 summary — content changed on re-ingest"),
            "module content must reflect the SECOND commit, not the stale first one"
        );

        // (c) the stale community_members row is gone — not orphaned, not
        // still pointing at a module whose content changed out from under it.
        let (remaining_count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM community_members WHERE module_id = $1")
                .bind(module_id)
                .fetch_one(&pg)
                .await
                .expect("count community_members after second commit");
        assert_eq!(
            remaining_count, 0,
            "stale community_members row referencing the reused module id must be cleared"
        );

        clean_repo(&pg, &neo4j, REPO2).await;
    }
}
