//! GraphService impl bodies — docs (graph.rs split). Inherent `gs_*` methods
//! that the thin `impl GraphService` in `super` delegates to.
use super::*;

impl RetrievalServices {
    pub(crate) async fn gs_get_doc_graph(&self, repo: String) -> DomainResult<String> {
        // Try cluster-based graph first (verbatim from doc.rs: get_doc_graph).
        let cluster_rows = self.doc_cluster_repo.list_clusters_by_repo(&repo).await?;

        let result: serde_json::Value = if !cluster_rows.is_empty() {
            // Cluster-based path.
            let cluster_explains = self.graph_read_repo.get_cluster_explains(&repo).await?;

            let mut explains_per_cluster: std::collections::HashMap<String, i64> =
                std::collections::HashMap::new();
            let mut explains_summary: std::collections::HashMap<String, i64> =
                std::collections::HashMap::new();

            for row in cluster_explains {
                *explains_per_cluster
                    .entry(row.cluster_id.clone())
                    .or_insert(0) += 1;
                *explains_summary.entry(row.target_repo).or_insert(0) += 1;
            }

            let mut nodes = vec![serde_json::json!({
                "id": repo,
                "label": repo,
                "path": "",
                "section_count": 0i64,
                "explains_count": 0i64,
                "is_virtual": false,
                "language": "repository",
            })];
            let mut edges: Vec<serde_json::Value> = Vec::new();

            for cr in &cluster_rows {
                let cid_str = cr.id.to_string();
                let cluster_node_id = format!("cluster:{cid_str}");
                nodes.push(serde_json::json!({
                    "id": cluster_node_id,
                    "label": cr.name,
                    "path": cr.name,
                    "section_count": cr.section_count as i64,
                    "explains_count": explains_per_cluster.get(&cid_str).copied().unwrap_or(0i64),
                    "is_virtual": false,
                    "language": "topic",
                }));
                edges.push(serde_json::json!({
                    "source": repo,
                    "target": format!("cluster:{cid_str}"),
                }));
            }

            serde_json::json!({
                "nodes": nodes,
                "edges": edges,
                "explains_summary": explains_summary,
            })
        } else {
            // Document-based fallback path.
            let doc_rows = self.graph_read_repo.get_doc_nodes(&repo).await?;

            let mut pg_ids: Vec<String> = Vec::new();
            let mut neo_docs: Vec<(String, String)> = Vec::new();
            for dr in &doc_rows {
                pg_ids.push(dr.pg_id.clone());
                neo_docs.push((dr.pg_id.clone(), dr.title.clone()));
            }

            let uuids: Vec<Uuid> = pg_ids
                .iter()
                .filter_map(|s| s.parse::<Uuid>().ok())
                .collect();

            // Section counts per doc.
            let sec_count_rows = self.doc_repo.count_sections_per_doc(&uuids).await?;
            let section_count_map: std::collections::HashMap<String, i64> = sec_count_rows
                .into_iter()
                .map(|(id, cnt)| (id.to_string(), cnt))
                .collect();

            // Source URLs.
            let source_url_rows = self.doc_repo.get_source_urls_by_repo(&repo).await?;
            let source_url_map: std::collections::HashMap<String, Option<String>> = source_url_rows
                .into_iter()
                .map(|(id, url)| (id.to_string(), url))
                .collect();

            // EXPLAINS edges (doc-based).
            let doc_explains = self.graph_read_repo.get_doc_explains(&repo).await?;
            let mut explains_per_doc: std::collections::HashMap<String, i64> =
                std::collections::HashMap::new();
            let mut explains_summary: std::collections::HashMap<String, i64> =
                std::collections::HashMap::new();
            for de in doc_explains {
                *explains_per_doc.entry(de.doc_id).or_insert(0) += 1;
                *explains_summary.entry(de.target_repo).or_insert(0) += 1;
            }

            let mut nodes = vec![serde_json::json!({
                "id": repo,
                "label": repo,
                "path": "",
                "section_count": 0i64,
                "explains_count": 0i64,
                "is_virtual": false,
                "language": "repository",
            })];
            let mut edges: Vec<serde_json::Value> = Vec::new();

            for (pg_id, title) in &neo_docs {
                let doc_id = format!("doc:{pg_id}");
                let source_url = source_url_map
                    .get(pg_id)
                    .and_then(std::clone::Clone::clone)
                    .unwrap_or_default();

                nodes.push(serde_json::json!({
                    "id": doc_id,
                    "label": title,
                    "path": if source_url.is_empty() { title.clone() } else { source_url },
                    "section_count": section_count_map.get(pg_id).copied().unwrap_or(0i64),
                    "explains_count": explains_per_doc.get(pg_id).copied().unwrap_or(0i64),
                    "is_virtual": false,
                    "language": "document",
                }));
                edges.push(serde_json::json!({
                    "source": repo,
                    "target": format!("doc:{pg_id}"),
                }));
            }

            serde_json::json!({
                "nodes": nodes,
                "edges": edges,
                "explains_summary": explains_summary,
            })
        };

        serde_json::to_string_pretty(&result).map_err(Into::into)
    }

