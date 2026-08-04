//! GraphService impl bodies — analytics (community detection, dead-code
//! ranking). Inherent `gs_*` method the thin `impl GraphService` in `super`
//! delegates to.
use super::*;
use akashic_domain::algos::code_community;
use akashic_domain::algos::dead_code;
use akashic_domain::algos::decision_lineage;
use akashic_domain::algos::http_link;

impl RetrievalServices {
    pub(crate) async fn gs_detect_code_communities(
        &self,
        repo: String,
        min_confidence: f64,
    ) -> DomainResult<Vec<code_community::Community>> {
        let rows = self
            .graph_read_repo
            .fetch_call_edges(&repo, min_confidence)
            .await?;
        let edges: Vec<code_community::CommunityEdge> = rows
            .into_iter()
            .map(|r| code_community::CommunityEdge {
                source_id: r.source,
                source_name: r.source_name,
                source_module: r.source_module,
                target_id: r.target,
                target_name: r.target_name,
                target_module: r.target_module,
                weight: r.weight,
            })
            .collect();
        Ok(code_community::detect_communities(&edges))
    }

    pub(crate) async fn gs_detect_dead_code(
        &self,
        repo: String,
    ) -> DomainResult<dead_code::DeadCodeReport> {
        let rows = self
            .graph_read_repo
            .fetch_zero_caller_functions(&repo)
            .await?;
        let total_functions = self.graph_read_repo.count_functions(&repo).await?;
        let fns: Vec<dead_code::DeadCodeFn> = rows
            .into_iter()
            .map(|r| dead_code::DeadCodeFn {
                name: r.name,
                module_path: r.module_path,
                fqn: r.fqn,
                visibility: r.visibility,
                is_entry_point: r.is_entry_point,
            })
            .collect();
        Ok(dead_code::build_report(
            usize::try_from(total_functions).unwrap_or(0),
            &fns,
        ))
    }

    pub(crate) async fn gs_link_cross_service_calls(
        &self,
    ) -> DomainResult<http_link::CrossServiceLinkReport> {
        // 1. Fetch call sites + route sites (global, all repos).
        let call_rows = self.graph_read_repo.fetch_http_call_sites().await?;
        let route_rows = self.graph_read_repo.fetch_route_sites().await?;

        // Move the row fields into the algo structs — the source rows are not
        // used after this, so there is no need to clone every String.
        let calls: Vec<http_link::HttpCallSite> = call_rows
            .into_iter()
            .map(|r| http_link::HttpCallSite {
                caller_pg_id: r.caller_pg_id,
                caller_repo: r.caller_repo,
                caller_fqn: r.caller_fqn,
                http_method: r.http_method,
                http_path: r.http_path,
            })
            .collect();
        let routes: Vec<http_link::RouteSite> = route_rows
            .into_iter()
            .map(|r| http_link::RouteSite {
                handler_pg_id: r.handler_pg_id,
                handler_repo: r.handler_repo,
                handler_fqn: r.handler_fqn,
                http_method: r.http_method,
                http_path: r.http_path,
            })
            .collect();

        // 2. Pure match.
        let links = http_link::link_calls(&calls, &routes);

        // 3. Persist (global delete-then-MERGE).
        self.graph_write_repo
            .create_http_calls_edges(&links)
            .await?;

        // 4. Build the report. Resolve pg_id → human label from the rows.
        let caller_label: std::collections::HashMap<&str, (&str, &str)> = calls
            .iter()
            .map(|c| {
                (
                    c.caller_pg_id.as_str(),
                    (c.caller_repo.as_str(), c.caller_fqn.as_str()),
                )
            })
            .collect();
        let handler_label: std::collections::HashMap<&str, (&str, &str)> = routes
            .iter()
            .map(|r| {
                (
                    r.handler_pg_id.as_str(),
                    (r.handler_repo.as_str(), r.handler_fqn.as_str()),
                )
            })
            .collect();

        let cross_repo_links = links.iter().filter(|l| l.cross_repo).count();
        let intra_repo_links = links.len() - cross_repo_links;

        let items: Vec<http_link::CrossServiceLinkItem> = links
            .iter()
            .map(|l| {
                let (caller_repo, caller_fqn) = caller_label
                    .get(l.caller_pg_id.as_str())
                    .copied()
                    .unwrap_or(("", ""));
                let (handler_repo, handler_fqn) = handler_label
                    .get(l.handler_pg_id.as_str())
                    .copied()
                    .unwrap_or(("", ""));
                http_link::CrossServiceLinkItem {
                    caller_repo: caller_repo.to_string(),
                    caller_fqn: caller_fqn.to_string(),
                    handler_repo: handler_repo.to_string(),
                    handler_fqn: handler_fqn.to_string(),
                    http_method: l.http_method.clone(),
                    http_path: l.http_path.clone(),
                    matched_via: l.matched_via.clone(),
                    cross_repo: l.cross_repo,
                }
            })
            .collect();

        // Unmatched: per-call-site dangling — a call whose (caller, method, path)
        // never appears among the links (see http_link::dangling_calls doc).
        let unmatched = http_link::dangling_calls(&calls, &links);

        Ok(http_link::CrossServiceLinkReport {
            total_calls: calls.len(),
            total_routes: routes.len(),
            links_created: links.len(),
            cross_repo_links,
            intra_repo_links,
            links: items,
            unmatched,
        })
    }

