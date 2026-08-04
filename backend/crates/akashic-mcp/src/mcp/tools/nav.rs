//! nav MCP tools (Slice E tools.rs split). One of the composed
//! `#[tool_router]` blocks; combined in `super`'s `AkashicMcp::new`.
use super::*;

#[tool_router(router = nav_tools, vis = "pub(crate)")]
impl AkashicMcp {
    // ══════════════════════════════════════════════════════════════════
    // traverse_code_calls — call graph traversal via CALLS edges
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "traverse_code_calls",
        description = "Traverse CALLS edges to build a precise call graph from a symbol. \
Shows who calls it (upstream) and what it calls (downstream). \
Use search_knowledge first to find the exact symbol name."
    )]
    async fn traverse_code_calls(
        &self,
        Parameters(args): Parameters<TraverseCallsArgs>,
    ) -> Result<String, String> {
        let output = self
            .navigation_service
            .traverse_code_calls(
                args.symbol.clone(),
                args.repo.clone(),
                args.direction.clone(),
                args.max_depth.clamp(1, 4) as u8,
                args.min_confidence.clamp(0.0, 1.0),
            )
            .await
            .map_err(|e| format!("traverse_code_calls failed: {e}"))?;
        info!(symbol = %args.symbol, direction = %args.direction, "traverse_code_calls completed");
        Ok(output)
    }

    // ══════════════════════════════════════════════════════════════════
    // goto_definition — resolve a symbol name to its precise definition(s)
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "goto_definition",
        description = "Resolve a symbol NAME to its precise definition(s) by exact fqn/name \
match (not substring). Returns the one definition, or ranked candidates if the name is \
ambiguous. Use the returned fqn with find_references."
    )]
    async fn goto_definition(
        &self,
        Parameters(args): Parameters<crate::mcp::types::GotoDefinitionArgs>,
    ) -> Result<String, String> {
        let cands = self
            .navigation_service
            .goto_definition(
                args.name.clone(),
                args.repo.clone(),
                args.module_hint.clone(),
                args.kind_hint.clone(),
            )
            .await
            .map_err(|e| format!("goto_definition failed: {e}"))?;

        if cands.is_empty() {
            return Ok(format!(
                "No definition found for '{}'. Try search_knowledge for a semantic search.",
                args.name
            ));
        }
        let mut out = String::new();
        if cands.len() == 1 {
            out.push_str(&format!("=== DEFINITION of '{}' ===\n", args.name));
        } else {
            out.push_str(&format!(
                "=== {} CANDIDATES for '{}' (ambiguous — pass an fqn to find_references) ===\n",
                cands.len(),
                args.name
            ));
        }
        for c in &cands {
            let fqn = c.fqn.clone().unwrap_or_else(|| c.name.clone());
            let vis = c.visibility.clone().unwrap_or_default();
            let lines = match (c.start_line, c.end_line) {
                (Some(s), Some(e)) => format!("L{s}-{e}"),
                _ => "L?".into(),
            };
            out.push_str(&format!(
                "  {fqn} ({}) — {} [{lines}{}]\n",
                c.chunk_type,
                c.module_path,
                if vis.is_empty() {
                    String::new()
                } else {
                    format!(", {vis}")
                }
            ));
            if let Some(sig) = &c.signature
                && !sig.is_empty()
            {
                out.push_str(&format!("      {sig}\n"));
            }
        }
        info!(name = %args.name, candidates = cands.len(), "goto_definition completed");
        Ok(out)
    }

    // ══════════════════════════════════════════════════════════════════
    // find_references — direct references (callers) of a precise fqn
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "find_references",
        description = "Find DIRECT references (call sites) to a precise fqn. Use \
goto_definition first to get the fqn. High-confidence by default (excludes name-based \
heuristic guesses); lower min_confidence to include them."
    )]
    async fn find_references(
        &self,
        Parameters(args): Parameters<crate::mcp::types::FindReferencesArgs>,
    ) -> Result<String, String> {
        let refs = self
            .navigation_service
            .find_references(
                args.fqn.clone(),
                args.repo.clone(),
                args.min_confidence.clamp(0.0, 1.0) as f64,
                args.include_kinds.clone(),
            )
            .await
            .map_err(|e| format!("find_references failed: {e}"))?;

        let mut out = format!(
            "=== REFERENCES to '{}' (min_confidence={:.2}) ===\n",
            args.fqn, args.min_confidence
        );
        if refs.is_empty() {
            out.push_str("  (none found)\n");
            return Ok(out);
        }
        // Group by ref_kind.
        let mut by_kind: std::collections::BTreeMap<
            String,
            Vec<&akashic_domain::types::ReferenceRow>,
        > = std::collections::BTreeMap::new();
        for r in &refs {
            by_kind.entry(r.ref_kind.clone()).or_default().push(r);
        }
        for (kind, rows) in &by_kind {
            out.push_str(&format!("── {} ({}) ──\n", kind, rows.len()));
            for r in rows {
                let lines = match (r.start_line, r.end_line) {
                    (Some(s), Some(e)) => format!("L{s}-{e}"),
                    _ => "L?".into(),
                };
                out.push_str(&format!(
                    "  [{:.2}] {} ({}) — {} [{lines}, method={}]\n",
                    r.confidence, r.caller_fqn, r.chunk_type, r.module_path, r.method
                ));
            }
        }
        info!(fqn = %args.fqn, refs = refs.len(), "find_references completed");
        Ok(out)
    }

    // ══════════════════════════════════════════════════════════════════
    // find_implementations — who implements/extends a trait or class
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "find_implementations",
        description = "Find implementations of a trait/interface/class by fqn. \
implementors=true (default) lists types that implement/extend it; implementors=false \
lists what it implements/extends. Use goto_definition first to get the fqn."
    )]
    async fn find_implementations(
        &self,
        Parameters(args): Parameters<crate::mcp::types::FindImplementationsArgs>,
    ) -> Result<String, String> {
        let rows = self
            .navigation_service
            .find_implementations(args.fqn.clone(), args.repo.clone(), args.implementors)
            .await
            .map_err(|e| format!("find_implementations failed: {e}"))?;
        let header = if args.implementors {
            format!("=== IMPLEMENTATIONS of '{}' ===\n", args.fqn)
        } else {
            format!("=== SUPERTYPES of '{}' ===\n", args.fqn)
        };
        let mut out = header;
        if rows.is_empty() {
            out.push_str("  (none found)\n");
            return Ok(out);
        }
        for r in &rows {
            let lines = match (r.start_line, r.end_line) {
                (Some(s), Some(e)) => format!("L{s}-{e}"),
                _ => "L?".into(),
            };
            out.push_str(&format!(
                "  [{}] {} ({}) — {} [{lines}]\n",
                r.ref_kind, r.caller_fqn, r.chunk_type, r.module_path
            ));
        }
        info!(fqn = %args.fqn, implementors = args.implementors, results = rows.len(), "find_implementations completed");
        Ok(out)
    }

    // ══════════════════════════════════════════════════════════════════
    // trace_execution_flow — execution path tracing via Flow nodes
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "trace_execution_flow",
        description = "Trace execution flows from entry points (API handlers, main, tests). \
Shows the ordered call chain for a business process. \
Use to understand how a symbol fits into larger execution paths."
    )]
    async fn trace_execution_flow(
        &self,
        Parameters(args): Parameters<TraceFlowArgs>,
    ) -> Result<String, String> {
        let output = self
            .navigation_service
            .trace_execution_flow(
                args.symbol.clone(),
                args.repo.clone(),
                args.entry_type.clone(),
                args.max_depth.clamp(1, 12) as u8,
                args.list_all,
            )
            .await
            .map_err(|e| format!("trace_execution_flow failed: {e}"))?;
        info!(
            symbol = ?args.symbol,
            list_all = args.list_all,
            "trace_execution_flow completed"
        );
        Ok(output)
    }
}
