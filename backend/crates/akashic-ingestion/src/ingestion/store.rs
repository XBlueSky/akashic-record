use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::info;
use uuid::Uuid;

use akashic_domain::ports::{
    ChunkGraphNode, ChunkGraphRepo, ChunkRepo, CommunityGraphRepo, CommunityRepo, IngestEdgeRepo,
    IngestionJobRepo, ModuleGraphRepo, ModuleRepo, NoteHealthRepo, NoteRepo,
};
use akashic_domain::types::ChunkRow;
use akashic_embed::EmbeddingProvider;
use akashic_kernel::AppEvent;

use akashic_extraction::{MetadataValue, RawChunk};

/// Split a camelCase or PascalCase string into individual words.
fn split_camel_case(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for c in s.chars() {
        if c.is_uppercase() && !current.is_empty() {
            words.push(current.clone());
            current.clear();
        }
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Extract semantic tags from a chunk's name and type.
fn extract_chunk_tags(name: &str, chunk_type: &str) -> Vec<String> {
    let mut tags = Vec::new();

    // Split snake_case / kebab-case into words, then split camelCase
    let words: Vec<String> = name
        .split(['_', '-'])
        .flat_map(split_camel_case)
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() > 2)
        .collect();
    tags.extend(words);

    // Add chunk_type as a tag
    tags.push(chunk_type.to_lowercase());

    tags.sort();
    tags.dedup();
    tags.truncate(10);
    tags
}

/// Read a `String` metadata field off a chunk (used to lift the walker's
/// `http_method` / `http_path` into `ChunkGraphNode`). Returns `None` when the
/// field is absent or not a string.
pub(crate) fn chunk_meta_str(chunk: &RawChunk, key: &str) -> Option<String> {
    match chunk.metadata.fields.get(key) {
        Some(MetadataValue::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Build a contextual prefix for embedding text.
/// Encodes repo, module, language, and chunk type so the vector
/// captures positional context alongside content semantics.
fn build_contextual_embed_text(
    repo_name: &str,
    module_path: &str,
    language: &str,
    chunk: &RawChunk,
) -> String {
    let type_label = match chunk.chunk_type.as_str() {
        "function" => "function",
        "class" | "struct" => "type definition",
        "trait" | "interface" => "interface/trait",
        "enum" => "enumeration",
        "type" => "type alias",
        "constant" | "config" => "constant/config",
        "component" => "UI component",
        "section" | "readme" => "documentation",
        "module" => "module",
        "macro" => "macro",
        _ => &chunk.chunk_type,
    };

    // Header symbol: prefer the fully-qualified name (EXT-1/2 enrichment) so
    // same-named symbols in different scopes get distinct vectors; fall back to
    // the bare name.
    let symbol = chunk
        .fqn
        .as_deref()
        .filter(|f| !f.is_empty())
        .unwrap_or(&chunk.name);
    // Enclosing scope — unless the fqn already implies it (avoid redundancy).
    let parent_suffix = match chunk.parent_fqn.as_deref() {
        Some(p) if !p.is_empty() && !symbol.starts_with(p) => format!(" in '{p}'"),
        _ => String::new(),
    };

    // Body, most-semantic first: the doc comment states intent in natural
    // language (the richest signal); the signature gives the type shape; the
    // content prefix is only a fallback when neither is present.
    let mut body = String::new();
    if let Some(doc) = chunk
        .doc
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        body.push_str(&doc.chars().take(500).collect::<String>());
    }
    if let Some(sig) = chunk.signature.as_deref().filter(|s| !s.is_empty()) {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(sig);
    }
    if body.is_empty() {
        body = chunk.content.chars().take(300).collect::<String>();
    }

    format!(
        "In repository '{repo_name}', module '{module_path}' ({language}), {type_label} '{symbol}'{parent_suffix}:\n{body}"
    )
}

/// Store ingested chunks and modules into PostgreSQL + Neo4j.
///
/// A1 T3: chunk/module writes go through port traits (`chunks`, `chunk_graph`,
/// `modules`, `module_graph`).
/// A1 T6: community writes go through `community` and `community_graph`.
/// A1 T8: job management, edge creation, and repo-graph writes go through
/// `job_repo`, `ingest_edge`, and `repo_graph`. Note reads for S8 go through
/// `note` and `note_health`.
#[derive(Clone)]
pub struct IngestionStore {
    // ── Port-backed repos (T3) ─────────────────────────────────────────────
    pub(crate) chunks: Arc<dyn ChunkRepo>,
    chunk_graph: Arc<dyn ChunkGraphRepo>,
    pub(crate) modules: Arc<dyn ModuleRepo>,
    module_graph: Arc<dyn ModuleGraphRepo>,
    // ── Port-backed repos (T6) ─────────────────────────────────────────────
    pub(crate) community: Arc<dyn CommunityRepo>,
    pub(crate) community_graph: Arc<dyn CommunityGraphRepo>,
    // ── Port-backed repos (T8) ─────────────────────────────────────────────
    job_repo: Arc<dyn IngestionJobRepo>,
    ingest_edge: Arc<dyn IngestEdgeRepo>,
    pub(crate) note: Arc<dyn NoteRepo>,
    pub(crate) note_health: Arc<dyn NoteHealthRepo>,
    // ── Cross-cutting infra ───────────────────────────────────────────────
    // `pub(crate)` (Roadmap F, Task 3): `stage4_embed_store` (stages.rs) now
    // builds `ModuleSnapshotRow`/`LargeChunkSnapshotRow` embeddings directly
    // (mirroring `upsert_module`/`store_large_chunks`'s internal
    // `self.embedder.embed(...)` calls) rather than through a DB-writing
    // method, so it needs the field itself, not just a write-shaped wrapper.
    pub(crate) embedder: Arc<dyn EmbeddingProvider>,
    event_tx: tokio::sync::broadcast::Sender<AppEvent>,
}

impl IngestionStore {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chunks: Arc<dyn ChunkRepo>,
        chunk_graph: Arc<dyn ChunkGraphRepo>,
        modules: Arc<dyn ModuleRepo>,
        module_graph: Arc<dyn ModuleGraphRepo>,
        community: Arc<dyn CommunityRepo>,
        community_graph: Arc<dyn CommunityGraphRepo>,
        job_repo: Arc<dyn IngestionJobRepo>,
        ingest_edge: Arc<dyn IngestEdgeRepo>,
        note: Arc<dyn NoteRepo>,
        note_health: Arc<dyn NoteHealthRepo>,
        embedder: Arc<dyn EmbeddingProvider>,
        event_tx: tokio::sync::broadcast::Sender<AppEvent>,
    ) -> Self {
        Self {
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
            embedder,
            event_tx,
        }
    }

    /// Store a batch of chunks: embed them, write to PG via ChunkRepo, and
    /// create Neo4j graph nodes via ChunkGraphRepo.
    pub async fn store_chunks(
        &self,
        repo_name: &str,
        git_ref: &str,
        module_path: &str,
        language: &str,
        chunks: &[RawChunk],
    ) -> Result<Vec<Uuid>> {
        let mut ids = Vec::with_capacity(chunks.len());

        // EXT-4: embed all content texts in one batch (the provider does a single
        // API round-trip per sub-batch instead of one per chunk).
        let content_texts: Vec<String> = chunks
            .iter()
            .map(|c| build_contextual_embed_text(repo_name, module_path, language, c))
            .collect();
        let content_embeddings = self.embedder.embed_batch(&content_texts).await?;

        // Signature embeddings only for chunks that carry a signature; batch them
        // too, remembering which chunk index each belongs to.
        let sig_idx: Vec<usize> = chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.signature.is_some())
            .map(|(i, _)| i)
            .collect();
        let sig_texts: Vec<String> = sig_idx
            .iter()
            .map(|&i| {
                let c = &chunks[i];
                let sig_name = c.fqn.as_deref().unwrap_or(&c.name);
                format!("{} {}", sig_name, c.signature.as_deref().unwrap_or(""))
            })
            .collect();
        let sig_embeddings = self.embedder.embed_batch(&sig_texts).await?;
        let mut sig_by_chunk: std::collections::HashMap<usize, Vec<f32>> =
            std::collections::HashMap::with_capacity(sig_idx.len());
        for (k, &i) in sig_idx.iter().enumerate() {
            sig_by_chunk.insert(i, sig_embeddings[k].clone());
        }

        // Parallel arrays for the batched Neo4j Chunk-node UNWIND.
        let mut graph_nodes: Vec<ChunkGraphNode> = Vec::with_capacity(chunks.len());

        // Parallel arrays for the batched TAGGED_WITH UNWIND.
        let mut tag_pg_ids: Vec<String> = Vec::new();
        let mut tag_names: Vec<String> = Vec::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let content_emb = &content_embeddings[i];
            let sig_emb = sig_by_chunk.get(&i).map(|v| v.as_slice());

            // Insert into PG via ChunkRepo port (SQL moved to PgChunkRepo).
            let pg_id = self
                .chunks
                .store_chunk_row(
                    repo_name,
                    module_path,
                    &chunk.chunk_type,
                    &chunk.name,
                    chunk.signature.as_deref(),
                    &chunk.content,
                    language,
                    content_emb.as_slice(),
                    sig_emb,
                    git_ref,
                    chunk.fqn.as_deref(),
                    chunk.parent_fqn.as_deref(),
                    chunk.start_line as i32,
                    chunk.end_line as i32,
                    chunk.visibility.as_str(),
                    chunk.is_async,
                    chunk.is_static,
                    chunk.is_exported,
                    chunk.doc.as_deref(),
                )
                .await?;

            ids.push(pg_id);

            // Collect graph node for batched Neo4j UNWIND.
            let pg_id_str = pg_id.to_string();
            graph_nodes.push(ChunkGraphNode {
                pg_id: pg_id_str.clone(),
                name: chunk.name.clone(),
                chunk_type: chunk.chunk_type.clone(),
                module_path: module_path.to_string(),
                fqn: chunk.fqn.clone(),
                parent_fqn: chunk.parent_fqn.clone(),
                start_line: chunk.start_line as i64,
                end_line: chunk.end_line as i64,
                visibility: chunk.visibility.as_str().to_string(),
                is_async: chunk.is_async,
                is_static: chunk.is_static,
                is_exported: chunk.is_exported,
                http_method: chunk_meta_str(chunk, "http_method"),
                http_path: chunk_meta_str(chunk, "http_path"),
            });

            // Collect (pg_id, tag) pairs for the batched TAGGED_WITH UNWIND.
            for tag in extract_chunk_tags(&chunk.name, &chunk.chunk_type) {
                tag_pg_ids.push(pg_id_str.clone());
                tag_names.push(tag);
            }
        }

        if graph_nodes.is_empty() {
            return Ok(ids);
        }

        // Batched Chunk-node MERGE via ChunkGraphRepo port.
        self.chunk_graph
            .create_chunk_nodes_batch(repo_name, &graph_nodes)
            .await
            .context("Failed to batch-create Chunk nodes in Neo4j")?;

        // Batched Tag + TAGGED_WITH MERGE via ChunkGraphRepo port.
        self.chunk_graph
            .create_tagged_with_edges(tag_pg_ids, tag_names)
            .await
            .context("Failed to batch-create TAGGED_WITH edges in Neo4j")?;

        Ok(ids)
    }

    /// Resolve (embed, but do NOT write) a file's chunks, for the RAM-first
    /// pipeline (Roadmap F). For each chunk: if `existing` has a
    /// byte-identical match by name, reuse its id (no embedder call, no
    /// accumulator append — the existing row is already correct and, at
    /// commit time, is either the same row still being kept or gets
    /// re-imported verbatim as part of a fresh commit; either way nothing
    /// needs to change for it). Otherwise generate a fresh `Uuid::new_v4()`,
    /// embed it, and append a `ChunkSnapshotRow` + `ChunkGraphNode` + its
    /// tag pairs to `acc`.
    ///
    /// Returns `(id, was_freshly_embedded)` per input chunk, in the same
    /// order as `chunks`.
    ///
    /// Only called from tests until Task 3's Stage 4 rewrite wires it in as
    /// the production call site — same deliberate, temporary task-boundary
    /// `#[allow(dead_code)]` as `IngestAccumulator` (see its doc comment).
    #[allow(dead_code)]
    pub(crate) async fn resolve_chunks(
        &self,
        repo_name: &str,
        module_path: &str,
        language: &str,
        chunks: &[akashic_extraction::RawChunk],
        existing: &std::collections::HashMap<String, ChunkRow>,
        acc: &mut crate::ingestion::accumulator::IngestAccumulator,
    ) -> Result<Vec<(Uuid, bool)>> {
        // Partition into (reused, to_embed) up front so embedding only
        // happens for genuinely new/changed chunks — same content-match
        // skip semantics as today's `store_chunks` callers implement
        // one layer up (in Stage 4), just moved inside this method since
        // Stage 4 no longer needs to see the embedding mechanics directly.
        let mut results: Vec<Option<(Uuid, bool)>> = vec![None; chunks.len()];
        let mut to_embed_idx: Vec<usize> = Vec::new();

        for (i, chunk) in chunks.iter().enumerate() {
            if let Some(existing_row) = existing.get(&chunk.name)
                && existing_row.content == chunk.content
            {
                results[i] = Some((existing_row.id, false));
            } else {
                to_embed_idx.push(i);
            }
        }

        if to_embed_idx.is_empty() {
            return Ok(results.into_iter().map(|r| r.unwrap()).collect());
        }

        let to_embed: Vec<&akashic_extraction::RawChunk> =
            to_embed_idx.iter().map(|&i| &chunks[i]).collect();

        // Same batch-embedding shape as `store_chunks` (content + optional
        // signature embeddings), just without any DB write at the end.
        let content_texts: Vec<String> = to_embed
            .iter()
            .map(|c| build_contextual_embed_text(repo_name, module_path, language, c))
            .collect();
        let content_embeddings = self.embedder.embed_batch(&content_texts).await?;

        let sig_local_idx: Vec<usize> = to_embed
            .iter()
            .enumerate()
            .filter(|(_, c)| c.signature.is_some())
            .map(|(i, _)| i)
            .collect();
        let sig_texts: Vec<String> = sig_local_idx
            .iter()
            .map(|&i| {
                let c = to_embed[i];
                let sig_name = c.fqn.as_deref().unwrap_or(&c.name);
                format!("{} {}", sig_name, c.signature.as_deref().unwrap_or(""))
            })
            .collect();
        let sig_embeddings = self.embedder.embed_batch(&sig_texts).await?;
        let mut sig_by_local: std::collections::HashMap<usize, Vec<f32>> =
            std::collections::HashMap::with_capacity(sig_local_idx.len());
        for (k, &i) in sig_local_idx.iter().enumerate() {
            sig_by_local.insert(i, sig_embeddings[k].clone());
        }

        for (local_i, chunk) in to_embed.iter().enumerate() {
            let global_i = to_embed_idx[local_i];
            let id = Uuid::new_v4();
            let content_emb = content_embeddings[local_i].clone();
            let sig_emb = sig_by_local.get(&local_i).cloned();

            acc.pg.chunks.push(akashic_domain::types::ChunkSnapshotRow {
                id,
                module_path: module_path.to_string(),
                chunk_type: chunk.chunk_type.clone(),
                name: chunk.name.clone(),
                signature: chunk.signature.clone(),
                content: chunk.content.clone(),
                language: Some(language.to_string()),
                embedding: content_emb,
                signature_embedding: sig_emb,
                git_ref: None, // filled in by Stage 4's caller, which knows req.git_ref
                fqn: chunk.fqn.clone(),
                parent_fqn: chunk.parent_fqn.clone(),
                start_line: Some(chunk.start_line as i32),
                end_line: Some(chunk.end_line as i32),
                visibility: Some(chunk.visibility.as_str().to_string()),
                is_async: chunk.is_async,
                is_static: chunk.is_static,
                is_exported: chunk.is_exported,
                doc: chunk.doc.clone(),
                ingested_at: None, // filled by the commit step (Task 7) at write time
            });

            acc.graph
                .chunk_nodes
                .push(akashic_domain::ports::ChunkGraphNode {
                    pg_id: id.to_string(),
                    name: chunk.name.clone(),
                    chunk_type: chunk.chunk_type.clone(),
                    module_path: module_path.to_string(),
                    fqn: chunk.fqn.clone(),
                    parent_fqn: chunk.parent_fqn.clone(),
                    start_line: chunk.start_line as i64,
                    end_line: chunk.end_line as i64,
                    visibility: chunk.visibility.as_str().to_string(),
                    is_async: chunk.is_async,
                    is_static: chunk.is_static,
                    is_exported: chunk.is_exported,
                    http_method: chunk_meta_str(chunk, "http_method"),
                    http_path: chunk_meta_str(chunk, "http_path"),
                });

            for tag in extract_chunk_tags(&chunk.name, &chunk.chunk_type) {
                acc.graph.chunk_tags.push((id.to_string(), tag));
            }

            results[global_i] = Some((id, true));
        }

        Ok(results.into_iter().map(|r| r.unwrap()).collect())
    }

    /// Upsert a module into PG (via ModuleRepo) and create its Neo4j node +
    /// relationships (via ModuleGraphRepo).
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_module(
        &self,
        repo_name: &str,
        module_path: &str,
        language: &str,
        summary: &str,
        exports_count: i32,
        file_count: i32,
        is_virtual: bool,
        git_ref: &str,
        chunk_pg_ids: &[Uuid],
    ) -> Result<Uuid> {
        // Embed the module summary; caller (pipeline) keeps this logic here
        // since it requires the embedder which stays on IngestionStore.
        let embedding = if !summary.is_empty() {
            let e = self.embedder.embed(summary).await?;
            Some(e.vector)
        } else {
            None
        };

        // Upsert module in PG via ModuleRepo port.
        let module_pg_id = self
            .modules
            .upsert_module(
                repo_name,
                module_path,
                language,
                summary,
                exports_count,
                file_count,
                is_virtual,
                git_ref,
                embedding.as_deref(),
            )
            .await
            .context("Failed to upsert module in PG")?;

        // Neo4j: merge Module node via ModuleGraphRepo.
        self.module_graph
            .upsert_module_node(repo_name, module_pg_id, module_path)
            .await?;

        // Neo4j: Repository -[:HAS_MODULE]-> Module.
        self.module_graph
            .create_has_module_edge(repo_name, module_pg_id)
            .await?;

        // Neo4j: Module -[:HAS_CHUNK]-> Chunk for each chunk.
        self.module_graph
            .create_has_chunk_edges(module_pg_id, chunk_pg_ids)
            .await?;

        Ok(module_pg_id)
    }

    /// Create IMPORTS_FROM edges between Module nodes in Neo4j.
    ///
    /// Each `(src_path, tgt_path)` pair is resolved to PG module IDs via
    /// `ModuleRepo::resolve_module_paths`, then a directed `IMPORTS_FROM`
    /// relationship is MERGEd in the graph via `IngestEdgeRepo::create_import_edges`.
    pub async fn create_import_edges(
        &self,
        repo_name: &str,
        imports: &[(String, String)],
    ) -> Result<()> {
        if imports.is_empty() {
            info!(repo_name, count = 0, "Created IMPORTS_FROM edges");
            return Ok(());
        }

        // PG: resolve every distinct module path to its id via ModuleRepo port.
        let mut distinct_paths: Vec<String> = imports
            .iter()
            .flat_map(|(s, t)| [s.clone(), t.clone()])
            .collect();
        distinct_paths.sort();
        distinct_paths.dedup();

        let rows = self
            .modules
            .resolve_module_paths(repo_name, &distinct_paths)
            .await
            .context("Failed to resolve module paths for IMPORTS_FROM edges")?;
        let path_to_id: std::collections::HashMap<String, Uuid> = rows.into_iter().collect();

        // Map each (src_path, tgt_path) to (src_id, tgt_id), dropping pairs
        // where either module is missing (same guard as the old `if let (Some, Some)`).
        let mut src_ids: Vec<String> = Vec::with_capacity(imports.len());
        let mut tgt_ids: Vec<String> = Vec::with_capacity(imports.len());
        for (src_path, tgt_path) in imports {
            if let (Some(src_id), Some(tgt_id)) =
                (path_to_id.get(src_path), path_to_id.get(tgt_path))
            {
                src_ids.push(src_id.to_string());
                tgt_ids.push(tgt_id.to_string());
            }
        }

        // Neo4j: ONE UNWIND via IngestEdgeRepo port.
        self.ingest_edge
            .create_import_edges(&src_ids, &tgt_ids)
            .await
            .context("Failed to batch-create IMPORTS_FROM edges in Neo4j")?;

        info!(
            repo_name,
            count = imports.len(),
            "Created IMPORTS_FROM edges"
        );
        Ok(())
    }

    /// Create CALLS edges between Chunk nodes in Neo4j.
    pub async fn create_call_edges(
        &self,
        repo_name: &str,
        calls: &[akashic_extraction::resolution::ResolvedEdge],
    ) -> Result<()> {
        use akashic_domain::types::IngestEdge;
        let edges: Vec<IngestEdge> = calls
            .iter()
            .map(|e| IngestEdge {
                src_chunk_id: e.src_chunk_id,
                tgt_chunk_id: e.tgt_chunk_id,
                confidence: e.confidence,
                method: e.method.clone(),
                ref_kind: e.ref_kind.clone(),
            })
            .collect();
        self.ingest_edge
            .create_call_edges(repo_name, &edges)
            .await
            .context("Failed to create CALLS edges via IngestEdgeRepo")
    }

    pub async fn create_reference_edges(
        &self,
        repo_name: &str,
        refs: &[akashic_extraction::resolution::ResolvedEdge],
    ) -> Result<()> {
        use akashic_domain::types::IngestEdge;
        let edges: Vec<IngestEdge> = refs
            .iter()
            .map(|e| IngestEdge {
                src_chunk_id: e.src_chunk_id,
                tgt_chunk_id: e.tgt_chunk_id,
                confidence: e.confidence,
                method: e.method.clone(),
                ref_kind: e.ref_kind.clone(),
            })
            .collect();
        self.ingest_edge
            .create_reference_edges(repo_name, &edges)
            .await
            .context("Failed to create REFERENCES edges via IngestEdgeRepo")
    }

    pub async fn create_implements_edges(
        &self,
        repo_name: &str,
        impls: &[akashic_extraction::resolution::ResolvedEdge],
    ) -> Result<()> {
        use akashic_domain::types::IngestEdge;
        let edges: Vec<IngestEdge> = impls
            .iter()
            .map(|e| IngestEdge {
                src_chunk_id: e.src_chunk_id,
                tgt_chunk_id: e.tgt_chunk_id,
                confidence: e.confidence,
                method: e.method.clone(),
                ref_kind: e.ref_kind.clone(),
            })
            .collect();
        self.ingest_edge
            .create_implements_edges(repo_name, &edges)
            .await
            .context("Failed to create IMPLEMENTS edges via IngestEdgeRepo")
    }

    pub async fn create_routes_to_edges(
        &self,
        repo_name: &str,
        routes: &[akashic_extraction::resolution::ResolvedEdge],
    ) -> Result<()> {
        use akashic_domain::types::IngestEdge;
        let edges: Vec<IngestEdge> = routes
            .iter()
            .map(|e| IngestEdge {
                src_chunk_id: e.src_chunk_id,
                tgt_chunk_id: e.tgt_chunk_id,
                confidence: e.confidence,
                method: e.method.clone(),
                ref_kind: e.ref_kind.clone(),
            })
            .collect();
        self.ingest_edge
            .create_routes_to_edges(repo_name, &edges)
            .await
            .context("Failed to create ROUTES_TO edges via IngestEdgeRepo")
    }

    pub async fn create_makes_http_call_edges(
        &self,
        repo_name: &str,
        calls: &[akashic_extraction::resolution::ResolvedEdge],
    ) -> Result<()> {
        use akashic_domain::types::IngestEdge;
        let edges: Vec<IngestEdge> = calls
            .iter()
            .map(|e| IngestEdge {
                src_chunk_id: e.src_chunk_id,
                tgt_chunk_id: e.tgt_chunk_id,
                confidence: e.confidence,
                method: e.method.clone(),
                ref_kind: e.ref_kind.clone(),
            })
            .collect();
        self.ingest_edge
            .create_makes_http_call_edges(repo_name, &edges)
            .await
            .context("Failed to create MAKES_HTTP_CALL edges via IngestEdgeRepo")
    }

    pub async fn create_symbol_import_edges(
        &self,
        repo_name: &str,
        imports: &[(String, uuid::Uuid)],
    ) -> Result<()> {
        self.ingest_edge
            .create_symbol_import_edges(repo_name, imports)
            .await
            .context("Failed to create import REFERENCES edges via IngestEdgeRepo")
    }

    /// Update ingestion job status in PG and broadcast an SSE event.
    ///
    /// Delegates the SQL write to `IngestionJobRepo::update_job_status` which
    /// fetches `total_files` internally and returns it so we can include it in
    /// the SSE broadcast without an extra query.
    pub async fn update_job_status(
        &self,
        job_id: Uuid,
        repo_name: &str,
        status: &str,
        processed_files: Option<i32>,
        total_chunks: Option<i32>,
        error_message: Option<&str>,
    ) -> Result<()> {
        // Delegate SQL to IngestionJobRepo port; returns total_files for SSE.
        let total_files = self
            .job_repo
            .update_job_status(job_id, status, processed_files, total_chunks, error_message)
            .await
            .context("Failed to update job status")?;

        // Broadcast job update event to all SSE clients (SSE logic stays here).
        let _ = self.event_tx.send(AppEvent::JobUpdate {
            repo_name: repo_name.to_string(),
            status: status.to_string(),
            processed_files,
            total_files,
            total_chunks,
        });

        Ok(())
    }

    /// Create a new ingestion job record.
    pub async fn create_job(&self, repo_name: &str, git_ref: &str) -> Result<Uuid> {
        self.job_repo
            .create_job(repo_name, git_ref)
            .await
            .context("Failed to create ingestion job")
    }

    /// Set total_files on a job.
    pub async fn set_total_files(&self, job_id: Uuid, total: i32) -> Result<()> {
        self.job_repo
            .set_total_files(job_id, total)
            .await
            .context("Failed to set total_files on ingestion job")
    }

    /// Insert a single large (multi-chunk) record into PG with its embedding.
    pub async fn insert_large_chunk(
        &self,
        repo_name: &str,
        module_path: &str,
        chunk_ids: &[Uuid],
        content: &str,
        git_ref: &str,
    ) -> Result<Uuid> {
        let embedding = self.embedder.embed(content).await?;
        // Delegate SQL to ChunkRepo port.
        self.chunks
            .insert_large_chunk(
                repo_name,
                module_path,
                chunk_ids,
                content,
                &embedding.vector,
                git_ref,
            )
            .await
            .context("Failed to insert large chunk")
    }

    /// Create sliding-window large chunks by combining consecutive chunk pairs.
    pub async fn store_large_chunks(
        &self,
        repo_name: &str,
        module_path: &str,
        git_ref: &str,
        chunk_ids: &[Uuid],
        chunks: &[RawChunk],
    ) -> Result<Vec<Uuid>> {
        let mut large_ids = Vec::new();

        if chunks.len() < 2 {
            return Ok(large_ids);
        }

        for i in 0..chunks.len() - 1 {
            let combined = format!("{}\n\n{}", chunks[i].content, chunks[i + 1].content);
            let ids = vec![chunk_ids[i], chunk_ids[i + 1]];
            let large_id = self
                .insert_large_chunk(repo_name, module_path, &ids, &combined, git_ref)
                .await?;
            large_ids.push(large_id);
        }

        Ok(large_ids)
    }
}

