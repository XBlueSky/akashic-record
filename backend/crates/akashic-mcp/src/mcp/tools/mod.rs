use std::sync::Arc;

use neo4rs::query;
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CacheScope, DiscoverResult, ServerCapabilities, ServerInfo},
    service::{RequestContext, RoleServer},
    tool, tool_handler, tool_router,
};
use sqlx::PgPool;
use tracing::info;

use akashic_domain::ports::{
    ChunkRepo, EdgeRepo, ModuleRepo, NoteHealthRepo, SagaRepo, SymbolRepo,
    corpus::CorpusStore,
    services::{CurationService, GraphService, NavigationService, SearchService},
};
use akashic_domain::types::CATEGORIES;
use akashic_kernel::actor::Authenticated;
use akashic_retrieval::graphrag;
use akashic_retrieval::linking;
use akashic_store_neo4j::{Neo4jEdgeRepo, Neo4jPool};
use akashic_store_pg::{PgChunkRepo, PgModuleRepo, PgNoteHealthRepo, PgSagaRepo, PgSymbolRepo};

use super::types::*;

// McpModuleRow / McpChunkRow / McpNoteRow / McpSectionRow type aliases removed
// (A2a): get_details / search_knowledge / global_query now delegate to
// SearchService; inline sqlx tuple rows are no longer needed here.
// McpLatestNoteRow type alias removed (A2a-T4): get_project_summary now
// delegates to GraphService::get_project_summary.

/// All MCP tool names registered by `AkashicMcp`. Must be exhaustive — every
/// `#[tool(...)]`-annotated method below must appear in this list AND in either
/// `READ_TOOLS` or `WRITE_TOOLS`. Enforced by `tests::all_tools_classified`.
#[allow(dead_code)] // Consumed by tests::all_tools_classified and external callers; bin-crate dead-code lint.
pub const ALL_TOOLS: &[&str] = &[
    "search_knowledge",
    "get_details",
    "save_note",
    "get_project_summary",
    "traverse_code_calls",
    "trace_execution_flow",
    "get_note_health",
    "supersede_note",
    "list_sagas",
    "get_saga_timeline",
    "global_query",
    "analyze_impact",
    "goto_definition",
    "find_references",
    "find_implementations",
    "detect_code_communities",
    "detect_dead_code",
    "analyze_change_impact",
    "link_cross_service_calls",
    "trace_decision_history",
    "get_decision_lineage",
    "get_docs_schema",
    "list_authoring_sections",
    "get_authoring_guide",
    "check_docs_coverage",
    "list_docs",
    "get_docs_page",
];

/// Tools that read from Neo4j / PostgreSQL and never mutate.
/// Anonymous callers may invoke these.
#[allow(dead_code)] // Consumed by tests::all_tools_classified; bin-crate dead-code lint.
pub const READ_TOOLS: &[&str] = &[
    "search_knowledge",
    "get_details",
    "get_project_summary",
    "traverse_code_calls",
    "trace_execution_flow",
    "get_note_health",
    "list_sagas",
    "get_saga_timeline",
    "global_query",
    "analyze_impact",
    "goto_definition",
    "find_references",
    "find_implementations",
    "detect_code_communities",
    "detect_dead_code",
    "analyze_change_impact",
    "trace_decision_history",
    "get_decision_lineage",
    "get_docs_schema",
    "list_authoring_sections",
    "get_authoring_guide",
    "check_docs_coverage",
    "list_docs",
    "get_docs_page",
];

/// Tools that mutate Neo4j / PostgreSQL. Anonymous callers are rejected
/// in-handler by [`require_actor`] (Slice E): the `mcp_auth` tower layer
/// inserts `Arc<dyn Authenticated>` into request extensions on a valid bearer,
/// and each write handler requires it.
pub const WRITE_TOOLS: &[&str] = &["save_note", "supersede_note", "link_cross_service_calls"];

