//! GraphService impl bodies — modules (graph.rs split). Inherent `gs_*` methods
//! that the thin `impl GraphService` in `super` delegates to.
use super::*;

impl RetrievalServices {
    pub(crate) async fn gs_list_modules(
        &self,
        repo: String,
        limit: i64,
        offset: i64,
    ) -> DomainResult<(i64, Vec<ModuleRow>)> {
        let total = self.module_repo.count_modules(&repo).await?;
        let rows = self.module_repo.list_modules(&repo, limit, offset).await?;
        Ok((total, rows))
    }

    pub(crate) async fn gs_list_module_chunks(
        &self,
        repo: String,
        module_path: String,
        limit: i64,
        offset: i64,
    ) -> DomainResult<Vec<ChunkRow>> {
        self.chunk_repo
            .list_chunks_in_module(&repo, &module_path, limit, offset)
            .await
            .map_err(DomainError::Internal)
    }

    pub(crate) async fn gs_get_chunk_detail(
        &self,
        repo: String,
        chunk_id: Uuid,
    ) -> DomainResult<Option<ChunkDetailRow>> {
        self.chunk_repo
            .get_chunk_detail(&repo, chunk_id)
            .await
            .map_err(DomainError::Internal)
    }

    pub(crate) async fn gs_get_module_detail(
        &self,
        module_id: Uuid,
    ) -> DomainResult<Option<String>> {
        // Faithful reproduction of the legacy graph/module.rs get_module_detail
        // handler over the existing repo ports. Neo4j pg_ids are matched on the
        // canonical UUID string (the handler matched on the raw URL string,
        // which mismatched for non-canonical input — fixed here).
        let module_id_str = module_id.to_string();

        // 1. Module metadata (id, path, repo) from PG. Missing module → 404.
        let Some((mod_uuid, mod_path, repo_name)) =
            self.module_repo.get_module_path_and_repo(module_id).await?
        else {
            return Ok(None);
        };
        let mod_name = mod_path.rsplit('/').next().unwrap_or(&mod_path).to_string();

        // 2. All chunks in the module (unbounded — sidebar drill-down).
        let chunk_rows = self
            .chunk_repo
            .list_chunks_in_module_all(&repo_name, &mod_path)
            .await?;

        // 3. Per-chunk ATTACHED_TO note pg_ids via Neo4j.
        let chunk_note_rows = self
            .graph_read_repo
            .get_chunk_note_ids(&module_id_str)
            .await?;
        let mut chunk_note_ids: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let mut all_note_id_strs: Vec<String> = Vec::new();
        for (chunk_id, note_ids) in chunk_note_rows {
            let note_ids: Vec<String> = note_ids.into_iter().filter(|s| !s.is_empty()).collect();
            all_note_id_strs.extend(note_ids.iter().cloned());
            chunk_note_ids.insert(chunk_id, note_ids);
        }

        // 4. Batch-fetch chunk-note details (dedup ids — no N+1).
        let unique_note_ids: Vec<Uuid> = all_note_id_strs
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .filter_map(|s| s.parse::<Uuid>().ok())
            .collect();
        let note_lookup: std::collections::HashMap<String, serde_json::Value> =
            if unique_note_ids.is_empty() {
                std::collections::HashMap::new()
            } else {
                self.note_repo
                    .fetch_notes_brief_batch(&unique_note_ids)
                    .await?
                    .into_iter()
                    .map(|n| {
                        let key = n.id.to_string();
                        let item = serde_json::json!({
                            "id": key,
                            "title": n.title.unwrap_or_else(|| "(untitled)".into()),
                            "category": n.category,
                        });
                        (key, item)
                    })
                    .collect()
            };

        // 5. Assemble chunk items, each carrying its ordered attached notes.
        let chunks: Vec<serde_json::Value> = chunk_rows
            .into_iter()
            .map(|c| {
                let cid_str = c.id.to_string();
                let notes: Vec<serde_json::Value> = chunk_note_ids
                    .get(&cid_str)
                    .map(|ids| {
                        ids.iter()
                            .filter_map(|nid| note_lookup.get(nid).cloned())
                            .collect()
                    })
                    .unwrap_or_default();
                serde_json::json!({
                    "id": cid_str,
                    "name": c.name,
                    "chunk_type": c.chunk_type,
                    "signature": c.signature,
                    "content": c.content,
                    "language": c.language,
                    "notes": notes,
                })
            })
            .collect();

        // 6. Module-level notes via Neo4j ATTACHED_TO + batch detail.
        let module_note_id_strs = self
            .graph_read_repo
            .get_module_note_ids(&module_id_str)
            .await?;
        let note_pg_ids: Vec<Uuid> = module_note_id_strs
            .iter()
            .filter_map(|s| s.parse::<Uuid>().ok())
            .collect();
        let notes: Vec<serde_json::Value> = if note_pg_ids.is_empty() {
            Vec::new()
        } else {
            self.note_repo
                .fetch_notes_detail_batch(&note_pg_ids)
                .await?
                .into_iter()
                .map(|n| {
                    serde_json::json!({
                        "id": n.id.to_string(),
                        "title": n.title.unwrap_or_else(|| "(untitled)".into()),
                        "category": n.category,
                        "summary": n.summary.unwrap_or_default(),
                    })
                })
                .collect()
        };

        let chunk_count = chunks.len() as i64;
        let note_count = notes.len() as i64;

        let view = serde_json::json!({
            "module": {
                "id": mod_uuid.to_string(),
                "path": mod_path,
                "name": mod_name,
                "chunk_count": chunk_count,
                "note_count": note_count,
            },
            "chunks": chunks,
            "notes": notes,
        });
        Ok(Some(view.to_string()))
    }