#[cfg(test)]
mod embed_text_tests {
    use super::build_contextual_embed_text;
    use akashic_extraction::{ChunkMetadata, RawChunk, Visibility};

    /// Build a RawChunk varying only the fields the embed-text builder reads.
    fn chunk(
        chunk_type: &str,
        name: &str,
        fqn: Option<&str>,
        parent_fqn: Option<&str>,
        signature: Option<&str>,
        doc: Option<&str>,
        content: &str,
    ) -> RawChunk {
        RawChunk {
            chunk_type: chunk_type.into(),
            name: name.into(),
            fqn: fqn.map(Into::into),
            parent_fqn: parent_fqn.map(Into::into),
            start_line: 1,
            end_line: 2,
            start_byte: 0,
            end_byte: content.len(),
            signature: signature.map(Into::into),
            content: content.into(),
            doc: doc.map(Into::into),
            receiver: None,
            is_async: false,
            is_static: false,
            is_const: false,
            is_exported: false,
            visibility: Visibility::Public,
            metadata: ChunkMetadata::default(),
        }
    }

    #[test]
    fn includes_doc_and_fqn() {
        let c = chunk(
            "method",
            "increment",
            Some("Counter.increment"),
            Some("Counter"),
            Some("fn increment(&mut self)"),
            Some("Bump the counter by one."),
            "self.count += 1;",
        );
        let t = build_contextual_embed_text("repo", "mod", "rust", &c);
        // fqn used in the header (not the bare name alone).
        assert!(t.contains("'Counter.increment'"), "fqn in header: {t}");
        // doc (richest signal) is present.
        assert!(t.contains("Bump the counter by one."), "doc present: {t}");
        // signature present.
        assert!(
            t.contains("fn increment(&mut self)"),
            "signature present: {t}"
        );
        // parent suffix suppressed — fqn already starts with "Counter".
        assert!(
            !t.contains(" in 'Counter'"),
            "redundant parent suppressed: {t}"
        );
    }

