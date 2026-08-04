use super::*;

// ── SearchService impl ─────────────────────────────────────────────────────────

#[async_trait]
impl SearchService for RetrievalServices {
    async fn graphrag_query(&self, req: GraphRagQuery) -> DomainResult<GraphRagResponse> {
        // Delegate to the existing GraphRagService pipeline unchanged.
        // GraphRagQuery / GraphRagResponse are now domain types (re-exported in
        // akashic-retrieval::graphrag::types).
        let svc = self.graphrag_service();
        let response = svc.query(&req).await?;

        // Track access_count for notes returned in results (verbatim from MCP
        // search_knowledge tool — moved here so both REST graphrag_query and MCP
        // search_knowledge go through the same code path).
        for node in &response.nodes_used {
            if node.space == graphrag::types::Space::Human {
                let _ = self
                    .note_health_repo
                    .increment_access_count(node.pg_id)
                    .await;
            }
        }

        Ok(response)
    }

    async fn search_knowledge(
        &self,
        req: KnowledgeSearchRequest,
    ) -> DomainResult<Vec<KnowledgeSearchItem>> {
        // Dual-level vector search across chunks, notes, and sections — a
        // faithful reproduction of the legacy akashic-server unified_search
        // handler over the domain Scout repo ports. The Scout rows carry the
        // display columns (per-row repo name, chunk module path, note
        // category/summary, section document title) and — unlike the GraphRAG
        // note search — do NOT exclude archived notes.
        let query = &req.query;
        let repo = req.repo.as_deref();
        let limit = req.limit.clamp(1, 200);

        let layer = req.layer.as_deref().unwrap_or("all");
        let search_chunks = layer == "map" || layer == "all";
        let search_notes = layer == "note" || layer == "all";
        let search_sections = layer == "doc" || layer == "all";

        // Embed once, share across all three searches.
        let emb = self.embedder.embed(query).await?.vector;

        let mut results: Vec<KnowledgeSearchItem> = Vec::new();

        if search_chunks {
            let rows = self
                .chunk_repo
                .scout_search_chunks_by_vector(&emb, repo, limit)
                .await?;
            for r in rows {
                results.push(KnowledgeSearchItem {
                    id: r.id.to_string(),
                    layer: "MAP".into(),
                    result_type: r.chunk_type,
                    name: r.name,
                    repo_name: r.repo_name,
                    module: Some(r.module_path),
                    summary: None,
                    score: r.score,
                    docs: None,
                });
            }
        }

        if search_notes {
            let rows = self
                .note_repo
                .scout_search_notes_by_vector(&emb, repo, limit)
                .await?;
            for r in rows {
                results.push(KnowledgeSearchItem {
                    id: r.id.to_string(),
                    layer: "NOTE".into(),
                    result_type: r.category,
                    name: r.title.unwrap_or_else(|| "(untitled)".into()),
                    repo_name: r.repo_name,
                    module: None,
                    summary: r.summary,
                    score: r.score,
                    docs: None,
                });
            }
        }

        if search_sections {
            let rows = self
                .doc_repo
                .scout_search_sections_by_vector(&emb, repo, limit)
                .await?;
            for r in rows {
                // A DOC-layer hit backed by a docs-corpus page (`doc_type ==
                // "corpus"`) carries a deep link back to that page — build it
                // before moving `r.heading`/`r.repo_name` into the pushed item.
                let docs = if r.doc_type == "corpus" {
                    r.source_url.as_ref().map(|path| KnowledgeDocsRef {
                        repo: r.repo_name.clone(),
                        path: path.clone(),
                        anchor: akashic_domain::algos::corpus_contract::github_slug(&r.heading),
                    })
                } else {
                    None
                };
                results.push(KnowledgeSearchItem {
                    id: r.id.to_string(),
                    layer: "DOC".into(),
                    result_type: "section".into(),
                    name: r.heading,
                    repo_name: r.repo_name,
                    module: None,
                    summary: Some(r.doc_title),
                    score: r.score,
                    docs,
                });
            }
        }

        // Sort by score descending, truncate to limit.
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit as usize);
        Ok(results)
    }

    async fn get_details(
        &self,
        ids: Vec<Uuid>,
        repo: Option<String>,
    ) -> DomainResult<Vec<serde_json::Value>> {
        // Mode 1 — module list: no IDs, just repo.
        if ids.is_empty() {
            let repo_name = repo.ok_or_else(|| {
                DomainError::BadRequest("Either 'ids' or 'repo' query parameter required".into())
            })?;
            let rows = self.module_repo.list_modules(&repo_name, 100, 0).await?;
            let modules: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|m| {
                    serde_json::json!({
                        "id": m.id.to_string(),
                        "path": m.path,
                        "language": m.language,
                        "summary": m.summary,
                        "exports_count": m.exports_count,
                        "file_count": m.file_count,
                    })
                })
                .collect();
            return Ok(modules);
        }

        // Mode 2 — batch-fetch full detail for specific IDs.
        // Three batched queries (verbatim SQL from search.rs details handler),
        // then emit results in input order.
        let chunk_rows = self.chunk_repo.fetch_chunks_full_batch(&ids).await?;
        let note_rows = self.note_repo.fetch_notes_full_batch(&ids).await?;
        let section_rows = self.doc_repo.fetch_sections_detail_batch(&ids).await?;

        // Index by id for O(1) lookup.
        let mut by_id: std::collections::HashMap<Uuid, serde_json::Value> =
            std::collections::HashMap::with_capacity(ids.len());

        for r in chunk_rows {
            by_id.entry(r.id).or_insert_with(|| {
                serde_json::json!({
                    "id": r.id.to_string(),
                    "layer": "MAP",
                    "name": r.name,
                    "chunk_type": r.chunk_type,
                    "module_path": r.module_path,
                    "signature": r.signature,
                    "content": r.content,
                    "language": r.language,
                })
            });
        }

        for r in note_rows {
            by_id.entry(r.id).or_insert_with(|| {
                serde_json::json!({
                    "id": r.id.to_string(),
                    "layer": "NOTE",
                    "title": r.title,
                    "summary": r.summary,
                    "facts": r.facts,
                    "content": r.content,
                    "category": r.category,
                    "related_symbols": r.related_symbols,
                    "related_files": r.related_files,
                    "branch": r.branch,
                })
            });
        }

        for r in section_rows {
            by_id.entry(r.id).or_insert_with(|| {
                serde_json::json!({
                    "id": r.id.to_string(),
                    "layer": "DOC",
                    "heading": r.heading,
                    "content": r.content,
                    "document_title": r.document_title,
                    "depth": r.depth,
                    "tags": r.tags,
                })
            });
        }

        // Emit in request order; echo id string for not-found entries.
        let details: Vec<serde_json::Value> = ids
            .into_iter()
            .map(|id| {
                by_id.remove(&id).unwrap_or_else(|| {
                    serde_json::json!({
                        "id": id.to_string(),
                        "error": "Not found",
                    })
                })
            })
            .collect();

        Ok(details)
    }

    async fn global_query(
        &self,
        query: String,
        repo: String,
        level: Option<u8>,
        include_examples: bool,
        max_communities: usize,
        _prefer_space: Option<PreferSpace>,
    ) -> DomainResult<Vec<CommunitySummaryItem>> {
        let max_k = (max_communities as i64).clamp(1, 10);

        // Auto: pick the highest available summarised level.
        let target_level: i16 = if let Some(l) = level {
            l.min(2) as i16
        } else {
            self.community_repo
                .max_summarized_level(&repo)
                .await?
                .unwrap_or(0)
        };

        // Embed query once (verbatim from MCP global_query).
        let emb = self.embedder.embed(&query).await?.vector;

        let rows = self
            .community_repo
            .select_communities_by_vector(&repo, target_level, &emb, max_k)
            .await?;

        let mut items: Vec<CommunitySummaryItem> = Vec::with_capacity(rows.len());
        for row in rows {
            let examples = if include_examples {
                self.community_repo
                    .fetch_community_examples(row.id, 3)
                    .await
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            items.push(CommunitySummaryItem {
                id: row.id,
                name: row.name,
                summary: row.summary,
                level: row.level,
                member_count: row.member_count,
                score: row.score,
                examples,
            });
        }

        Ok(items)
    }

    async fn relink_explains(
        &self,
        repo: String,
        _user_token: Option<String>,
    ) -> DomainResult<RelinkResult> {
        use crate::linking::explains;

        // 1. Fetch all sections for the repo with their embeddings.
        //    SQL moved verbatim to DocumentRepo::fetch_sections_for_relink.
        let section_rows = self.doc_repo.fetch_sections_for_relink(&repo).await?;
        let total = section_rows.len();

        let mut exact_count: usize = 0;
        let mut llm_count: usize = 0;

        // 2. Process each section — deterministic first, LLM-verified fallback.
        //    Logic preserved verbatim from the original handler.
        for row in &section_rows {
            let exact = explains::create_deterministic_explains_edges(
                &self.symbol_repo,
                &self.edge_repo,
                row.id,
                &row.content,
                &repo,
            )
            .await
            .unwrap_or(0);

            exact_count += exact;

            if exact == 0
                && let Some(ref emb) = row.embedding
            {
                let llm_edges = explains::create_llm_verified_explains_edges(
                    &self.symbol_repo,
                    &self.edge_repo,
                    row.id,
                    &row.content,
                    &row.heading,
                    emb,
                    &repo,
                    self.llm.as_ref(),
                )
                .await
                .unwrap_or(0);

                llm_count += llm_edges;
            }
        }

        // 3. Collect target repos from Neo4j after relinking.
        //    Cypher moved verbatim to GraphReadRepo::explains_target_repos.
        //    NOTE: DocClustering re-cluster is intentionally left in the handler
        //    to avoid a retrieval→ingestion dependency cycle (akashic-ingestion
        //    already depends on akashic-retrieval for linking). The handler calls
        //    this method and then performs the cluster_repo_sections step itself.
        let target_repos = self
            .graph_read_repo
            .explains_target_repos(&repo)
            .await
            .unwrap_or_default();

        Ok(RelinkResult {
            sections_processed: total,
            edges_created_exact: exact_count,
            edges_created_llm: llm_count,
            target_repos,
        })
    }
}