    // ── GET /api/v1/documents/:id/sections ───────────────────────────────────

    pub(crate) async fn gs_get_document_detail(&self, doc_id: Uuid) -> DomainResult<String> {
        // 1. Document metadata from PG.
        let doc = self
            .doc_repo
            .get_document_by_id(doc_id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("document {doc_id}")))?;

        // 2. Sections from PG.
        let section_rows = self.doc_repo.get_sections_by_doc(doc_id).await?;
        let section_ids: Vec<String> = section_rows.iter().map(|s| s.id.to_string()).collect();
        let section_count = section_rows.len() as i64;

        // 3. Batch EXPLAINS (UNWIND — N+1 fix preserved).
        let explains_rows = self
            .graph_read_repo
            .batch_section_explains(section_ids)
            .await?;

        let mut explains_map: std::collections::HashMap<
            String,
            Vec<(String, f64, String, String)>,
        > = std::collections::HashMap::new();
        for er in explains_rows {
            explains_map.entry(er.section_id).or_default().push((
                er.target_id,
                er.confidence,
                er.method,
                er.target_repo,
            ));
        }
        let total_explains: i64 = explains_map.values().map(|v| v.len() as i64).sum();

        // 4. Batch chunk metadata for EXPLAINS targets.
        let all_target_ids: Vec<Uuid> = explains_map
            .values()
            .flatten()
            .filter_map(|(tid, ..)| tid.parse::<Uuid>().ok())
            .collect();

        let chunk_meta_map: std::collections::HashMap<String, (String, String, String)> =
            if all_target_ids.is_empty() {
                std::collections::HashMap::new()
            } else {
                self.chunk_repo
                    .fetch_chunks_meta_batch(&all_target_ids)
                    .await?
                    .into_iter()
                    .map(|(cid, name, ctype, mpath)| (cid.to_string(), (name, ctype, mpath)))
                    .collect()
            };

        // 5. Assemble sections with explains targets.
        let sections: Vec<serde_json::Value> = section_rows
            .into_iter()
            .map(|s| {
                let sid_str = s.id.to_string();
                let entries = explains_map.get(&sid_str).cloned().unwrap_or_default();
                let explains: Vec<serde_json::Value> = entries
                    .into_iter()
                    .map(|(target_id, confidence, method, target_repo)| {
                        let (name, ctype, mpath) = chunk_meta_map
                            .get(&target_id)
                            .cloned()
                            .unwrap_or_else(|| ("unknown".into(), "unknown".into(), String::new()));
                        serde_json::json!({
                            "chunk_id": target_id,
                            "chunk_name": name,
                            "chunk_type": ctype,
                            "repo_name": target_repo,
                            "module_path": mpath,
                            "confidence": confidence,
                            "method": method,
                        })
                    })
                    .collect();

                serde_json::json!({
                    "id": sid_str,
                    "parent_id": s.parent_id.map(|p| p.to_string()),
                    "heading": s.heading,
                    "depth": s.depth as i32,
                    "content": s.content,
                    "tags": s.tags,
                    "explains": explains,
                })
            })
            .collect();

