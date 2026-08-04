//! GraphService impl bodies — view (graph.rs split). Inherent `gs_*` methods
//! that the thin `impl GraphService` in `super` delegates to.
use super::*;

impl RetrievalServices {
    pub(crate) async fn gs_get_graph(&self, repo: String) -> DomainResult<GraphView> {
        // Five independent Neo4j reads over the same repo — run them
        // concurrently instead of five sequential round-trips (this is the
        // primary UI graph endpoint). Errors still propagate (finding #38/#39).
        let (branch_rows, note_rows, module_rows, chunk_rows, import_rows) = tokio::try_join!(
            self.graph_read_repo.get_branches(&repo),
            self.graph_read_repo.get_graph_notes(&repo),
            self.graph_read_repo.get_graph_modules(&repo),
            self.graph_read_repo.get_graph_chunks(&repo),
            self.graph_read_repo.get_import_edges(&repo),
        )?;

        let mut nodes = vec![GraphNode {
            id: repo.clone(),
            label: repo.clone(),
            node_type: "Repository".into(),
        }];
        let mut edges: Vec<GraphEdge> = Vec::new();

        // Branch nodes + edges
        for br in branch_rows {
            let id = format!("branch:{}", br.name);
            nodes.push(GraphNode {
                id: id.clone(),
                label: br.name,
                node_type: "Branch".into(),
            });
            edges.push(GraphEdge {
                source: repo.clone(),
                target: id,
                edge_type: "HAS_BRANCH".into(),
            });
        }

        // Note nodes + edges
        for nr in note_rows {
            let ctx_id = format!("ctx:{}", nr.uuid);
            nodes.push(GraphNode {
                id: ctx_id.clone(),
                label: nr.uuid[..8.min(nr.uuid.len())].to_string(),
                node_type: "Note".into(),
            });
            if let Some(branch) = nr.branch {
                let branch_id = format!("branch:{branch}");
                edges.push(GraphEdge {
                    source: ctx_id.clone(),
                    target: branch_id,
                    edge_type: "LINKED_TO".into(),
                });
            }
            if let Some(category) = nr.category {
                let cat_id = format!("cat:{category}");
                edges.push(GraphEdge {
                    source: ctx_id,
                    target: cat_id.clone(),
                    edge_type: "TAGGED_AS".into(),
                });
                if !nodes.iter().any(|n| n.id == cat_id) {
                    nodes.push(GraphNode {
                        id: cat_id,
                        label: category,
                        node_type: "Category".into(),
                    });
                }
            }
        }

        // Module nodes + edges
        for mr in module_rows {
            let mod_id = format!("mod:{}", mr.pg_id);
            nodes.push(GraphNode {
                id: mod_id.clone(),
                label: mr.path,
                node_type: "Module".into(),
            });
            edges.push(GraphEdge {
                source: repo.clone(),
                target: mod_id,
                edge_type: "HAS_MODULE".into(),
            });
        }

        // Chunk nodes + edges
        for cr in chunk_rows {
            let chunk_id = format!("chunk:{}", cr.pg_id);
            let mod_id = format!("mod:{}", cr.module_id);
            nodes.push(GraphNode {
                id: chunk_id.clone(),
                label: cr.name,
                node_type: "Chunk".into(),
            });
            edges.push(GraphEdge {
                source: mod_id,
                target: chunk_id,
                edge_type: "HAS_CHUNK".into(),
            });
        }

        // IMPORTS_FROM edges
        for ie in import_rows {
            edges.push(GraphEdge {
                source: format!("mod:{}", ie.src_id),
                target: format!("mod:{}", ie.tgt_id),
                edge_type: "IMPORTS_FROM".into(),
            });
        }

        Ok(GraphView { nodes, edges })
    }

    // ── GET /api/v1/god-nodes/:repo ──────────────────────────────────────────

    pub(crate) async fn gs_get_god_nodes(&self, repo: String) -> DomainResult<GodNodesResult> {
        // Three Neo4j queries (verbatim from core.rs: get_god_nodes).
        // Both module and chunk god-node queries use unwrap_or_default (verbatim).
        let module_rows = self.graph_read_repo.get_module_god_nodes(&repo).await?;
        let chunk_rows = self.graph_read_repo.get_chunk_god_nodes(&repo).await?;
        let total_nodes = self.graph_read_repo.get_total_node_count(&repo).await?;

        let mut god_nodes: Vec<GodNodeItem> = Vec::new();

        for mr in module_rows {
            let name = mr.path.rsplit('/').next().unwrap_or(&mr.path).to_string();
            god_nodes.push(GodNodeItem {
                id: format!("mod:{}", mr.pg_id),
                name,
                path: mr.path,
                degree: mr.total_deg,
                node_type: "module".into(),
                connectivity: mr.import_deg,
                composition: mr.chunk_count,
            });
        }

        for cr in chunk_rows {
            god_nodes.push(GodNodeItem {
                id: format!("chunk:{}", cr.pg_id),
                name: cr.name,
                path: cr.path,
                degree: cr.total_calls,
                node_type: "chunk".into(),
                connectivity: cr.in_calls,
                composition: cr.out_calls,
            });
        }

        // Sort by degree descending, take top 10 (verbatim from handler).
        god_nodes.sort_by_key(|n| std::cmp::Reverse(n.degree));
        god_nodes.truncate(10);

        Ok(GodNodesResult {
            god_nodes,
            total_nodes,
        })
    }

    // ── GET /api/v1/graph/:repo/modules ──────────────────────────────────────

