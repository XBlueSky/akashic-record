use super::*;

// ── Shared per-target impact collector (extracted from analyze_impact) ─────────

impl RetrievalServices {
    /// Run the five reverse-traversal queries for ONE impact target and collect
    /// the raw blast-radius edges plus the affected flows and suggested tests.
    ///
    /// Behavior-preserving extraction from `analyze_impact`: seed-agnostic
    /// `aggregate_impacts`, `min_score` filtering, truncation and formatting stay
    /// with the CALLERS (`analyze_impact` / `analyze_change_impact`). The
    /// `max_depth` clamp lives here so every caller clamps identically.
    async fn collect_impact_for_target(
        &self,
        target: &str,
        repo: Option<&str>,
        max_depth: u8,
    ) -> DomainResult<(Vec<RawImpactEdge>, Vec<AffectedFlow>, Vec<SuggestedTest>)> {
        // FIX(finding-4): bound the blast radius — verbatim limit from MCP handler.
        const PER_TRAVERSAL_LIMIT: usize = 200;

        let max_depth = max_depth.clamp(1, 4);

        // The five reverse-traversal queries are independent reads over the same
        // (target, repo); run them concurrently instead of five sequential
        // round-trips. Order preserved so edge assembly below is unchanged.
        let (call_rows, import_rows, explains_rows, note_rows, flow_rows) = tokio::try_join!(
            async {
                self.traversal_repo
                    .impact_callers(target, repo, max_depth, PER_TRAVERSAL_LIMIT)
                    .await
                    .map_err(|e| anyhow::anyhow!("Callers query failed: {e}"))
            },
            async {
                self.traversal_repo
                    .impact_importers(target, repo, PER_TRAVERSAL_LIMIT)
                    .await
                    .map_err(|e| anyhow::anyhow!("Imports query failed: {e}"))
            },
            async {
                self.traversal_repo
                    .impact_explains(target, repo, PER_TRAVERSAL_LIMIT)
                    .await
                    .map_err(|e| anyhow::anyhow!("Explains query failed: {e}"))
            },
            async {
                self.traversal_repo
                    .impact_notes(target, repo, PER_TRAVERSAL_LIMIT)
                    .await
                    .map_err(|e| anyhow::anyhow!("Notes query failed: {e}"))
            },
            async {
                self.traversal_repo
                    .impact_flows(target, repo, PER_TRAVERSAL_LIMIT)
                    .await
                    .map_err(|e| anyhow::anyhow!("Flow query failed: {e}"))
            },
        )?;

        let mut all_edges: Vec<RawImpactEdge> = Vec::new();

        // 1. CALLS upstream (CalledBy)
        for row in call_rows {
            all_edges.push(RawImpactEdge {
                node_id: row.pg_id,
                node_name: row.name,
                module_path: row.module,
                entity_type: row.ctype,
                path_type: ImpactPathType::CalledBy,
                hops: row.depth as u32,
                confidence: row.conf,
            });
        }

        // 2. IMPORTS_FROM (ImportedBy)
        for row in import_rows {
            all_edges.push(RawImpactEdge {
                node_id: row.pg_id,
                node_name: row.name,
                module_path: row.module,
                entity_type: row.ctype,
                path_type: ImpactPathType::ImportedBy,
                hops: 1,
                confidence: 1.0,
            });
        }

        // 3. EXPLAINS (ExplainedBy)
        for row in explains_rows {
            all_edges.push(RawImpactEdge {
                node_id: row.pg_id,
                node_name: row.name,
                module_path: row.module,
                entity_type: row.ctype,
                path_type: ImpactPathType::ExplainedBy,
                hops: 1,
                confidence: row.conf,
            });
        }

        // 4. ATTACHED_TO (NotedBy)
        for row in note_rows {
            all_edges.push(RawImpactEdge {
                node_id: row.pg_id,
                node_name: row.name,
                module_path: row.module,
                entity_type: row.ctype,
                path_type: ImpactPathType::NotedBy,
                hops: 1,
                confidence: 1.0,
            });
        }

        // 5. Affected flows (InFlow) — collected separately, not pushed as edges.

        let mut affected_flows: Vec<AffectedFlow> = Vec::new();
        let mut suggested_tests: Vec<SuggestedTest> = Vec::new();

        for row in &flow_rows {
            affected_flows.push(AffectedFlow {
                flow_name: row.flow_name.clone(),
                entry_point: row.flow_name.clone(),
                entry_type: row.flow_etype.clone(),
                step_position: row.my_pos as u32,
                total_steps: row.flow_steps as u32,
            });

            // Per-flow IS_ENTRY_POINT → suggested test
            let ep_rows = self
                .traversal_repo
                .get_flow_entry_points_by_id(&row.flow_id)
                .await
                .unwrap_or_default();
            for ep in ep_rows {
                suggested_tests.push(SuggestedTest {
                    entry_point: ep.ep_name,
                    entry_type: row.flow_etype.clone(),
                    flow_name: row.flow_name.clone(),
                    reason: format!("contains affected symbol at step {}", row.my_pos),
                });
            }
        }

        Ok((all_edges, affected_flows, suggested_tests))
    }
}

