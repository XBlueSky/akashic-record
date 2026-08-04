//! notes MCP tools (Slice E tools.rs split). One of the composed
//! `#[tool_router]` blocks; combined in `super`'s `AkashicMcp::new`.
use super::*;

#[tool_router(router = notes_tools, vis = "pub(crate)")]
impl AkashicMcp {
    // ══════════════════════════════════════════════════════════════════
    // save_note — thinned (A2a-T10): delegates core orchestration to
    // CurationService (embed + dedup + saga + PG insert + Neo4j create
    // with compensating rollback). Non-fatal saga/tag/symbol graph
    // edges remain here as post-save steps (require db/pg handles not
    // yet in scope of CurationService).
    // ══════════════════════════════════════════════════════════════════

    /// Save a piece of knowledge linked to a repository and branch.
    #[tool(
        name = "save_note",
        description = "Save knowledge linked to a Git repository and branch. \
Category must be one of: ARCHITECTURE, BUG_FIX, CONFIG, ONBOARDING, DECISION. \
GUARDRAILS: \
1) READ FIRST — always search_knowledge before saving to avoid duplicates. \
2) BE TIME-AWARE — no relative time ('today'). Use exact versions/dates. \
3) BE OBJECTIVE — factual engineering notes, no first-person pronouns. \
4) FORMAT — summary: actionable one-liner ≤100 chars. content: WHAT/WHY/IMPACT/ACTION."
    )]
    async fn save_note(
        &self,
        Parameters(args): Parameters<SaveNoteArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        // Write-tool gate (Slice E): anonymous callers are rejected.
        let actor = require_actor(&ctx)?;
        // Input validation (MCP-layer: before delegation).
        if !CATEGORIES.contains(&args.category.as_str()) {
            return Err(format!(
                "Invalid category '{}'. Must be one of: {}",
                args.category,
                CATEGORIES.join(", ")
            ));
        }
        // FIX(finding-2): count Unicode scalar values, not UTF-8 bytes.
        let summary_chars = args.summary.chars().count();
        if summary_chars > 100 {
            return Err(format!(
                "Summary too long ({summary_chars} chars). Must be ≤100 characters."
            ));
        }
        for (i, fact) in args.facts.iter().enumerate() {
            let fact_chars = fact.chars().count();
            if fact_chars > 200 {
                return Err(format!(
                    "Fact #{} too long ({fact_chars} chars). Each fact must be ≤200 characters.",
                    i + 1
                ));
            }
        }

        // Resolve supersedes UUID for the service request.
        let supersedes = args
            .supersedes
            .as_deref()
            .and_then(|s| uuid::Uuid::parse_str(s).ok());

        // ── Delegate to CurationService (embed + dedup + PG insert + Neo4j
        //    create with compensating rollback preserved in service). ─────────
        use akashic_domain::ports::services::SaveNoteRequest;
        let saved = self
            .curation_service
            .save_note(SaveNoteRequest {
                repo: args.repo_name.clone(),
                branch: args.branch_name.clone(),
                category: args.category.clone(),
                title: args.title.clone(),
                summary: args.summary.clone(),
                content: args.content.clone(),
                facts: args.facts.clone(),
                related_symbols: args.related_symbols.clone(),
                related_files: args.related_files.clone(),
                tags: args.tags.clone(),
                issue_ref: args.issue_ref.clone(),
                topic: args.topic.clone(),
                supersedes,
                skip_dedup: args.skip_dedup,
            })
            .await
            .map_err(|e| e.to_string())?;

        let pg_id = saved.note_id.to_string();

        // ── Post-save: non-fatal saga Neo4j PART_OF link ──────────────
        // Uses the saga_id the service already resolved (no re-resolution);
        // the edge write lives here because the service holds no EdgeRepo.
        if let Some(sid) = saved.saga_id {
            let saga_pg_id = sid.to_string();
            let saga_name = args
                .issue_ref
                .as_deref()
                .or(args.topic.as_deref())
                .unwrap_or("unnamed");
            self.db
                .execute(
                    query(
                        "MERGE (s:Saga {pg_id: $saga_pg_id}) \
                         ON CREATE SET s.repo_name = $repo, s.name = $saga_name \
                         WITH s \
                         MATCH (n:Note {pg_id: $note_pg_id}) \
                         MERGE (n)-[:PART_OF]->(s)",
                    )
                    .param("saga_pg_id", saga_pg_id.as_str())
                    .param("repo", args.repo_name.as_str())
                    .param("saga_name", saga_name)
                    .param("note_pg_id", pg_id.as_str()),
                )
                .await
                .ok(); // non-fatal
        }

        // ── Post-save: TAGGED_WITH edges for tags ──────────────────────
        // One round-trip (UNWIND) and NON-FATAL, matching the saga/chunk links
        // above: the note is already persisted, so a tag-edge hiccup must not
        // fail the whole tool call (nor skip the chunk-linking step below).
        if !args.tags.is_empty() {
            let tags: Vec<String> = args.tags.iter().map(|t| t.to_lowercase()).collect();
            if let Err(e) = self
                .db
                .execute(
                    query(
                        "MATCH (n:Note {pg_id: $pg_id}) \
                         UNWIND $tags AS tagname \
                         MERGE (t:Tag {name: tagname}) \
                         MERGE (n)-[:TAGGED_WITH]->(t)",
                    )
                    .param("pg_id", pg_id.as_str())
                    .param("tags", tags),
                )
                .await
            {
                tracing::warn!(err = %e, "Failed to create TAGGED_WITH edges");
            }
        }

        // ── Post-save: auto-link to chunks (non-fatal) ─────────────────
        if !args.related_symbols.is_empty() {
            let symbol_repo_link: Arc<dyn SymbolRepo> =
                Arc::new(PgSymbolRepo::new(self.pg.clone()));
            let edge_repo_link: Arc<dyn EdgeRepo> = Arc::new(Neo4jEdgeRepo::new(self.db.clone()));
            if let Err(e) = linking::link_note_to_chunks(
                &symbol_repo_link,
                &edge_repo_link,
                &pg_id,
                &args.repo_name,
                &args.related_symbols,
            )
            .await
            {
                tracing::warn!(err = %e, "Failed to link note to chunks");
            }
        }

        info!(pg_id, repo = %args.repo_name, branch = %args.branch_name, "Note saved");

        spawn_write_audit(&self.pg, actor, "save_note", &args, true);
        Ok(format!(
            "Note saved successfully.\nuuid: {pg_id}\nrepo: {}\nbranch: {}\ncategory: {}\ntitle: {}",
            args.repo_name, args.branch_name, args.category, args.title
        ))
    }

    // ══════════════════════════════════════════════════════════════════
    // get_note_health — note staleness and health check
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "get_note_health",
        description = "Check health of knowledge notes in a repository. \
Shows stale notes (deleted symbols, content divergence), usage stats, and auto-archived notes."
    )]
    async fn get_note_health(
        &self,
        Parameters(args): Parameters<NoteHealthArgs>,
    ) -> Result<String, String> {
        let note_health: Arc<dyn NoteHealthRepo> = Arc::new(PgNoteHealthRepo::new(self.pg.clone()));
        let summary = akashic_curation::notes::health::get_health_summary(
            &note_health,
            &args.repo,
            args.min_staleness,
        )
        .await
        .map_err(|e| format!("Health check failed: {e}"))?;

        let mut out = format!("=== NOTE HEALTH: {} ===\n\n", args.repo);
        out.push_str(&format!(
            "Total: {}  Healthy: {}  Needs Review: {}  Likely Stale: {}  Archived: {}\n\n",
            summary.total_notes,
            summary.healthy,
            summary.needs_review,
            summary.likely_stale,
            summary.archived
        ));

        if summary.stale_notes.is_empty() {
            out.push_str("All notes are healthy!\n");
        } else {
            for note in &summary.stale_notes {
                let icon = if note.staleness_score >= 0.7 {
                    "!!"
                } else {
                    "?"
                };
                out.push_str(&format!(
                    "  [{icon}] [{:.2}] \"{}\" — accesses: {}\n",
                    note.staleness_score, note.title, note.access_count
                ));
                for reason in &note.staleness_reasons {
                    out.push_str(&format!("         {reason}\n"));
                }
                out.push_str(&format!("         -> {}\n", note.suggestion));
            }
        }

        info!(repo = %args.repo, stale = summary.stale_notes.len(), "get_note_health completed");
        Ok(out)
    }

    // ══════════════════════════════════════════════════════════════════
    // supersede_note — thinned (A2a-T10): delegates to CurationService
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "supersede_note",
        description = "Mark an old note as superseded by a new one. Sets invalid_at on the old note and links it to the replacement. Use this when knowledge has been updated and the old version should be retired from search results."
    )]
    async fn supersede_note(
        &self,
        Parameters(args): Parameters<SupersedeNoteArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        // Write-tool gate (Slice E): anonymous callers are rejected.
        let actor = require_actor(&ctx)?;
        let old_uuid = uuid::Uuid::parse_str(&args.old_note_id)
            .map_err(|e| format!("Invalid old_note_id: {e}"))?;
        let new_uuid = uuid::Uuid::parse_str(&args.new_note_id)
            .map_err(|e| format!("Invalid new_note_id: {e}"))?;

        self.curation_service
            .supersede_note(old_uuid, new_uuid)
            .await
            .map_err(|e| e.to_string())?;

        spawn_write_audit(&self.pg, actor, "supersede_note", &args, true);
        Ok(format!(
            "✓ Note {} marked as superseded by {}",
            args.old_note_id, args.new_note_id
        ))
    }

    // ══════════════════════════════════════════════════════════════════
    // list_sagas — list narrative sagas for a repository
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "list_sagas",
        description = "List narrative sagas (issue/branch/topic threads) for a repository. Each saga groups related notes into a coherent storyline. Filter by status: active, resolved, archived."
    )]
    async fn list_sagas(
        &self,
        Parameters(args): Parameters<ListSagasArgs>,
    ) -> Result<String, String> {
        let saga_repo: Arc<dyn SagaRepo> = Arc::new(PgSagaRepo::new(self.pg.clone()));
        let sagas = akashic_curation::notes::saga::list_sagas(
            &saga_repo,
            &args.repo_name,
            args.status.as_deref(),
        )
        .await
        .map_err(|e| e.to_string())?;

        if sagas.is_empty() {
            return Ok(format!("No sagas found for {}", args.repo_name));
        }

        let mut lines = vec![format!(
            "[SAGAS] {} total for {}",
            sagas.len(),
            args.repo_name
        )];
        for s in &sagas {
            let name = s.saga.name.as_deref().unwrap_or("unnamed");
            let ref_str = s
                .saga
                .source_ref
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default();
            let stype = s.saga.source_type.as_deref().unwrap_or("unknown");
            lines.push(format!(
                "  [{id}] {name}{ref_str} | {stype} | {status} | {count} notes",
                id = s.saga.id,
                name = name,
                ref_str = ref_str,
                stype = stype,
                status = s.saga.status,
                count = s.note_count,
            ));
        }

        Ok(lines.join("\n"))
    }

    // ══════════════════════════════════════════════════════════════════
    // get_saga_timeline — chronological timeline of notes in a saga
    // ══════════════════════════════════════════════════════════════════

    #[tool(
        name = "get_saga_timeline",
        description = "Get the chronological timeline of notes within a saga, including superseded notes. Shows the full narrative arc of an issue, feature branch, or topic."
    )]
    async fn get_saga_timeline(
        &self,
        Parameters(args): Parameters<GetSagaTimelineArgs>,
    ) -> Result<String, String> {
        let saga_uuid =
            uuid::Uuid::parse_str(&args.saga_id).map_err(|e| format!("Invalid saga_id: {e}"))?;

        let saga_repo: Arc<dyn SagaRepo> = Arc::new(PgSagaRepo::new(self.pg.clone()));
        let (saga, entries) =
            akashic_curation::notes::saga::get_saga_timeline(&saga_repo, saga_uuid)
                .await
                .map_err(|e| e.to_string())?;

        let saga_name = saga.name.as_deref().unwrap_or("unnamed");
        let ref_str = saga
            .source_ref
            .as_deref()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        let stype = saga.source_type.as_deref().unwrap_or("unknown");
        let mut lines = vec![format!(
            "[SAGA] {}{} | {} | {}",
            saga_name, ref_str, stype, saga.status
        )];

        for e in &entries {
            let date = e
                .valid_at
                .map(|t| t.format("%Y-%m-%d").to_string())
                .or_else(|| Some(e.created_at.format("%Y-%m-%d").to_string()))
                .unwrap_or_default();
            let title = e.title.as_deref().unwrap_or("(untitled)");
            let status = if e.invalid_at.is_some() {
                format!("superseded → {}", e.superseded_by.as_deref().unwrap_or("?"))
            } else {
                "valid".to_string()
            };
            lines.push(format!(
                "  [{id}] {date} ({cat}) \"{title}\" — {status}",
                id = e.uuid,
                date = date,
                cat = e.category,
                title = title,
                status = status,
            ));
        }

        Ok(lines.join("\n"))
    }
}
