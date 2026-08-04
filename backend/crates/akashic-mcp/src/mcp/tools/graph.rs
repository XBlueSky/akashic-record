//! graph MCP tools (Slice E tools.rs split). One of the composed
//! `#[tool_router]` blocks; combined in `super`'s `AkashicMcp::new`.
use super::*;

#[tool_router(router = graph_tools, vis = "pub(crate)")]
impl AkashicMcp {
    // ══════════════════════════════════════════════════════════════════
    // get_project_summary — thinned (A2a-T4): delegates to GraphService
    // ══════════════════════════════════════════════════════════════════

    /// Get a summary overview of a repository.
    #[tool(
        name = "get_project_summary",
        description = "Get an aggregated overview of a repository, including its branches, note counts by category, and the most recent knowledge entries."
    )]
    async fn get_project_summary(
        &self,
        Parameters(args): Parameters<GetProjectSummaryArgs>,
    ) -> Result<String, String> {
        self.graph_service
            .get_project_summary(args.repo_name, args.branch_name)
            .await
            .map_err(|e| format!("GraphService::get_project_summary failed: {e}"))
    }

    // ══════════════════════════════════════════════════════════════════
    // analyze_impact — blast radius analysis
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "analyze_impact",
        description = "Evaluate blast radius of changing a symbol or file. \
Uses weighted ripple scoring across CALLS, IMPORTS_FROM, EXPLAINS, ATTACHED_TO, and execution flows. \
Returns impact-ranked results with affected flows and suggested test targets."
    )]
    async fn analyze_impact(
        &self,
        Parameters(args): Parameters<AnalyzeImpactArgs>,
    ) -> Result<String, String> {
        use akashic_domain::algos::impact as impact_algo;

        let report = self
            .navigation_service
            .analyze_impact(
                args.target.clone(),
                args.repo.clone(),
                args.max_depth.clamp(1, 4) as u8,
                args.min_score,
            )
            .await
            .map_err(|e| format!("analyze_impact failed: {e}"))?;

        let risk = impact_algo::risk_level(&report.affected);
        let output = impact_algo::format_impact(
            &args.target,
            &report.affected,
            risk,
            &report.flows,
            &report.suggested_tests,
        );

        info!(target = %args.target, risk = %risk, nodes = report.affected.len(), "analyze_impact completed");

        Ok(output)
    }

    #[tool(
        name = "analyze_change_impact",
        description = "Evaluate the COMBINED blast radius of a SET of changed symbols (e.g. the \
functions/types touched by a diff or PR). Runs the analyze_impact ripple scoring per symbol, \
merges and de-duplicates the affected nodes, unions affected flows and suggested tests, excludes \
the changed symbols themselves, and reports which changed symbols had dependents vs none."
    )]
    async fn analyze_change_impact(
        &self,
        Parameters(args): Parameters<AnalyzeChangeImpactArgs>,
    ) -> Result<String, String> {
        use akashic_domain::algos::impact as impact_algo;

        let report = self
            .navigation_service
            .analyze_change_impact(
                args.changed.clone(),
                args.repo.clone(),
                args.max_depth.clamp(1, 4) as u8,
                args.min_score,
            )
            .await
            .map_err(|e| format!("analyze_change_impact failed: {e}"))?;

        info!(
            changed = args.changed.len(),
            with_impact = report.with_impact.len(),
            affected = report.affected.len(),
            risk = %report.risk,
            "analyze_change_impact completed"
        );

        Ok(impact_algo::format_change_impact(&report))
    }

    #[tool(
        name = "detect_code_communities",
        description = "Partition a repository's call graph into communities (functional \
clusters) via Leiden community detection. Returns communities ranked by size with member \
symbols and a dominant-module label. Symbols with no CALLS edge are not shown."
    )]
    async fn detect_code_communities(
        &self,
        Parameters(args): Parameters<DetectCodeCommunitiesArgs>,
    ) -> Result<String, String> {
        use akashic_domain::algos::code_community;
        let communities = self
            .graph_service
            .detect_code_communities(args.repo.clone(), args.min_confidence)
            .await
            .map_err(|e| format!("detect_code_communities failed: {e}"))?;
        info!(repo = %args.repo, communities = communities.len(), "detect_code_communities completed");
        Ok(code_community::format_communities(
            &args.repo,
            &communities,
            args.max_communities as usize,
            args.max_members as usize,
        ))
    }

    #[tool(
        name = "detect_dead_code",
        description = "List ranked dead-code candidates for a repository: `function` chunks with \
zero inbound CALLS edges that are not entry points. Each candidate carries a confidence tier \
(high/medium/low, by visibility) and a reason. Call-graph recall is imperfect — candidates \
require human confirmation before removal."
    )]
    async fn detect_dead_code(
        &self,
        Parameters(args): Parameters<DetectDeadCodeArgs>,
    ) -> Result<String, String> {
        use akashic_domain::algos::dead_code;
        let report = self
            .graph_service
            .detect_dead_code(args.repo.clone())
            .await
            .map_err(|e| format!("detect_dead_code failed: {e}"))?;
        info!(
            repo = %args.repo,
            candidates = report.candidates.len(),
            "detect_dead_code completed"
        );
        Ok(dead_code::format_dead_code(
            &args.repo,
            &report,
            args.max_candidates as usize,
        ))
    }

    #[tool(
        name = "link_cross_service_calls",
        description = "Rebuild cross-service HTTP_CALLS edges across the WHOLE graph (all repos): \
match every client http_call (method, path) against every server route and persist a \
(caller:function)-[:HTTP_CALLS {http_method, http_path, matched_via, cross_repo}]->(handler:function) \
edge per match. Idempotent full rebuild (delete-then-MERGE). WRITE tool — requires authentication. \
Returns a report of links created (cross-repo vs intra-repo) and dangling client calls with no route."
    )]
    async fn link_cross_service_calls(
        &self,
        Parameters(args): Parameters<LinkCrossServiceCallsArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        use akashic_domain::algos::http_link;
        let actor = require_actor(&ctx)?;
        let report = self
            .graph_service
            .link_cross_service_calls()
            .await
            .map_err(|e| format!("link_cross_service_calls failed: {e}"))?;
        spawn_write_audit(&self.pg, actor, "link_cross_service_calls", &args, true);
        info!(
            links = report.links_created,
            cross_repo = report.cross_repo_links,
            "link_cross_service_calls completed"
        );
        Ok(http_link::format_cross_service_links(
            &report,
            args.include_intra_repo,
            50,
            25,
        ))
    }

    #[tool(
        name = "trace_decision_history",
        description = "Given a code symbol or fqn, list the architecture DECISION notes attached to \
matching code and walk each one's SUPERSEDES chain, returning a merged decision timeline \
(nearest-first by supersede distance). `symbol` is matched as a forgiving substring (CONTAINS) \
against chunk name/fqn. Read-only."
    )]
    async fn trace_decision_history(
        &self,
        Parameters(args): Parameters<TraceDecisionHistoryArgs>,
    ) -> Result<String, String> {
        use akashic_domain::algos::decision_lineage;
        let timeline = self
            .graph_service
            .trace_decision_history(args.repo.clone(), args.symbol.clone())
            .await
            .map_err(|e| format!("trace_decision_history failed: {e}"))?;
        info!(
            repo = %args.repo,
            symbol = %args.symbol,
            entries = timeline.len(),
            "trace_decision_history completed"
        );
        Ok(decision_lineage::format_decision_history(
            &args.repo,
            &args.symbol,
            &timeline,
        ))
    }

    #[tool(
        name = "get_decision_lineage",
        description = "Given one DECISION note's UUID, return its full SUPERSEDES lineage (the \
decisions it replaced and the decisions that replaced it) plus the chunks that note is directly \
attached to. Read-only."
    )]
    async fn get_decision_lineage(
        &self,
        Parameters(args): Parameters<GetDecisionLineageArgs>,
    ) -> Result<String, String> {
        use akashic_domain::algos::decision_lineage;
        let note_uuid =
            uuid::Uuid::parse_str(&args.note_id).map_err(|e| format!("Invalid note_id: {e}"))?;
        let report = self
            .graph_service
            .get_decision_lineage(note_uuid)
            .await
            .map_err(|e| format!("get_decision_lineage failed: {e}"))?;
        info!(
            note_id = %args.note_id,
            entries = report.timeline.len(),
            "get_decision_lineage completed"
        );
        Ok(decision_lineage::format_decision_lineage(&report))
    }
}