    #[test]
    fn signature_without_doc_has_no_doc_line() {
        let c = chunk(
            "function",
            "add",
            Some("add"),
            None,
            Some("fn add(a: i32, b: i32) -> i32"),
            None,
            "a + b",
        );
        let t = build_contextual_embed_text("repo", "mod", "rust", &c);
        assert!(
            t.contains("fn add(a: i32, b: i32) -> i32"),
            "signature: {t}"
        );
        // No doc, no content fallback (signature present), and no stray parent.
        assert!(!t.contains(" in '"), "no parent suffix: {t}");
    }

    #[test]
    fn falls_back_to_content_when_no_doc_or_signature() {
        let c = chunk(
            "config",
            "settings",
            None,
            None,
            None,
            None,
            "key = value\nport = 8080",
        );
        let t = build_contextual_embed_text("repo", "mod", "toml", &c);
        // Bare name in header (no fqn), content used as the body.
        assert!(t.contains("'settings'"), "name header: {t}");
        assert!(t.contains("key = value"), "content fallback: {t}");
    }

    #[test]
    fn parent_suffix_shown_when_not_redundant() {
        // fqn does NOT start with parent_fqn → suffix should appear.
        let c = chunk(
            "method",
            "render",
            Some("render"),
            Some("ui.Widget"),
            None,
            Some("Draw the widget."),
            "...",
        );
        let t = build_contextual_embed_text("repo", "mod", "ts", &c);
        assert!(
            t.contains(" in 'ui.Widget'"),
            "non-redundant parent shown: {t}"
        );
        assert!(t.contains("Draw the widget."), "doc present: {t}");
    }