    pub(crate) async fn gs_get_module_graph(&self, repo: String) -> DomainResult<ModuleGraphView> {
        // Step 1: Neo4j module nodes.
        let module_rows = self.graph_read_repo.get_module_nodes(&repo).await?;

        let mut pg_id_strs: Vec<String> = Vec::new();
        let mut neo_modules: Vec<(String, String)> = Vec::new(); // (pg_id, path)
        for mr in &module_rows {
            pg_id_strs.push(mr.pg_id.clone());
            neo_modules.push((mr.pg_id.clone(), mr.path.clone()));
        }

        // Step 2: Batch-fetch PG module metadata (language, is_virtual).
        let uuids: Vec<Uuid> = pg_id_strs
            .iter()
            .filter_map(|s| s.parse::<Uuid>().ok())
            .collect();
        let pg_meta = self.module_repo.get_module_meta_batch(&uuids).await?;
        let pg_map: std::collections::HashMap<String, (Option<String>, bool)> = pg_meta
            .into_iter()
            .map(|r| (r.id.to_string(), (r.language, r.is_virtual)))
            .collect();

        // Step 3: Chunk counts per module (via path resolution).
        let path_rows = self.module_repo.get_module_paths_batch(&uuids).await?;
        let paths: Vec<String> = path_rows.iter().map(|(_, p)| p.clone()).collect();
        let path_to_id: std::collections::HashMap<String, String> = path_rows
            .into_iter()
            .map(|(id, p)| (p, id.to_string()))
            .collect();

        let count_rows = self
            .chunk_repo
            .count_chunks_per_module(&repo, &paths)
            .await?;
        let chunk_count_map: std::collections::HashMap<String, i64> = count_rows
            .into_iter()
            .filter_map(|(mp, cnt)| path_to_id.get(&mp).map(|id| (id.clone(), cnt)))
            .collect();

        // Step 4: Note counts per module (Neo4j ATTACHED_TO).
        let note_count_rows = self.graph_read_repo.get_module_note_counts(&repo).await?;
        let note_count_map: std::collections::HashMap<String, i64> = note_count_rows
            .into_iter()
            .map(|r| (r.pg_id, r.note_count))
            .collect();

        // Step 5: Build nodes — Repository root at index 0.
        let mut nodes = vec![ModuleGraphNode {
            id: repo.clone(),
            label: repo.clone(),
            path: String::new(),
            chunk_count: 0,
            note_count: 0,
            is_virtual: false,
            language: "repository".into(),
        }];
        let mut edges: Vec<GraphEdge> = Vec::new();

        for (pg_id, path) in &neo_modules {
            let (language, is_virtual) = pg_map.get(pg_id).cloned().unwrap_or((None, false));
            let label = path.rsplit('/').next().unwrap_or(path).to_string();
            let mod_id = format!("mod:{pg_id}");

            nodes.push(ModuleGraphNode {
                id: mod_id.clone(),
                label,
                path: path.clone(),
                chunk_count: chunk_count_map.get(pg_id).copied().unwrap_or(0),
                note_count: note_count_map.get(pg_id).copied().unwrap_or(0),
                is_virtual,
                language: language.unwrap_or_else(|| "unknown".into()),
            });

            edges.push(GraphEdge {
                source: repo.clone(),
                target: mod_id,
                edge_type: "HAS_MODULE".into(),
            });
        }

        // Step 6: IMPORTS_FROM edges (module.rs uses unwrap_or_default — verbatim).
        let import_rows = self.graph_read_repo.get_module_import_edges(&repo).await?;
        for ie in import_rows {
            edges.push(GraphEdge {
                source: format!("mod:{}", ie.src_id),
                target: format!("mod:{}", ie.tgt_id),
                edge_type: "IMPORTS_FROM".into(),
            });
        }

        // Total CALLS count.
        let calls_count = self.graph_read_repo.get_calls_count(&repo).await?;

        // Step 7: Saga groups — N+1 fix: batch-fetch statuses in one PG query.
        let saga_group_rows = self.graph_read_repo.get_saga_group_rows(&repo).await?;

        let mut pending_groups: Vec<(String, String, Vec<String>)> = Vec::new();
        for sg in saga_group_rows {
            if sg.module_ids.len() >= 2 {
                pending_groups.push((sg.saga_id, sg.saga_name, sg.module_ids));
            }
        }

        let saga_uuids: Vec<Uuid> = pending_groups
            .iter()
            .filter_map(|(saga_id, _, _)| saga_id.parse::<Uuid>().ok())
            .collect();

        let saga_status_map: std::collections::HashMap<String, String> = if saga_uuids.is_empty() {
            std::collections::HashMap::new()
        } else {
            self.saga_repo
                .get_saga_statuses_batch(&saga_uuids)
                .await?
                .into_iter()
                .map(|(id, status)| (id.to_string(), status))
                .collect()
        };

        let saga_groups: Vec<SagaGroupItem> = pending_groups
            .into_iter()
            .map(|(saga_id, saga_name, mod_ids)| {
                let status = saga_status_map
                    .get(&saga_id)
                    .cloned()
                    .unwrap_or_else(|| "active".into());
                SagaGroupItem {
                    saga_id,
                    name: saga_name,
                    status,
                    module_ids: mod_ids.into_iter().map(|id| format!("mod:{id}")).collect(),
                }
            })
            .collect();

        Ok(ModuleGraphView {
            nodes,
            edges,
            calls_count,
            saga_groups,
        })
    }

    // ── GET /api/v1/modules/:id/chunks ───────────────────────────────────────
}