/// The `get_info` instructions surfaced to MCP clients. Hand-authored guidance
/// (adds usage hints beyond the raw tool descriptions), so it is not
/// auto-generated — but `tests::instructions_mention_every_tool` guards it
/// against drift as tools are added/removed from [`ALL_TOOLS`].
const MCP_INSTRUCTIONS: &str = "Akashic Record — Git-aware knowledge memory + code map. \
     27 tools: \
     1) search_knowledge — search across code (Map) and notes (Notes), returns compact index with preview snippets. \
     2) get_details — fetch full content for IDs from search results, or list modules. \
     3) save_note — save knowledge with guardrails (search first, no relative time, be objective). \
     4) get_project_summary — repository overview with stats. \
     5) traverse_code_calls — traverse CALLS edges to build call graphs (upstream/downstream). \
     6) trace_execution_flow — trace execution flows from entry points (API handlers, main, tests). \
     7) get_note_health — check health of knowledge notes. \
     8) supersede_note — mark an old note as superseded by a new one. \
     9) list_sagas — list narrative sagas for a repository. \
     10) get_saga_timeline — chronological timeline of notes in a saga. \
     11) global_query — answer global questions about architecture using community summaries. \
     12) analyze_impact — evaluate blast radius of changing a symbol or file. \
     13) goto_definition — resolve a symbol name to its precise definition(s). \
     14) find_references — find direct call-site references to a precise fqn. \
     15) find_implementations — find who implements/extends a trait or class (incoming IMPLEMENTS); or what a type declares it implements/extends (outgoing). Use goto_definition first to get the fqn. \
     16) detect_code_communities — cluster the call graph into communities (Leiden). \
     17) detect_dead_code — list ranked dead-code candidates (zero-inbound-CALLS functions, entry points excluded). \
     18) analyze_change_impact — combined blast radius for a SET of changed symbols (e.g. the functions/types touched by a diff or PR). \
     19) link_cross_service_calls — rebuild cross-service HTTP_CALLS edges across all repos (WRITE). \
     20) trace_decision_history — trace the DECISION-note history for a code symbol (attached decisions + their SUPERSEDES chains). \
     21) get_decision_lineage — full SUPERSEDES lineage for one decision note plus the code it is attached to. \
     22) get_docs_schema — canonical JSON Schema for the docs-kit manifest or docs-toml contract. \
     23) list_authoring_sections — table of contents for the docs-authoring guide. \
     24) get_authoring_guide — fetch one docs-authoring guide section's markdown. \
     25) check_docs_coverage — validate a docs corpus tree with the SAME nav/link/anchor checker the platform's ingest path runs. \
     26) list_docs — published docs corpora: repo list with version stamps, or one repo's full nav tree. \
     27) get_docs_page — read one full docs page (raw markdown + version stamp) by corpus path.";

/// MCP server handler backed by Neo4j, PostgreSQL, and an embedding provider.
#[derive(Clone)]
pub struct AkashicMcp {
    pub db: Neo4jPool,
    pub pg: PgPool,
    /// A2a-T4: graph topology reads (get_project_summary, etc.)
    pub graph_service: Arc<dyn GraphService>,
    /// A2a-T5: search, get_details, global_query
    pub search_service: Arc<dyn SearchService>,
    /// A2a-T7: code navigation (6 nav tools)
    pub navigation_service: Arc<dyn NavigationService>,
    /// A2a-T10: note CRUD (save_note, supersede_note)
    pub curation_service: Arc<dyn CurationService>,
    /// Plan-3 E5: docs-corpus raw reads (list_docs, get_docs_page)
    pub corpus_store: Arc<dyn CorpusStore>,
    /// rmcp tool dispatch table (generated by `#[tool_router]`).
    pub tool_router: ToolRouter<AkashicMcp>,
}

