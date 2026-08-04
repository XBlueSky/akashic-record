//! In-memory accumulator for the RAM-first ingest pipeline (Roadmap F).
//!
//! Stages 4-7 append into this instead of writing to Postgres/Neo4j. The
//! shapes are E2's `PgRepoSnapshot`/`GraphRepoSnapshot` (Roadmap E2,
//! `akashic_domain::types`) — reused verbatim, not duplicated, since this
//! accumulator's final state IS what gets committed via
//! `SnapshotPgRepo::import_repo_snapshot` + the Neo4j write-port replay
//! (Task 7).

use std::collections::HashMap;

use akashic_domain::types::{GraphRepoSnapshot, PgRepoSnapshot};
use uuid::Uuid;

/// One chunk visible to Stage 6/7 resolution for this ingest run.
///
/// Roadmap F, Task 7.5 (post-Task-7 fix): two independent code reviews of
/// Task 7 found that Stage 6's `ChunkIndex` — the structure that powers
/// EVERY CALLS/REFERENCES/IMPLEMENTS/ROUTES_TO/MAKES_HTTP_CALL edge
/// resolution — and Stage 7's entry-point scan were built SOLELY from
/// `acc.pg.chunks`. That field is deliberately delta-only: `resolve_chunks`
/// (Task 2) never re-stages a byte-matched/unchanged chunk there, which is
/// correct for what actually gets WRITTEN (nothing needs to change for an
/// unchanged row) but means any chunk that byte-matched an existing row is
/// INVISIBLE to both stages. On a re-run against COMPLETELY unchanged source
/// content — the exact scenario Roadmap E1's content-match skip exists to
/// make cheap, so the D1/D1b/D1c/D3 call-resolution roadmap can re-measure
/// recall under new resolver code — `acc.pg.chunks` ends up EMPTY and Stage 6
/// resolves ZERO edges: not degraded, a total failure of that workflow.
///
/// `chunk_index_source` fixes this: every chunk currently valid for this
/// repo — both freshly-resolved this run (also mirrored into `pg.chunks`)
/// AND byte-matched/reused chunks (deliberately NOT in `pg.chunks`, since
/// nothing needs to be re-written for them, but still fully present in
/// Postgres from a prior run) — gets an entry here. `pg.chunks` stays
/// delta-only exactly as before; this field is index-only and is never
/// written to any database directly.
///
/// Field selection: the 7-tuple Stage 6's `ChunkIndex::build_from` consumes
/// (id, name, module_path, parent_fqn, signature, chunk_type, content) plus
/// `language` (Stage 7's entry-point scan and the Go-structural-typing
/// filter) — PLUS `fqn`. `fqn` isn't read by `ChunkIndex` itself, but Stage
/// 6's EXT-8-1 auxiliary maps (`fqn_by_id`/`type_id_by_fqn`) are built by a
/// SEPARATE scan over the exact same source and read `.fqn` — switching
/// that scan's source to `chunk_index_source` too (this fix does, since
/// otherwise reused chunks would stay invisible to the inheritance-aware
/// tier-0 cascade specifically) requires `fqn` here as well.
#[derive(Debug, Clone)]
pub(crate) struct ChunkIndexEntry {
    pub id: Uuid,
    pub name: String,
    pub module_path: String,
    pub fqn: Option<String>,
    pub parent_fqn: Option<String>,
    pub signature: Option<String>,
    pub chunk_type: String,
    pub content: String,
    pub language: Option<String>,
}