// ── NavigationService impl ────────────────────────────────────────────────────

#[async_trait]
impl NavigationService for RetrievalServices {
    async fn traverse_code_calls(
        &self,
        symbol: String,
        repo: Option<String>,
        direction: String,
        max_depth: u8,
        min_confidence: f32,
    ) -> DomainResult<String> {
        let max_depth = max_depth.clamp(1, 4);
        let min_conf = (min_confidence as f64).clamp(0.0, 1.0);
        let direction = direction.to_lowercase();
        let repo_ref = repo.as_deref();

        let mut output = String::new();

        // Downstream: start -[:CALLS*1..N]-> callee
        if direction == "downstream" || direction == "both" {
            let rows = self
                .traversal_repo
                .traverse_calls_downstream(&symbol, repo_ref, max_depth, min_conf)
                .await
                .map_err(|e| anyhow::anyhow!("Neo4j downstream query failed: {e}"))?;

            output.push_str(&format!("=== DOWNSTREAM (callees of '{symbol}') ===\n"));
            if rows.is_empty() {
                output.push_str("  (none found)\n");
            } else {
                for row in &rows {
                    let indent = "  ".repeat(row.depth as usize);
                    output.push_str(&format!(
                        "{indent}[d{}] {} ({}) — {} [conf={:.2}, method={}]\n",
                        row.depth, row.name, row.ctype, row.module, row.conf, row.method
                    ));
                }
            }
            output.push('\n');
        }

        // Upstream: caller -[:CALLS*1..N]-> start
        if direction == "upstream" || direction == "both" {
            let rows = self
                .traversal_repo
                .traverse_calls_upstream(&symbol, repo_ref, max_depth, min_conf)
                .await
                .map_err(|e| anyhow::anyhow!("Neo4j upstream query failed: {e}"))?;

            output.push_str(&format!("=== UPSTREAM (callers of '{symbol}') ===\n"));
            if rows.is_empty() {
                output.push_str("  (none found)\n");
            } else {
                for row in &rows {
                    let indent = "  ".repeat(row.depth as usize);
                    output.push_str(&format!(
                        "{indent}[d{}] {} ({}) — {} [conf={:.2}, method={}]\n",
                        row.depth, row.name, row.ctype, row.module, row.conf, row.method
                    ));
                }
            }
            output.push('\n');
        }

        Ok(output)
    }

    async fn goto_definition(
        &self,
        name: String,
        repo: Option<String>,
        module_hint: Option<String>,
        kind_hint: Option<String>,
    ) -> DomainResult<Vec<SymbolCandidate>> {
        // Delegate to the existing free function in symbol_resolution.
        // sym::resolve_symbol returns Vec<sym::SymbolCandidate>; convert to domain type.
        let cands = sym::resolve_symbol(
            &self.symbol_repo,
            &name,
            repo.as_deref(),
            module_hint.as_deref(),
            kind_hint.as_deref(),
        )
        .await?;

        Ok(cands
            .into_iter()
            .map(|c| SymbolCandidate {
                fqn: c.fqn,
                name: c.name,
                chunk_type: c.chunk_type,
                module_path: c.module_path,
                visibility: c.visibility,
                start_line: c.start_line,
                end_line: c.end_line,
                signature: c.signature,
            })
            .collect())
    }

