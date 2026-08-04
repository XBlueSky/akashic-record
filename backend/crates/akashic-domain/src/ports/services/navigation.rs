use super::*;

// ── NavigationService (6 methods) ─────────────────────────────────────────────

/// Use-case port for code-navigation operations.
///
/// Covers: call-graph traversal, goto-definition, reference/implementation
/// lookup, execution-flow tracing, and impact analysis.
#[async_trait]
pub trait NavigationService: Send + Sync {
    /// Traverse CALLS edges upstream and/or downstream from a symbol name.
    ///
    /// Returns a formatted plain-text call trace (matches the existing MCP
    /// `String` output; structured types deferred to A2 UI layer).
    ///
    /// MCP: `traverse_code_calls`
    async fn traverse_code_calls(
        &self,
        symbol: String,
        repo: Option<String>,
        direction: String,
        max_depth: u8,
        min_confidence: f32,
    ) -> crate::DomainResult<String>;

    /// Resolve a symbol NAME to ranked candidate definitions.
    ///
    /// MCP: `goto_definition`
    async fn goto_definition(
        &self,
        name: String,
        repo: Option<String>,
        module_hint: Option<String>,
        kind_hint: Option<String>,
    ) -> crate::DomainResult<Vec<SymbolCandidate>>;

    /// Find direct references (callers/importers) of a precise FQN.
    ///
    /// MCP: `find_references`
    async fn find_references(
        &self,
        fqn: String,
        repo: Option<String>,
        min_confidence: f64,
        include_kinds: Vec<String>,
    ) -> crate::DomainResult<Vec<ReferenceRow>>;

    /// Find implementations of a trait/interface/class by FQN.
    ///
    /// MCP: `find_implementations`
    async fn find_implementations(
        &self,
        fqn: String,
        repo: Option<String>,
        implementors: bool,
    ) -> crate::DomainResult<Vec<ReferenceRow>>;

    /// Trace execution flows that contain a symbol.
    ///
    /// Returns formatted plain-text output (matches existing MCP `String`
    /// output; structured types deferred to A2 UI layer).
    ///
    /// MCP: `trace_execution_flow`
    async fn trace_execution_flow(
        &self,
        symbol: Option<String>,
        repo: Option<String>,
        entry_type: Option<String>,
        max_depth: u8,
        list_all: bool,
    ) -> crate::DomainResult<String>;

    /// Evaluate blast radius of changing a symbol or file.
    ///
    /// MCP: `analyze_impact`
    async fn analyze_impact(
        &self,
        target: String,
        repo: Option<String>,
        max_depth: u8,
        min_score: f64,
    ) -> crate::DomainResult<ImpactReport>;

    /// Evaluate the COMBINED blast radius of a SET of changed symbols.
    ///
    /// Runs `analyze_impact`'s per-target ripple scoring for each changed
    /// symbol, merges + de-duplicates the affected nodes, unions affected flows
    /// and suggested tests, excludes the changed symbols themselves, and reports
    /// which changed symbols had dependents (`with_impact`) vs none
    /// (`without_impact`).
    ///
    /// MCP: `analyze_change_impact`
    async fn analyze_change_impact(
        &self,
        changed: Vec<String>,
        repo: Option<String>,
        max_depth: u8,
        min_score: f64,
    ) -> crate::DomainResult<ChangeImpactReport>;
}