    // ── GET /api/v1/modules/:id/call-graph ───────────────────────────────────

    pub(crate) async fn gs_get_module_call_graph(&self, module_id: Uuid) -> DomainResult<String> {
        // 1. Module metadata from PG.
        let row = self
            .module_repo
            .get_module_path_and_repo(module_id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("module {module_id}")))?;
        let (mod_uuid, mod_path, repo_name) = row;
        let mod_name = mod_path.rsplit('/').next().unwrap_or(&mod_path).to_string();
        let module_id_str = module_id.to_string();

        // 2. Intra-module CALLS edges from Neo4j (verbatim from module.rs).
        let calls_rows = self
            .graph_read_repo
            .get_intra_module_calls(&module_id_str)
            .await?;

        let mut calls: Vec<serde_json::Value> = Vec::new();
        let mut internal_pg_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for cr in &calls_rows {
            internal_pg_ids.insert(cr.source.clone());
            internal_pg_ids.insert(cr.target.clone());
            calls.push(serde_json::json!({
                "source": format!("chunk:{}", cr.source),
                "target": format!("chunk:{}", cr.target),
                "confidence": cr.confidence,
                "method": cr.method,
            }));
        }

        // 3. All chunks belonging to this module (PG).
        let chunk_rows = self
            .chunk_repo
            .list_chunks_in_module_all(&repo_name, &mod_path)
            .await?;

        let chunks: Vec<serde_json::Value> = chunk_rows
            .into_iter()
            .map(|c| {
                let line_count = c.content.chars().filter(|ch| *ch == '\n').count() as i64 + 1;
                serde_json::json!({
                    "id": format!("chunk:{}", c.id),
                    "name": c.name,
                    "chunk_type": c.chunk_type,
                    "signature": c.signature,
                    "line_count": line_count,
                })
            })
            .collect();

        // 4. Ghost nodes (one hop out via CALLS).
        let ghost_rows = self.graph_read_repo.get_ghost_nodes(&module_id_str).await?;

        let ghost_pg_id_strs: Vec<String> = ghost_rows.iter().map(|g| g.id.clone()).collect();

        // 5. Resolve ghost metadata from PG.
        let mut ghost_module_map: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();
        let mut ghost_type_map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        if !ghost_pg_id_strs.is_empty() {
            let ghost_uuids: Vec<Uuid> = ghost_pg_id_strs
                .iter()
                .filter_map(|s| s.parse::<Uuid>().ok())
                .collect();

            let ghost_chunk_rows = self.chunk_repo.get_chunk_module_batch(&ghost_uuids).await?;

            let mut ghost_module_paths: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for gr in &ghost_chunk_rows {
                ghost_type_map.insert(gr.id.to_string(), gr.chunk_type.clone());
                ghost_module_paths.insert(gr.module_path.clone());
            }

            if !ghost_module_paths.is_empty() {
                let paths_vec: Vec<String> = ghost_module_paths.into_iter().collect();
                let mod_rows = self
                    .module_repo
                    .resolve_module_paths(&repo_name, &paths_vec)
                    .await?;
                let path_to_mod: std::collections::HashMap<String, String> = mod_rows
                    .into_iter()
                    .map(|(path, mid)| (path, format!("mod:{mid}")))
                    .collect();

                for gr in &ghost_chunk_rows {
                    if let Some(mod_id) = path_to_mod.get(&gr.module_path) {
                        ghost_module_map
                            .insert(gr.id.to_string(), (mod_id.clone(), gr.module_path.clone()));
                    }
                }
            }
        }

        let external: Vec<serde_json::Value> = ghost_rows
            .into_iter()
            .map(|g| {
                let (module_node_id, module_path) = ghost_module_map
                    .get(&g.id)
                    .cloned()
                    .unwrap_or_else(|| (String::new(), String::new()));
                let chunk_type = ghost_type_map
                    .get(&g.id)
                    .cloned()
                    .unwrap_or_else(|| "function".into());
                serde_json::json!({
                    "id": format!("chunk:{}", g.id),
                    "name": g.name,
                    "chunk_type": chunk_type,
                    "module_path": module_path,
                    "module_id": module_node_id,
                    "direction": g.direction,
                })
            })
            .collect();

        // 6. Cross-module CALLS edges for ghost nodes.
        let caller_edge_rows = self
            .graph_read_repo
            .get_caller_edges(&module_id_str)
            .await?;
        for ce in caller_edge_rows {
            calls.push(serde_json::json!({
                "source": format!("chunk:{}", ce.source),
                "target": format!("chunk:{}", ce.target),
                "confidence": ce.confidence,
                "method": ce.method,
            }));
        }

        let callee_edge_rows = self
            .graph_read_repo
            .get_callee_edges(&module_id_str)
            .await?;
        for ce in callee_edge_rows {
            calls.push(serde_json::json!({
                "source": format!("chunk:{}", ce.source),
                "target": format!("chunk:{}", ce.target),
                "confidence": ce.confidence,
                "method": ce.method,
            }));
        }

        let result = serde_json::json!({
            "module": {
                "id": format!("mod:{mod_uuid}"),
                "path": mod_path,
                "name": mod_name,
            },
            "chunks": chunks,
            "calls": calls,
            "external": external,
        });

        serde_json::to_string_pretty(&result).map_err(Into::into)
    }

    // ── GET /api/v1/doc-graph/:repo ──────────────────────────────────────────
}