    async fn find_references(
        &self,
        fqn: String,
        repo: Option<String>,
        min_confidence: f64,
        include_kinds: Vec<String>,
    ) -> DomainResult<Vec<ReferenceRow>> {
        // Delegate to the existing free function in symbol_resolution.
        let rows = sym::find_references(
            &self.symbol_repo,
            &self.traversal_repo,
            &fqn,
            repo.as_deref(),
            min_confidence,
            &include_kinds,
        )
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| ReferenceRow {
                caller_fqn: r.caller_fqn,
                caller_name: r.caller_name,
                module_path: r.module_path,
                chunk_type: r.chunk_type,
                start_line: r.start_line,
                end_line: r.end_line,
                ref_kind: r.ref_kind,
                confidence: r.confidence,
                method: r.method,
            })
            .collect())
    }

    async fn find_implementations(
        &self,
        fqn: String,
        repo: Option<String>,
        implementors: bool,
    ) -> DomainResult<Vec<ReferenceRow>> {
        // Delegate to the existing free function in symbol_resolution.
        let rows = sym::find_implementations(
            &self.symbol_repo,
            &self.traversal_repo,
            &fqn,
            repo.as_deref(),
            implementors,
        )
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| ReferenceRow {
                caller_fqn: r.caller_fqn,
                caller_name: r.caller_name,
                module_path: r.module_path,
                chunk_type: r.chunk_type,
                start_line: r.start_line,
                end_line: r.end_line,
                ref_kind: r.ref_kind,
                confidence: r.confidence,
                method: r.method,
            })
            .collect())
    }

    async fn trace_execution_flow(
        &self,
        symbol: Option<String>,
        repo: Option<String>,
        entry_type: Option<String>,
        max_depth: u8,
        list_all: bool,
    ) -> DomainResult<String> {
        let mut output = String::new();

        if list_all {
            let repo_str = repo.as_deref().ok_or_else(|| {
                DomainError::BadRequest("repo required when list_all=true".into())
            })?;

            let rows = self.traversal_repo.list_flows_by_repo(repo_str).await?;

            output.push_str(&format!("=== ALL FLOWS in '{repo_str}' ===\n\n"));
            if rows.is_empty() {
                output.push_str("  (no flows found — run ingestion first)\n");
            } else {
                for row in &rows {
                    let trunc_mark = if row.trunc { " (truncated)" } else { "" };
                    output.push_str(&format!(
                        "  [{}] {} — {} steps{trunc_mark}\n",
                        row.etype, row.name, row.steps
                    ));
                }
                output.push_str(&format!("\nTotal: {} flows\n", rows.len()));
            }

            return Ok(output);
        }

        let symbol_str = symbol.as_deref().ok_or_else(|| {
            DomainError::BadRequest("symbol required (or use list_all=true)".into())
        })?;

        let flow_rows = self
            .traversal_repo
            .find_flows_by_symbol(symbol_str, repo.as_deref(), entry_type.as_deref())
            .await?;

        if flow_rows.is_empty() {
            return Ok(format!(
                "No execution flows found containing '{symbol_str}'.\n"
            ));
        }

        output.push_str(&format!(
            "=== EXECUTION FLOWS containing '{symbol_str}' ===\n\n"
        ));

        let max_depth_clamped = max_depth.clamp(1, 12);

        for flow_row in &flow_rows {
            output.push_str(&format!(
                "── Flow: \"{}\" ({}) ──\n",
                flow_row.flow_name, flow_row.flow_etype
            ));

            let step_rows = self
                .traversal_repo
                .get_flow_steps(&flow_row.flow_id, max_depth_clamped)
                .await?;

            for step_row in &step_rows {
                let indent = "  ".repeat(step_row.depth as usize);
                let marker = if step_row.pos == 0 {
                    " <- entry"
                } else if step_row.pos == flow_row.my_position {
                    " << YOU ARE HERE"
                } else {
                    ""
                };
                output.push_str(&format!(
                    "  [{}] {indent}{}  depth={}{marker}\n",
                    step_row.pos, step_row.name, step_row.depth
                ));
            }

            output.push_str(&format!("  ({} total steps)\n\n", flow_row.flow_steps));
        }

        output.push_str(&format!(
            "This symbol appears in {} flow(s).\n",
            flow_rows.len()
        ));

        Ok(output)
    }

    async fn analyze_impact(
        &self,
        target: String,
        repo: Option<String>,
        max_depth: u8,
        min_score: f64,
    ) -> DomainResult<ImpactReport> {
        // FIX(finding-4): bound the blast radius — verbatim limit from MCP handler.
        const MAX_IMPACT_NODES: usize = 100;

        let (all_edges, affected_flows, suggested_tests) = self
            .collect_impact_for_target(&target, repo.as_deref(), max_depth)
            .await?;

        // Aggregate, filter, truncate — verbatim from MCP handler.
        let mut nodes = impact_algo::aggregate_impacts(all_edges);
        if min_score > 0.0 {
            nodes.retain(|n| n.impact_score >= min_score);
        }
        nodes.truncate(MAX_IMPACT_NODES);

        Ok(ImpactReport {
            affected: nodes,
            flows: affected_flows,
            suggested_tests,
        })
    }

    async fn analyze_change_impact(
        &self,
        changed: Vec<String>,
        repo: Option<String>,
        max_depth: u8,
        min_score: f64,
    ) -> DomainResult<ChangeImpactReport> {
        // FIX(finding-4): bound the blast radius — same cap as analyze_impact.
        const MAX_IMPACT_NODES: usize = 100;

        // De-duplicate the changed set, preserving first-seen order.
        let mut seen = std::collections::HashSet::new();
        let changed: Vec<String> = changed
            .into_iter()
            .filter(|s| seen.insert(s.clone()))
            .collect();

        let repo_ref = repo.as_deref();

        // Collect impact per changed symbol (index-aligned with `changed`).
        let mut per_seed_edges: Vec<Vec<RawImpactEdge>> = Vec::with_capacity(changed.len());
        let mut all_flows: Vec<AffectedFlow> = Vec::new();
        let mut all_tests: Vec<SuggestedTest> = Vec::new();
        for symbol in &changed {
            let (edges, flows, tests) = self
                .collect_impact_for_target(symbol, repo_ref, max_depth)
                .await?;
            per_seed_edges.push(edges);
            all_flows.extend(flows);
            all_tests.extend(tests);
        }

        Ok(impact_algo::aggregate_change_impacts(
            changed,
            per_seed_edges,
            all_flows,
            all_tests,
            min_score,
            MAX_IMPACT_NODES,
        ))
    }
}
