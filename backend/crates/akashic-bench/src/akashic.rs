//! Akashic retrieval strategy: turn a fixture `(tool, args)` step into the
//! exact string the corresponding MCP tool hands the agent, by calling the
//! same read-side service method + `algos::format_*` the tool wraps, over the
//! live PG+Neo4j graph. No MCP server / transport / rmcp machinery — the
//! services + formatters ARE the code the tools wrap.
//!
//! Read-only w.r.t. graph/chunk/embedding data. (Service wiring goes through
//! `akashic_test_support::build_app_state`, which runs idempotent auth-schema
//! DDL — inert on an existing dev DB — but performs no data writes.)
//!
//! The five dispatched tools were chosen so none needs the embedder:
//! `traverse_code_calls` / `trace_execution_flow` (service returns a
//! pre-formatted String), `analyze_impact` (→ `format_impact`),
//! `detect_dead_code` (→ `format_dead_code`), and `goto_definition` (formatting
//! reproduced here verbatim from the MCP handler, since it formats inline).

use crate::metrics::Blob;
use crate::questions::AkashicStep;
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;

use akashic_domain::algos::{dead_code, impact as impact_algo};
use akashic_domain::ports::services::{GraphService, NavigationService};

/// A pluggable executor for one akashic-plan step. The trait lets the
/// deterministic integration test inject a fake dispatcher (no DB).
#[async_trait]
pub trait AkashicDispatcher: Send + Sync {
    async fn dispatch(&self, step: &AkashicStep) -> Result<Blob>;
}

/// Execute an akashic plan step-by-step, collecting one blob per step.
pub async fn run(dispatcher: &dyn AkashicDispatcher, plan: &[AkashicStep]) -> Result<Vec<Blob>> {
    let mut out = Vec::with_capacity(plan.len());
    for step in plan {
        out.push(dispatcher.dispatch(step).await?);
    }
    Ok(out)
}

/// The real dispatcher: read-side services over the live graph.
pub struct ServiceDispatcher {
    nav: Arc<dyn NavigationService>,
    graph: Arc<dyn GraphService>,
}

impl ServiceDispatcher {
    /// Wire the services via the shared `build_app_state` helper. The caller
    /// (CLI) must set `DATABASE_URL` / `NEO4J_URI` / `NEO4J_USER` /
    /// `NEO4J_PASSWORD` in the environment first — `build_app_state` reads them
    /// (falling back to the dev defaults) and panics on a connection failure,
    /// which for this integration CLI is the intended fail-loud behavior.
    pub async fn connect() -> Result<Self> {
        let app = akashic_test_support::build_app_state("http://unused".into()).await;
        Ok(Self {
            nav: app.navigation_service.clone(),
            graph: app.graph_service.clone(),
        })
    }
}

// Defaults MUST mirror the real MCP arg structs in
// `crates/akashic-mcp/src/mcp/types.rs` so an omitted fixture arg produces the
// SAME tool behaviour the agent would get (else the measured akashic output
// drifts from the real tool). TraverseCallsArgs: max_depth=3, min_confidence=0.6.
// AnalyzeImpactArgs: max_depth=2. TraceFlowArgs: max_depth=8.
fn default_traverse_depth() -> u8 {
    3
}
fn default_confidence() -> f32 {
    0.6
}
fn default_impact_depth() -> u8 {
    2
}
fn default_trace_depth() -> u8 {
    8
}
fn default_direction() -> String {
    "both".into()
}
fn default_max_candidates() -> usize {
    50
}

#[derive(Deserialize)]
struct TraverseArgs {
    symbol: String,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default = "default_direction")]
    direction: String,
    #[serde(default = "default_traverse_depth")]
    max_depth: u8,
    #[serde(default = "default_confidence")]
    min_confidence: f32,
}

#[derive(Deserialize)]
struct AnalyzeArgs {
    target: String,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default = "default_impact_depth")]
    max_depth: u8,
    #[serde(default)]
    min_score: f64,
}

#[derive(Deserialize)]
struct DeadCodeArgs {
    repo: String,
    #[serde(default = "default_max_candidates")]
    max_candidates: usize,
}

