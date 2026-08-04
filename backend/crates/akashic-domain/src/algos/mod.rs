//! Pure, DB-free algorithms relocated from service crates (A1 Task 2).
//!
//! Every module here is infra-free: no sqlx, neo4rs, reqwest, or IO.
//! These can be unit-tested with `cargo test -p akashic-domain` and no database.

pub mod code_community;
pub mod community;
pub mod corpus_contract;
pub mod dead_code;
pub mod decision_lineage;
pub mod dedup;
pub mod entry_points;
pub mod file_routes;
pub mod git_ref;
pub mod go_structural;
pub mod http_link;
pub mod impact;
pub mod llms;
pub mod memory_format;
pub mod ranking;
pub mod rrf;
pub mod saga_status;

// ── Convenient re-exports ────────────────────────────────────────────────────

pub use community::{run_leiden, try_leiden, union_find_components};
pub use corpus_contract::{SlugCounter, check_links, github_slug, parse_nav, parse_version_header};
pub use dead_code::{
    CAVEAT, Confidence, DeadCodeCandidate, DeadCodeFn, DeadCodeReport, build_report,
    format_dead_code, rank,
};
pub use decision_lineage::{
    AttachedChunk, ChainMember, DecisionLineageReport, DecisionTimelineEntry,
    format_decision_history, format_decision_lineage, merge_decision_timeline,
};
pub use dedup::{format_block, format_warn, verdict_message};
pub use entry_points::{DetectedEntryPoint, detect};
pub use file_routes::{FileRoute, file_based_route};
pub use git_ref::resolve_reingest_git_ref;
pub use go_structural::{
    go_interface_methods, go_method_receiver, go_structural_implements, go_type_is_interface,
};
pub use http_link::{
    CrossServiceLink, CrossServiceLinkItem, CrossServiceLinkReport, HttpCallSite, RouteSite,
    dangling_calls, format_cross_service_links, link_calls, path_matches,
};
pub use impact::{
    RawImpactEdge, aggregate_change_impacts, aggregate_impacts, format_change_impact,
    format_impact, path_score, risk_level,
};
pub use memory_format::format_top_note_line;
pub use ranking::{
    CONCEPT_ARCHETYPE_TEXT, SYMBOL_ARCHETYPE_TEXT, analyze as analyze_query, set_archetypes,
};
pub use rrf::{
    FusedItem, RankedItem, RankedItemMeta, cross_space_merge, multi_source_rrf, weighted_rrf,
};
pub use saga_status::{SagaStatus, StepStatus, extract_issue_from_branch};