/// Everything Stages 4-7 resolve before the commit gate (Task 7) decides
/// whether to write it. `module_path_to_id` exists because Stage 5's
/// `IMPORTS_FROM` resolution and Stage 6's chunk-index construction both need
/// to translate a module PATH (known at parse time) to the UUID Stage 4
/// assigned it — a lookup that used to be a Postgres query
/// (`ModuleRepo::resolve_module_paths`) because Stage 4 had already written
/// those rows; now it's a plain in-memory map lookup.
///
/// `chunk_index_source` (Roadmap F, Task 7.5): see `ChunkIndexEntry`'s doc
/// comment for the full rationale — this is index-only, never written to any
/// database, and exists purely so Stage 6/7 see byte-matched/reused chunks
/// that `pg.chunks` deliberately omits.
///
/// Constructed and populated starting in this task (Task 2, Roadmap F) but
/// only *consumed* by production code starting with Task 3's Stage 4
/// rewrite. `#[allow(dead_code)]` is a deliberate, temporary task-boundary
/// artifact — matching the `FailedFile` precedent from this same roadmap's
/// Task 1 (`stages.rs`) — rather than an underscore-prefix or deletion,
/// because this is the pinned shape Task 3 onward consumes verbatim.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub(crate) struct IngestAccumulator {
    pub pg: PgRepoSnapshot,
    pub graph: GraphRepoSnapshot,
    pub module_path_to_id: HashMap<String, Uuid>,
    pub chunk_index_source: Vec<ChunkIndexEntry>,
}

impl IngestAccumulator {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_accumulator_is_empty() {
        let acc = IngestAccumulator::new();
        assert!(acc.pg.modules.is_empty());
        assert!(acc.pg.chunks.is_empty());
        assert!(acc.graph.chunk_nodes.is_empty());
        assert!(acc.module_path_to_id.is_empty());
        assert!(acc.chunk_index_source.is_empty());
    }
}