    /// Batch-resolve note titles for a set of timeline members: dedup the ids,
    /// fetch briefs in one query, and build an id→title map ("(untitled)" when a
    /// title is null). Shared by gs_trace_decision_history / gs_get_decision_lineage.
    async fn resolve_member_titles(
        &self,
        members: &[decision_lineage::ChainMember],
    ) -> DomainResult<std::collections::HashMap<uuid::Uuid, String>> {
        let ids: Vec<uuid::Uuid> = members
            .iter()
            .map(|m| m.note_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let brief = self.note_repo.fetch_notes_brief_batch(&ids).await?;
        Ok(brief
            .into_iter()
            .map(|r| (r.id, r.title.unwrap_or_else(|| "(untitled)".into())))
            .collect())
    }

    pub(crate) async fn gs_trace_decision_history(
        &self,
        repo: String,
        symbol: String,
    ) -> DomainResult<Vec<decision_lineage::DecisionTimelineEntry>> {
        // 1. DECISION notes attached to chunks matching `symbol`.
        let attachments = self
            .graph_read_repo
            .fetch_decisions_for_symbol(&repo, &symbol)
            .await?;

        // 2. Walk each distinct decision's supersede chain; flatten members.
        let mut seen_seeds: std::collections::HashSet<uuid::Uuid> =
            std::collections::HashSet::new();
        let mut members: Vec<decision_lineage::ChainMember> = Vec::new();
        for a in &attachments {
            if !seen_seeds.insert(a.note_id) {
                continue;
            }
            let chain = self
                .graph_read_repo
                .fetch_supersede_chain(a.note_id)
                .await?;
            for r in chain {
                members.push(decision_lineage::ChainMember {
                    note_id: r.note_id,
                    direction: r.direction,
                    hop_distance: r.hop_distance,
                });
            }
        }

        // 3. Resolve titles from PG for every note in the timeline.
        let titles = self.resolve_member_titles(&members).await?;

        // 4. Merge + order.
        Ok(decision_lineage::merge_decision_timeline(&members, &titles))
    }

    pub(crate) async fn gs_get_decision_lineage(
        &self,
        note_id: uuid::Uuid,
    ) -> DomainResult<decision_lineage::DecisionLineageReport> {
        let chain = self.graph_read_repo.fetch_supersede_chain(note_id).await?;
        let members: Vec<decision_lineage::ChainMember> = chain
            .into_iter()
            .map(|r| decision_lineage::ChainMember {
                note_id: r.note_id,
                direction: r.direction,
                hop_distance: r.hop_distance,
            })
            .collect();

        let titles = self.resolve_member_titles(&members).await?;
        let timeline = decision_lineage::merge_decision_timeline(&members, &titles);

        let chunk_rows = self
            .graph_read_repo
            .fetch_chunks_attached_to_note(note_id)
            .await?;
        let attached_chunks = chunk_rows
            .into_iter()
            .map(|c| decision_lineage::AttachedChunk {
                name: c.name,
                fqn: c.fqn,
                module_path: c.module_path,
                chunk_type: c.chunk_type,
            })
            .collect();

        Ok(decision_lineage::DecisionLineageReport {
            note_id,
            timeline,
            attached_chunks,
        })
    }
}
