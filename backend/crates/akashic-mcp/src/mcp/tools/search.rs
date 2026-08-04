//! search MCP tools (Slice E tools.rs split). One of the composed
//! `#[tool_router]` blocks; combined in `super`'s `AkashicMcp::new`.
use super::*;

#[tool_router(router = search_tools, vis = "pub(crate)")]
impl AkashicMcp {
    // ══════════════════════════════════════════════════════════════════
    // search_knowledge — The Scout (dual-layer search, compact results)
    // ══════════════════════════════════════════════════════════════════

    /// Search across code structure (Map) and knowledge notes (Notes).
    #[tool(
        name = "search_knowledge",
        description = "Dual-level (BM25 keyword + vector semantic) search across code, documentation, and knowledge notes. \
Automatically detects query type to balance exact symbol matching with conceptual search. \
Returns compact index — use get_details for full content. Optional prefer_space to boost a specific space."
    )]
    async fn search_knowledge(
        &self,
        Parameters(args): Parameters<SearchKnowledgeArgs>,
    ) -> Result<String, String> {
        // ── Auto-explore detection ──────────────────────────────────────
        let mut mode = args.mode.clone();
        if mode.is_none() {
            let word_count = args.query.split_whitespace().count();
            let q_lower = args.query.trim().to_lowercase();
            if args.repo.is_some()
                && (word_count < 3
                    || q_lower.contains("overview")
                    || q_lower.contains("summary")
                    || q_lower == "what is this")
            {
                mode = Some("explore".to_string());
            }
        }

        // ── Mode routing (override prefer_space/limit via local vars) ───
        let mut prefer_space = args.prefer_space.clone();
        let mut limit_override = args.limit;

        if let Some(ref m) = mode {
            match m.as_str() {
                "explore" => {
                    let repo = args.repo.as_deref().unwrap_or("");
                    if repo.is_empty() {
                        return Err("repo is required for explore mode".into());
                    }
                    let chunk_repo: Arc<dyn ChunkRepo> =
                        Arc::new(PgChunkRepo::new(self.pg.clone()));
                    let module_repo: Arc<dyn ModuleRepo> =
                        Arc::new(PgModuleRepo::new(self.pg.clone()));
                    let note_health: Arc<dyn NoteHealthRepo> =
                        Arc::new(PgNoteHealthRepo::new(self.pg.clone()));
                    let saga_repo: Arc<dyn SagaRepo> = Arc::new(PgSagaRepo::new(self.pg.clone()));
                    let l0 = akashic_curation::notes::memory_stack::build_l0(
                        &chunk_repo,
                        &module_repo,
                        &note_health,
                        &saga_repo,
                        repo,
                    )
                    .await
                    .unwrap_or_default();
                    let l1 =
                        akashic_curation::notes::memory_stack::build_l1(&note_health, repo, 10)
                            .await
                            .unwrap_or_default();
                    return Ok(format!("{l0}\n\n{l1}"));
                }
                "saga" => {
                    let repo = args.repo.as_deref().unwrap_or("");
                    if repo.is_empty() {
                        return Err("repo is required for saga mode".into());
                    }
                    prefer_space = Some("human".to_string());
                }
                "code" => {
                    prefer_space = Some("code".to_string());
                    limit_override = 15;
                }
                "notes" => {
                    prefer_space = Some("human".to_string());
                }
                _ => {} // unknown mode: fall through to default
            }
        }

        let limit = if limit_override > 0 {
            limit_override as usize
        } else {
            3
        };

        let prefer_space_domain = prefer_space.as_deref().and_then(|s| match s {
            "code" => Some(graphrag::types::PreferSpace::Code),
            "doc" => Some(graphrag::types::PreferSpace::Doc),
            "human" => Some(graphrag::types::PreferSpace::Human),
            _ => None,
        });
        let req = akashic_domain::types::GraphRagQuery {
            query: args.query.clone(),
            repo_name: args.repo.clone(),
            k_per_space: Some(limit),
            token_budget: Some(64_000),
            prefer_space: prefer_space_domain,
        };
        // Delegate to SearchService::graphrag_query (collapses REST graphrag_query dup).
        // Access-count UPDATE for Human-space notes is now inside the service method.
        let response = self
            .search_service
            .graphrag_query(req)
            .await
            .map_err(|e| format!("GraphRAG query failed: {e}"))?;

        // Format as compact index for MCP (formatting stays in the tool layer).
        let mut output = String::new();
        for (i, node) in response.nodes_used.iter().enumerate() {
            let space_label = match node.space {
                akashic_domain::types::Space::Code => "CODE",
                akashic_domain::types::Space::Doc => "DOC",
                akashic_domain::types::Space::Human => "NOTE",
            };
            output.push_str(&format!(
                "[{}] ({}) {} | {} | hop={} | score={:.3}\n",
                i + 1,
                space_label,
                node.name,
                node.entity_type,
                node.hop_distance,
                node.final_score,
            ));
        }
        output.push_str(&format!(
            "\nTotal context tokens: {}\n",
            response.total_tokens
        ));

        info!(count = response.nodes_used.len(), query = %args.query, "search_knowledge (GraphRAG) completed");

        Ok(output)
    }

    // ══════════════════════════════════════════════════════════════════
    // get_details — The Sniper (full content for specific IDs)
    // ══════════════════════════════════════════════════════════════════

    /// Fetch full content for specific IDs from search_knowledge results.
    #[tool(
        name = "get_details",
        description = "Fetch full content for specific IDs from search_knowledge results. With repo only (no ids): lists modules."
    )]
    async fn get_details(
        &self,
        Parameters(args): Parameters<GetDetailsArgs>,
    ) -> Result<String, String> {
        // Mode 1: module-list mode (no IDs).
        if args.ids.is_none() || args.ids.as_ref().is_some_and(std::vec::Vec::is_empty) {
            let repo = args.repo.ok_or("Either 'ids' or 'repo' must be provided")?;
            // Delegate to SearchService (collapses REST /details dup).
            let modules = self
                .search_service
                .get_details(Vec::new(), Some(repo.clone()))
                .await
                .map_err(|e| format!("get_details failed: {e}"))?;

            info!(count = modules.len(), repo = %repo, "Modules listed");
            return serde_json::to_string_pretty(&modules)
                .map_err(|e| format!("Serialization failed: {e}"));
        }

        // Mode 2: fetch full details for specific IDs. Mode 1 returns early when
        // `ids` is None/empty; resolve defensively (no unwrap) so a future guard
        // refactor can't turn this into a panic.
        let Some(ids) = args.ids else {
            return Err("Either 'ids' or 'repo' must be provided".to_string());
        };
        let id_vec: Vec<uuid::Uuid> = ids
            .iter()
            .map(|s| {
                s.parse::<uuid::Uuid>()
                    .map_err(|_| format!("Invalid UUID: {s}"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let details = self
            .search_service
            .get_details(id_vec, None)
            .await
            .map_err(|e| format!("get_details failed: {e}"))?;

        info!(count = details.len(), "get_details completed");
        serde_json::to_string_pretty(&details).map_err(|e| format!("Serialization failed: {e}"))
    }

    // ══════════════════════════════════════════════════════════════════
    // global_query — community-based global search
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "global_query",
        description = "Answer global questions about repository architecture, themes, and patterns. \
Uses community detection (Leiden algorithm) to provide high-level understanding. \
Use for questions like 'what are the main subsystems?' or 'how is the codebase organized?'"
    )]
    async fn global_query(
        &self,
        Parameters(args): Parameters<GlobalQueryArgs>,
    ) -> Result<String, String> {
        // Delegate to SearchService (collapses inline SQL dup).
        let items = self
            .search_service
            .global_query(
                args.query.clone(),
                args.repo.clone(),
                args.level,
                args.include_examples,
                args.max_communities as usize,
                None, // prefer_space not used by global_query
            )
            .await
            .map_err(|e| format!("global_query failed: {e}"))?;

        if items.is_empty() {
            return Ok(format!(
                "No community summaries found for '{}'. Run ingestion first.\n",
                args.repo
            ));
        }

        let target_level = items.first().map_or(0, |i| i.level);
        let mut out = format!(
            "=== GLOBAL QUERY: '{}' (level {}) ===\n\n",
            args.repo, target_level
        );

        for item in &items {
            out.push_str(&format!(
                "<community level=\"{}\" name=\"{}\" members=\"{}\" score=\"{:.3}\">\n{}\n</community>\n\n",
                item.level, item.name, item.member_count, item.score, item.summary
            ));

            if args.include_examples && !item.examples.is_empty() {
                out.push_str("  Examples:\n");
                for (ename, etype, epath) in &item.examples {
                    out.push_str(&format!("    - {ename} ({etype}) — {epath}\n"));
                }
                out.push('\n');
            }
        }

        out.push_str(&format!(
            "Showing {} communities at level {}.\n",
            items.len(),
            target_level
        ));

        info!(repo = %args.repo, level = target_level, results = items.len(), "global_query completed");

        Ok(out)
    }
}