    #[test]
    fn http_meta_reads_route_metadata_fields() {
        use akashic_extraction::MetadataValue;
        let mut meta = ChunkMetadata::default();
        meta.fields.insert(
            "http_method".to_string(),
            MetadataValue::String("GET".to_string()),
        );
        meta.fields.insert(
            "http_path".to_string(),
            MetadataValue::String("/api/x".to_string()),
        );
        let c = RawChunk {
            chunk_type: "route".into(),
            name: "GET /api/x".into(),
            fqn: None,
            parent_fqn: None,
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 0,
            signature: None,
            content: "route(\"/api/x\", get(h))".into(),
            doc: None,
            receiver: None,
            is_async: false,
            is_static: false,
            is_const: false,
            is_exported: true,
            visibility: Visibility::Public,
            metadata: meta,
        };
        assert_eq!(
            super::chunk_meta_str(&c, "http_method"),
            Some("GET".to_string())
        );
        assert_eq!(
            super::chunk_meta_str(&c, "http_path"),
            Some("/api/x".to_string())
        );

        let plain = RawChunk {
            metadata: ChunkMetadata::default(),
            ..c
        };
        assert_eq!(super::chunk_meta_str(&plain, "http_method"), None);
    }
}

#[cfg(test)]
mod tag_tests {
    use super::{extract_chunk_tags, split_camel_case};