#[derive(Deserialize)]
struct TraceArgs {
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    entry_type: Option<String>,
    #[serde(default = "default_trace_depth")]
    max_depth: u8,
    #[serde(default)]
    list_all: bool,
}

#[derive(Deserialize)]
struct GotoArgs {
    name: String,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    module_hint: Option<String>,
    #[serde(default)]
    kind_hint: Option<String>,
}

#[async_trait]
impl AkashicDispatcher for ServiceDispatcher {
    async fn dispatch(&self, step: &AkashicStep) -> Result<Blob> {
        match step.tool.as_str() {
            "traverse_code_calls" => {
                let a: TraverseArgs = serde_json::from_value(step.args.clone())
                    .context("traverse_code_calls args")?;
                let out = self
                    .nav
                    .traverse_code_calls(
                        a.symbol,
                        a.repo,
                        a.direction,
                        a.max_depth.clamp(1, 4),
                        a.min_confidence.clamp(0.0, 1.0),
                    )
                    .await
                    .map_err(|e| anyhow!("traverse_code_calls failed: {e}"))?;
                Ok(Blob(out))
            }
            "analyze_impact" => {
                let a: AnalyzeArgs =
                    serde_json::from_value(step.args.clone()).context("analyze_impact args")?;
                let report = self
                    .nav
                    .analyze_impact(
                        a.target.clone(),
                        a.repo,
                        a.max_depth.clamp(1, 4),
                        a.min_score,
                    )
                    .await
                    .map_err(|e| anyhow!("analyze_impact failed: {e}"))?;
                let risk = impact_algo::risk_level(&report.affected);
                Ok(Blob(impact_algo::format_impact(
                    &a.target,
                    &report.affected,
                    risk,
                    &report.flows,
                    &report.suggested_tests,
                )))
            }
            "detect_dead_code" => {
                let a: DeadCodeArgs =
                    serde_json::from_value(step.args.clone()).context("detect_dead_code args")?;
                let report = self
                    .graph
                    .detect_dead_code(a.repo.clone())
                    .await
                    .map_err(|e| anyhow!("detect_dead_code failed: {e}"))?;
                Ok(Blob(dead_code::format_dead_code(
                    &a.repo,
                    &report,
                    a.max_candidates,
                )))
            }
            "trace_execution_flow" => {
                let a: TraceArgs = serde_json::from_value(step.args.clone())
                    .context("trace_execution_flow args")?;
                let out = self
                    .nav
                    .trace_execution_flow(
                        a.symbol,
                        a.repo,
                        a.entry_type,
                        a.max_depth.clamp(1, 12),
                        a.list_all,
                    )
                    .await
                    .map_err(|e| anyhow!("trace_execution_flow failed: {e}"))?;
                Ok(Blob(out))
            }
            "goto_definition" => {
                let a: GotoArgs =
                    serde_json::from_value(step.args.clone()).context("goto_definition args")?;
                let cands = self
                    .nav
                    .goto_definition(a.name.clone(), a.repo, a.module_hint, a.kind_hint)
                    .await
                    .map_err(|e| anyhow!("goto_definition failed: {e}"))?;
                Ok(Blob(format_goto_definition(&a.name, &cands)))
            }
            other => bail!("akashic_plan references unknown tool '{other}'"),
        }
    }
}

/// Reproduce the MCP `goto_definition` handler's inline formatting verbatim
/// (crates/akashic-mcp/src/mcp/tools/nav.rs) so the measured output matches the
/// real tool byte-for-byte.
fn format_goto_definition(name: &str, cands: &[akashic_domain::types::SymbolCandidate]) -> String {
    if cands.is_empty() {
        return format!(
            "No definition found for '{name}'. Try search_knowledge for a semantic search.",
        );
    }
    let mut out = String::new();
    if cands.len() == 1 {
        out.push_str(&format!("=== DEFINITION of '{name}' ===\n"));
    } else {
        out.push_str(&format!(
            "=== {} CANDIDATES for '{name}' (ambiguous — pass an fqn to find_references) ===\n",
            cands.len(),
        ));
    }
    for c in cands {
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
    out
}