        let result = serde_json::json!({
            "document": {
                "id": doc.id.to_string(),
                "title": doc.title,
                "doc_type": doc.doc_type,
                "source_url": doc.source_url,
                "section_count": section_count,
                "explains_count": total_explains,
            },
            "sections": sections,
        });

        serde_json::to_string_pretty(&result).map_err(Into::into)
    }

    // ── GET /api/v1/clusters/:id/sections ────────────────────────────────────

    pub(crate) async fn gs_get_cluster_detail(&self, cluster_id: Uuid) -> DomainResult<String> {
        // 1. Cluster metadata from PG.
        let cluster = self
            .doc_cluster_repo
            .get_cluster_by_id(cluster_id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("cluster {cluster_id}")))?;

        // 2. Sections for this cluster from PG.
        let section_rows = self.doc_repo.get_sections_by_cluster(cluster_id).await?;
        let section_ids: Vec<String> = section_rows.iter().map(|s| s.id.to_string()).collect();

        // 3. Batch EXPLAINS (UNWIND — N+1 fix preserved).
        let explains_rows = self
            .graph_read_repo
            .batch_section_explains(section_ids)
            .await?;

        let mut explains_map: std::collections::HashMap<
            String,
            Vec<(String, f64, String, String)>,
        > = std::collections::HashMap::new();
        for er in explains_rows {
            explains_map.entry(er.section_id).or_default().push((
                er.target_id,
                er.confidence,
                er.method,
                er.target_repo,
            ));
        }
        let total_explains: i64 = explains_map.values().map(|v| v.len() as i64).sum();

        // 4. Batch chunk metadata for EXPLAINS targets.
        let all_target_ids: Vec<Uuid> = explains_map
            .values()
            .flatten()
            .filter_map(|(tid, ..)| tid.parse::<Uuid>().ok())
            .collect();

        let chunk_meta_map: std::collections::HashMap<String, (String, String, String)> =
            if all_target_ids.is_empty() {
                std::collections::HashMap::new()
            } else {
                self.chunk_repo
                    .fetch_chunks_meta_batch(&all_target_ids)
                    .await?
                    .into_iter()
                    .map(|(cid, cname, ctype, mpath)| (cid.to_string(), (cname, ctype, mpath)))
                    .collect()
            };

        // 5. Assemble sections with explains targets.
        let sections: Vec<serde_json::Value> = section_rows
            .into_iter()
            .map(|s| {
                let sid_str = s.id.to_string();
                let entries = explains_map.get(&sid_str).cloned().unwrap_or_default();
                let explains: Vec<serde_json::Value> = entries
                    .into_iter()
                    .map(|(target_id, confidence, method, target_repo)| {
                        let (cname, ctype, mpath) = chunk_meta_map
                            .get(&target_id)
                            .cloned()
                            .unwrap_or_else(|| ("unknown".into(), "unknown".into(), String::new()));
                        serde_json::json!({
                            "chunk_id": target_id,
                            "chunk_name": cname,
                            "chunk_type": ctype,
                            "repo_name": target_repo,
                            "module_path": mpath,
                            "confidence": confidence,
                            "method": method,
                        })
                    })
                    .collect();

                serde_json::json!({
                    "id": sid_str,
                    "parent_id": s.parent_id.map(|p| p.to_string()),
                    "heading": s.heading,
                    "depth": s.depth as i32,
                    "content": s.content,
                    "tags": s.tags,
                    "explains": explains,
                })
            })
            .collect();

        let result = serde_json::json!({
            "document": {
                "id": cluster_id.to_string(),
                "title": cluster.name,
                "doc_type": "cluster",
                "source_url": serde_json::Value::Null,
                "section_count": cluster.section_count as i64,
                "explains_count": total_explains,
            },
            "sections": sections,
        });

        serde_json::to_string_pretty(&result).map_err(Into::into)
    }

    // ── GET /api/v1/docs/:repo/:version/page/*path (Task 10) ─────────────────

    pub(crate) async fn gs_find_corpus_document_id(
        &self,
        repo: String,
        path: String,
    ) -> DomainResult<Option<Uuid>> {
        Ok(self
            .doc_repo
            .get_document_id_by_source(&repo, "corpus", &path)
            .await?)
    }

    // ── MCP: get_project_summary ──────────────────────────────────────────────
}