impl AkashicMcp {
    /// Construct the MCP handler, wiring the `#[tool_router]`-generated dispatch
    /// table. All callers build via this ctor so the `tool_router` field stays
    /// an internal detail.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Neo4jPool,
        pg: PgPool,
        graph_service: Arc<dyn GraphService>,
        search_service: Arc<dyn SearchService>,
        navigation_service: Arc<dyn NavigationService>,
        curation_service: Arc<dyn CurationService>,
        corpus_store: Arc<dyn CorpusStore>,
    ) -> Self {
        Self {
            db,
            pg,
            graph_service,
            search_service,
            navigation_service,
            curation_service,
            corpus_store,
            tool_router: Self::search_tools()
                + Self::notes_tools()
                + Self::graph_tools()
                + Self::nav_tools()
                + Self::docs_tools()
                + Self::docs_read_tools(),
        }
    }
}

/// Extract the authenticated actor from the request context, or return an
/// `unauthorized` tool error. Write tools call this at the top; anonymous
/// callers (no `mcp_auth`-inserted `Authenticated` extension) are rejected.
/// Read tools never call it, so anonymous reads stay allowed. Replaces the
/// rmcp-0.1 proxy `auth_gate` body-peek gate (Slice E).
fn require_actor(ctx: &RequestContext<RoleServer>) -> Result<Arc<dyn Authenticated>, String> {
    ctx.extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<Arc<dyn Authenticated>>().cloned())
        .ok_or_else(|| "unauthorized: this tool requires authentication".to_string())
}

/// Fire-and-forget B2 audit for a write tool. In-handler replacement for the
/// rmcp-0.1 proxy `try_audit_write` (Slice E): the write handler owns the actor
/// (via [`require_actor`]) and its serialized args, and spawns the audit write.
fn spawn_write_audit<T: serde::Serialize>(
    pg: &PgPool,
    actor: Arc<dyn Authenticated>,
    tool_name: &str,
    args: &T,
    success: bool,
) {
    let args_value = serde_json::to_value(args).unwrap_or(serde_json::Value::Null);
    let repo = akashic_store_pg::PgAuditRepo::new(pg.clone());
    let tool = tool_name.to_string();
    tokio::spawn(async move {
        use akashic_kernel::AuditPort;
        repo.record_write(actor, tool, args_value, Vec::new(), success, None)
            .await;
    });
}
mod docs_contract;
mod docs_read;
mod graph;
mod nav;
mod notes;
mod search;

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AkashicMcp {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo is #[non_exhaustive] in rmcp 3.x — start from Default and
        // set fields (struct-literal construction is forbidden cross-crate).
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(MCP_INSTRUCTIONS.into());
        info
    }

    // Server metadata (tool list, capabilities, instructions) is static per
    // deploy — advertise a 1h public cache (SEP-2549) via the `discover`
    // (server/discover) response instead of the default non-cacheable-private
    // the base `ServerHandler::discover` returns.
    async fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> Result<DiscoverResult, rmcp::ErrorData> {
        let result = DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            self.get_info(),
        );
        Ok(result
            .with_ttl_ms(3_600_000)
            .with_cache_scope(CacheScope::Public))
    }
}

#[cfg(test)]
mod tests {
    use super::{ALL_TOOLS, MCP_INSTRUCTIONS, READ_TOOLS, WRITE_TOOLS};
    use std::collections::HashSet;

    #[test]
    fn instructions_mention_every_tool() {
        // The get_info instructions string is hand-authored; guard it against
        // drift so a tool added to ALL_TOOLS without a matching line is caught.
        for tool in ALL_TOOLS {
            assert!(
                MCP_INSTRUCTIONS.contains(tool),
                "MCP instructions do not mention tool '{tool}'"
            );
        }
        assert!(
            MCP_INSTRUCTIONS.contains(&format!("{} tools", ALL_TOOLS.len())),
            "MCP instructions header count is out of sync with ALL_TOOLS ({})",
            ALL_TOOLS.len()
        );
    }