/// Live-DB test for `IngestionStore::resolve_chunks`.
///
/// This only needs a live **Postgres** connection (to seed + read the
/// "existing chunk" row the content-match-skip half of the test depends on).
/// `resolve_chunks` never touches Neo4j at all (it appends to an in-memory
/// `IngestAccumulator` instead of writing anywhere), so `IngestionStore` is
/// built here with real Postgres-backed adapters for the PG-side ports
/// (trivial — this test already holds a live PG pool for seeding) and small
/// no-op stub adapters for the four Neo4j-only ports, rather than also
/// standing up a live Neo4j connection this test has no genuine need for.
///
/// The embedder/DB-connect pattern (deterministic fake vector + a
/// call-counting `EmbeddingProvider`) mirrors `e2e_test.rs`'s established
/// `FakeEmbedder` / `CountingEmbedder` (the same one
/// `e2e_stage4_skips_reembed_on_unchanged_content`, E1's Task 2 test, uses to
/// prove a byte-match skips re-embedding) — re-derived here rather than
/// imported because those types are module-private in `e2e_test.rs` (same
/// visibility drift `snapshot_e2e_test.rs` already documented and worked
/// around for the same reason).
#[cfg(test)]
mod resolve_chunks_tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use akashic_domain::ports::{
        ChunkGraphNode, ChunkGraphRepo, ChunkRepo, CommunityGraphRepo, CommunityRepo,
        EmbeddingResponse, IngestEdgeRepo, IngestionJobRepo, ModuleGraphRepo, ModuleRepo,
        NoteHealthRepo, NoteRepo,
    };
    use akashic_domain::types::{ChunkRow, IngestEdge};
    use akashic_embed::EmbeddingProvider;
    use akashic_extraction::{ChunkMetadata, RawChunk, Visibility};
    use akashic_store_pg::{
        PgChunkRepo, PgCommunityRepo, PgIngestionJobRepo, PgModuleRepo, PgNoteHealthRepo,
        PgNoteRepo,
    };
    use uuid::Uuid;

    use crate::ingestion::accumulator::IngestAccumulator;
    use crate::ingestion::store::IngestionStore;

    /// Width of the live `chunks.embedding` column — same as
    /// `e2e_test.rs::DIM` (`vector(1536)`).
    const DIM: usize = 1536;

    const REPO: &str = "accumulator-resolve-chunks-test";
    const MODULE_PATH: &str = "src/lib.rs";

    /// Deterministic fake vector generator — same FNV/xorshift algorithm as
    /// `e2e_test.rs::FakeEmbedder`, factored to a plain fn so both `embed`
    /// and `embed_batch` below can share it.
    fn fake_vector(text: &str) -> Vec<f32> {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in text.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let mut v = Vec::with_capacity(DIM);
        let mut state = h | 1;
        for _ in 0..DIM {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let f = ((state % 1000) as f32) / 1000.0 + 0.001;
            v.push(f);
        }
        v
    }

    /// Counts individual texts actually embedded (summed across every
    /// `embed`/`embed_batch` call) rather than call *count* —
    /// `resolve_chunks` always issues two `embed_batch` calls per invocation
    /// with pending work (one content batch, one signature batch, the
    /// latter possibly empty; same shape `store_chunks` already has), so the
    /// call count alone can't distinguish "1 chunk embedded" from "N chunks
    /// embedded". The text count can: it proves the byte-matched "existing"
    /// chunk's content never entered either batch.
    struct CountingEmbedder {
        texts_embedded: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl EmbeddingProvider for CountingEmbedder {
        async fn embed(&self, text: &str) -> anyhow::Result<EmbeddingResponse> {
            self.texts_embedded.fetch_add(1, Ordering::SeqCst);
            Ok(EmbeddingResponse {
                vector: fake_vector(text),
                tokens_used: 0,
                model: "fake".into(),
            })
        }

        async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.texts_embedded.fetch_add(texts.len(), Ordering::SeqCst);
            Ok(texts.iter().map(|t| fake_vector(t)).collect())
        }

        fn dimensions(&self) -> usize {
            DIM
        }
    }

    // ── No-op stand-ins for the four Neo4j-only ports `resolve_chunks` never
    // touches (see module doc comment above for why no live Neo4j is needed) ──
    struct UnusedGraphPort;

    #[async_trait::async_trait]
    impl ChunkGraphRepo for UnusedGraphPort {
        async fn create_chunk_nodes_batch(
            &self,
            _repo_name: &str,
            _chunks: &[ChunkGraphNode],
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches ChunkGraphRepo")
        }
        async fn create_tagged_with_edges(
            &self,
            _tag_pg_ids: Vec<String>,
            _tag_names: Vec<String>,
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches ChunkGraphRepo")
        }
        async fn delete_chunks_by_repo(&self, _repo_name: &str) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches ChunkGraphRepo")
        }
        async fn delete_chunks_by_module(
            &self,
            _repo_name: &str,
            _module_path: &str,
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches ChunkGraphRepo")
        }
    }

    #[async_trait::async_trait]
    impl ModuleGraphRepo for UnusedGraphPort {
        async fn upsert_module_node(
            &self,
            _repo_name: &str,
            _module_pg_id: Uuid,
            _path: &str,
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches ModuleGraphRepo")
        }
        async fn create_has_module_edge(
            &self,
            _repo_name: &str,
            _module_pg_id: Uuid,
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches ModuleGraphRepo")
        }
        async fn create_has_chunk_edges(
            &self,
            _module_pg_id: Uuid,
            _chunk_pg_ids: &[Uuid],
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches ModuleGraphRepo")
        }
        async fn delete_modules_by_repo(&self, _repo_name: &str) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches ModuleGraphRepo")
        }
        async fn get_module_nodes(
            &self,
            _repo_name: &str,
        ) -> anyhow::Result<Vec<(String, String)>> {
            unreachable!("resolve_chunks never touches ModuleGraphRepo")
        }
    }

    #[async_trait::async_trait]
    impl CommunityGraphRepo for UnusedGraphPort {
        async fn delete_communities_by_repo(&self, _repo_name: &str) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches CommunityGraphRepo")
        }
        async fn create_community_node(
            &self,
            _community_id: Uuid,
            _level: u8,
            _repo_name: &str,
            _member_count: usize,
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches CommunityGraphRepo")
        }
        async fn create_has_member_edge(
            &self,
            _community_id: Uuid,
            _member_pg_id: Uuid,
            _member_label: &str,
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches CommunityGraphRepo")
        }
        async fn load_graph_for_detection(
            &self,
            _repo_name: &str,
        ) -> anyhow::Result<(
            Vec<Uuid>,
            std::collections::HashMap<Uuid, (String, String)>,
            Vec<(Uuid, Uuid, f64)>,
        )> {
            unreachable!("resolve_chunks never touches CommunityGraphRepo")
        }
    }

    #[async_trait::async_trait]
    impl IngestEdgeRepo for UnusedGraphPort {
        async fn create_call_edges(
            &self,
            _repo_name: &str,
            _calls: &[IngestEdge],
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches IngestEdgeRepo")
        }
        async fn create_reference_edges(
            &self,
            _repo_name: &str,
            _refs: &[IngestEdge],
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches IngestEdgeRepo")
        }
        async fn create_implements_edges(
            &self,
            _repo_name: &str,
            _impls: &[IngestEdge],
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches IngestEdgeRepo")
        }
        async fn create_routes_to_edges(
            &self,
            _repo_name: &str,
            _routes: &[IngestEdge],
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches IngestEdgeRepo")
        }
        async fn create_makes_http_call_edges(
            &self,
            _repo_name: &str,
            _calls: &[IngestEdge],
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches IngestEdgeRepo")
        }
        async fn create_symbol_import_edges(
            &self,
            _repo_name: &str,
            _imports: &[(String, Uuid)],
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches IngestEdgeRepo")
        }
        async fn create_import_edges(
            &self,
            _src_ids: &[String],
            _tgt_ids: &[String],
        ) -> anyhow::Result<()> {
            unreachable!("resolve_chunks never touches IngestEdgeRepo")
        }
    }

    /// Build a `RawChunk` fixture varying only name/content — the fields
    /// `resolve_chunks` actually reads for the embed-text + snapshot-row build.
    fn raw_chunk(name: &str, content: &str) -> RawChunk {
        RawChunk {
            chunk_type: "function".into(),
            name: name.into(),
            fqn: Some(name.into()),
            parent_fqn: None,
            start_line: 1,
            end_line: 2,
            start_byte: 0,
            end_byte: content.len(),
            signature: None,
            content: content.into(),
            doc: None,
            receiver: None,
            is_async: false,
            is_static: false,
            is_const: false,
            is_exported: true,
            visibility: Visibility::Public,
            metadata: ChunkMetadata::default(),
        }
    }

    async fn cleanup(pg: &sqlx::PgPool) {
        let _ = sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
            .bind(REPO)
            .execute(pg)
            .await;
    }

    #[tokio::test]
    #[serial_test::serial]
    #[ignore = "requires live Postgres; run with --ignored"]
    async fn resolve_chunks_reuses_id_on_byte_match_and_generates_fresh_id_otherwise() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://akashic@localhost:5432/akashic".into());
        let pg = akashic_store_pg::connect(&database_url)
            .await
            .expect("connect Postgres");

        // Idempotent schema init (same call every live-DB test in this crate
        // makes), hardcoded to float32/1536 (matching this test's fake
        // vectors) rather than routing through a full `akashic_config::Config`
        // — this test needs no Neo4j / LLM / other config knob at all.
        akashic_store_pg::init_schema(&pg, DIM, "vector(1536)", "vector_cosine_ops")
            .await
            .expect("pg init_schema");

        cleanup(&pg).await;

        // ── Seed one pre-existing chunk row directly (bypassing the store's
        // write path — this row stands in for "what a prior ingest already
        // landed", matching what `list_chunks_in_module_all` would return) ──
        let existing_content = "fn existing() {}";
        let existing_vec = pgvector::Vector::from(fake_vector(existing_content));
        let (existing_id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO chunks (repo_name, module_path, chunk_type, name, content, language, embedding) \
             VALUES ($1, $2, 'function', 'existing', $3, 'rust', $4) RETURNING id",
        )
        .bind(REPO)
        .bind(MODULE_PATH)
        .bind(existing_content)
        .bind(existing_vec)
        .fetch_one(&pg)
        .await
        .expect("seed existing chunk row");

        let mut existing: HashMap<String, ChunkRow> = HashMap::new();
        existing.insert(
            "existing".to_string(),
            ChunkRow {
                id: existing_id,
                name: "existing".to_string(),
                chunk_type: "function".to_string(),
                signature: None,
                content: existing_content.to_string(),
                language: Some("rust".to_string()),
                fqn: None,
                parent_fqn: None,
            },
        );

        // ── Build an IngestionStore: real Postgres-backed ports (this test
        // already holds a live PG pool) + no-op stubs for the Neo4j-only
        // ports + a call-counting embedder ──────────────────────────────
        let chunks: Arc<dyn ChunkRepo> = Arc::new(PgChunkRepo::new(pg.clone()));
        let modules: Arc<dyn ModuleRepo> = Arc::new(PgModuleRepo::new(pg.clone()));
        let community: Arc<dyn CommunityRepo> = Arc::new(PgCommunityRepo::new(pg.clone()));
        let job_repo: Arc<dyn IngestionJobRepo> = Arc::new(PgIngestionJobRepo::new(pg.clone()));
        let note: Arc<dyn NoteRepo> = Arc::new(PgNoteRepo::new(pg.clone()));
        let note_health: Arc<dyn NoteHealthRepo> = Arc::new(PgNoteHealthRepo::new(pg.clone()));
        let chunk_graph: Arc<dyn ChunkGraphRepo> = Arc::new(UnusedGraphPort);
        let module_graph: Arc<dyn ModuleGraphRepo> = Arc::new(UnusedGraphPort);
        let community_graph: Arc<dyn CommunityGraphRepo> = Arc::new(UnusedGraphPort);
        let ingest_edge: Arc<dyn IngestEdgeRepo> = Arc::new(UnusedGraphPort);
        let embedder = Arc::new(CountingEmbedder {
            texts_embedded: AtomicUsize::new(0),
        });
        let (event_tx, _rx) = tokio::sync::broadcast::channel(16);

        let store = IngestionStore::new(
            chunks,
            chunk_graph,
            modules,
            module_graph,
            community,
            community_graph,
            job_repo,
            ingest_edge,
            note,
            note_health,
            embedder.clone() as Arc<dyn EmbeddingProvider>,
            event_tx,
        );

        // ── resolve_chunks: one byte-matching chunk + one new chunk ───────
        let chunks_in = vec![
            raw_chunk("existing", existing_content),
            raw_chunk("new_fn", "fn new_fn() {}"),
        ];
        let mut acc = IngestAccumulator::new();
        let results = store
            .resolve_chunks(REPO, MODULE_PATH, "rust", &chunks_in, &existing, &mut acc)
            .await
            .expect("resolve_chunks");

        assert_eq!(results.len(), 2);

        // Byte-match: reused id, NOT freshly embedded.
        let (reused_id, reused_fresh) = results[0];
        assert_eq!(
            reused_id, existing_id,
            "byte-match must reuse the existing id"
        );
        assert!(
            !reused_fresh,
            "byte-match must NOT be marked freshly embedded"
        );

        // New content: fresh id (not nil, not the existing one), freshly embedded.
        let (new_id, new_fresh) = results[1];
        assert_ne!(new_id, existing_id, "new chunk must get a NEW id");
        assert_ne!(new_id, Uuid::nil(), "new chunk id must not be nil");
        assert!(
            new_fresh,
            "new/changed chunk must be marked freshly embedded"
        );

        // Exactly ONE text was embedded in total (the "new_fn" content text;
        // neither chunk has a signature, so the signature batch is empty) —
        // proving the byte-matched "existing" chunk's content never reached
        // the embedder at all, only the new/changed chunk did.
        assert_eq!(
            embedder.texts_embedded.load(Ordering::SeqCst),
            1,
            "exactly one text (the new chunk's) must reach the embedder; the byte-match must not"
        );

        // The accumulator gained exactly one new ChunkSnapshotRow + one new
        // ChunkGraphNode (for "new_fn"), and none for "existing".
        assert_eq!(acc.pg.chunks.len(), 1, "exactly one new ChunkSnapshotRow");
        assert_eq!(acc.pg.chunks[0].id, new_id);
        assert_eq!(acc.pg.chunks[0].name, "new_fn");
        assert_eq!(
            acc.pg.chunks[0].git_ref, None,
            "git_ref is left unset at resolution time — Task 3's caller fills it in (it has req.git_ref in scope; this method doesn't)"
        );
        assert_eq!(
            acc.pg.chunks[0].ingested_at, None,
            "ingested_at is left unset at resolution time — the commit step (Task 7) stamps it when the data actually lands"
        );
        assert_eq!(
            acc.graph.chunk_nodes.len(),
            1,
            "exactly one new ChunkGraphNode"
        );
        assert_eq!(acc.graph.chunk_nodes[0].name, "new_fn");

        cleanup(&pg).await;
    }
}