    #[test]
    fn split_camel_case_breaks_on_uppercase_boundaries() {
        // Each capital starts a new word; the leading run is its own word.
        assert_eq!(split_camel_case("getUserId"), vec!["get", "User", "Id"]);
        assert_eq!(
            split_camel_case("HTTPServer"),
            vec!["H", "T", "T", "P", "Server"]
        );
        // No uppercase => single word; empty => no words.
        assert_eq!(split_camel_case("plain"), vec!["plain"]);
        assert!(split_camel_case("").is_empty());
    }

    #[test]
    fn extract_chunk_tags_splits_separators_and_lowercases() {
        // snake_case + camelCase are both split, words are lowercased, and the
        // chunk_type is appended as a tag. (Words of len <= 2 would be dropped,
        // but every word here is >= 3 chars.)
        let tags = extract_chunk_tags("get_userName", "function");
        assert!(tags.contains(&"get".to_string()), "snake word: {tags:?}");
        assert!(tags.contains(&"user".to_string()), "camel word: {tags:?}");
        assert!(tags.contains(&"name".to_string()), "camel word: {tags:?}");
        assert!(
            tags.contains(&"function".to_string()),
            "chunk_type tag: {tags:?}"
        );
        // Output is sorted and de-duplicated (sort+dedup in the impl).
        let mut sorted = tags.clone();
        sorted.sort();
        assert_eq!(tags, sorted, "tags are sorted: {tags:?}");
    }

    #[test]
    fn extract_chunk_tags_dedups_and_caps_at_ten() {
        // Repeated words collapse to one (chunk_type "render" also equals a
        // name word) and the result never exceeds the 10-tag cap.
        let dedup = extract_chunk_tags("render_render", "render");
        assert_eq!(
            dedup.iter().filter(|t| t.as_str() == "render").count(),
            1,
            "duplicate 'render' collapsed: {dedup:?}"
        );

        let many = extract_chunk_tags(
            "alpha_bravo_charlie_delta_echo_foxtrot_golf_hotel_india_juliet_kilo_lima",
            "function",
        );
        assert!(
            many.len() <= 10,
            "capped at 10 tags: {} ({many:?})",
            many.len()
        );
    }
}