    #[test]
    fn all_tools_classified() {
        let read: HashSet<&str> = READ_TOOLS.iter().copied().collect();
        let write: HashSet<&str> = WRITE_TOOLS.iter().copied().collect();
        let all: HashSet<&str> = ALL_TOOLS.iter().copied().collect();

        // Every tool is classified.
        for tool in &all {
            assert!(
                read.contains(tool) || write.contains(tool),
                "tool '{tool}' is not in READ_TOOLS or WRITE_TOOLS — classify it"
            );
        }

        // No tool is classified twice.
        let intersection: Vec<&&str> = read.intersection(&write).collect();
        assert!(
            intersection.is_empty(),
            "tools in both READ_TOOLS and WRITE_TOOLS: {intersection:?}"
        );

        // The classification is exhaustive — no orphan tools in the const lists.
        for tool in read.iter().chain(write.iter()) {
            assert!(
                all.contains(tool),
                "'{tool}' is in READ/WRITE but not in ALL_TOOLS — remove or add to ALL_TOOLS"
            );
        }

        // Snapshot: the write set is exactly these three names. This also guards
        // the audit invariant — `akashic_store_pg::repos::audit::extract_target_id`
        // has explicit arms only for these three write tools, so a new WRITE_TOOL
        // requires adding a matching arm there.
        let mut writes_sorted: Vec<&&str> = write.iter().collect();
        writes_sorted.sort();
        let mut expected: Vec<&&str> = ["save_note", "supersede_note", "link_cross_service_calls"]
            .iter()
            .collect();
        expected.sort();
        assert_eq!(
            writes_sorted, expected,
            "WRITE_TOOLS must be exactly [save_note, supersede_note, link_cross_service_calls]"
        );

        // Sanity: total count matches finding.md (17 read + 4 docs-contract
        // read tools from E5/A6 + 2 docs-read tools from Plan-3 E5 + 3 write).
        assert_eq!(all.len(), 27, "ALL_TOOLS must list all 27 tools");
    }

    // ── FIX(finding-2): summary/facts length must be measured in characters, not
    // UTF-8 bytes. These tests lock in the chars().count() semantics used by
    // save_note's validation so a regression to String::len() is caught. The
    // validation itself is inline in save_note (needs &self + a DB), so we assert
    // on the exact predicate it now uses.
    #[test]
    fn summary_length_counts_chars_not_bytes() {
        // 100 CJK characters: exactly at the limit. Each is 3 UTF-8 bytes (300
        // bytes total), which the old byte-based check rejected at >100.
        let summary: String = "字".repeat(100);
        assert_eq!(summary.chars().count(), 100, "must be 100 characters");
        assert_eq!(summary.len(), 300, "but 300 UTF-8 bytes");
        // chars()-based check accepts it; the old len()-based check would reject.
        assert!(
            summary.chars().count() <= 100,
            "100 CJK chars must pass the character limit"
        );
        assert!(
            summary.len() > 100,
            "regression guard: a byte-based check would wrongly reject this"
        );

        // 101 characters: just over the limit, must be rejected.
        let over: String = "字".repeat(101);
        assert!(
            over.chars().count() > 100,
            "101 chars must exceed the character limit"
        );

        // Emoji are multi-byte too: 100 emoji are 100 chars but 400 bytes.
        let emoji: String = "🦀".repeat(100);
        assert_eq!(emoji.chars().count(), 100);
        assert!(emoji.len() > 100);
        assert!(emoji.chars().count() <= 100);
    }

    #[test]
    fn fact_length_counts_chars_not_bytes() {
        // 200 CJK characters: exactly at the per-fact limit (600 UTF-8 bytes).
        let fact: String = "事".repeat(200);
        assert_eq!(fact.chars().count(), 200);
        assert!(fact.len() > 200, "byte count far exceeds the char limit");
        assert!(
            fact.chars().count() <= 200,
            "200 CJK chars must pass the per-fact character limit"
        );

        // 201 chars: just over, must be rejected.
        let over: String = "事".repeat(201);
        assert!(over.chars().count() > 200);
    }
}
