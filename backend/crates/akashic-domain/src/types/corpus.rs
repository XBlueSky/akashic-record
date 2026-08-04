//! Docs-corpus manifest + nav domain types (docs-kit ingestion, A6).
//!
//! `CorpusManifest` is the canonical artifact contract for the docs-kit
//! corpus pipeline: the CI packer writes it, the platform validator checks
//! against it, and [`manifest_json_schema`] — generated here via `schemars`,
//! not hand-written — is the single source of truth both sides consume. See
//! `backend/schemas/manifest.schema.json` (regenerate via the
//! `dump_manifest_schema` bin; never hand-edit it).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The canonical corpus manifest artifact produced by the CI packer.
///
/// `deny_unknown_fields` is load-bearing: an artifact with extra keys must be
/// rejected rather than silently accepted, since it means the packer and the
/// schema have drifted.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CorpusManifest {
    pub repo: String,
    pub version: String,
    pub sha: String,
    pub index: String,
    pub languages: Vec<String>,
    pub tool_version: String,
}

/// Navigation tree (sidebar structure) for a docs corpus. Persisted as-is in
/// the `corpus_versions.nav` JSONB column.
///
/// `description` is the index page's lede paragraph (see
/// `algos::corpus_contract::parse_nav`); it is not part of the manifest
/// schema, only of this in-memory/persisted nav shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavTree {
    pub description: String,
    pub groups: Vec<NavGroup>,
}

/// A titled group of pages within a [`NavTree`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavGroup {
    pub title: String,
    pub pages: Vec<NavPage>,
}

/// A single page entry within a [`NavGroup`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavPage {
    pub title: String,
    pub path: String,
    pub description: String,
}

/// Derivation lifecycle status for a corpus version, mirrored 1:1 with the
/// `corpus_versions.derive_status` TEXT column (lowercase strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeriveStatus {
    Pending,
    Running,
    Complete,
    Failed,
    /// This version's derive run bailed without rebuilding the Doc space
    /// because a newer publish for the same repo superseded it while it
    /// was waiting on the per-repo derive lock (review fix: derive must
    /// serialize per-repo, not per-version — see `run_derive`'s doc
    /// comment). Distinct from `Complete` (this version's own docs were
    /// never built) and from `Failed` (nothing errored; the newer
    /// version's derive run owns the Doc space instead).
    Superseded,
}

impl DeriveStatus {
    /// The lowercase wire/DB representation, e.g. `"pending"`.
    pub fn as_str(self) -> &'static str {
        match self {
            DeriveStatus::Pending => "pending",
            DeriveStatus::Running => "running",
            DeriveStatus::Complete => "complete",
            DeriveStatus::Failed => "failed",
            DeriveStatus::Superseded => "superseded",
        }
    }
}

impl std::str::FromStr for DeriveStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(DeriveStatus::Pending),
            "running" => Ok(DeriveStatus::Running),
            "complete" => Ok(DeriveStatus::Complete),
            "failed" => Ok(DeriveStatus::Failed),
            "superseded" => Ok(DeriveStatus::Superseded),
            other => Err(format!("unknown derive_status: {other}")),
        }
    }
}

/// Metadata for one row of `corpus_versions` — a single ingested corpus
/// version.
#[derive(Debug, Clone)]
pub struct CorpusVersionMeta {
    pub id: Uuid,
    pub repo_name: String,
    pub version: String,
    pub sha: String,
    pub is_latest: bool,
    pub is_tagged: bool,
    pub derive_status: DeriveStatus,
    pub ingested_at: chrono::DateTime<chrono::Utc>,
    pub page_count: Option<i32>,
    pub asset_count: Option<i32>,
    /// `manifest.index` — the index page's full corpus key (e.g. `index.md`
    /// or `docs/README.md` for a pull-bootstrapped corpus).
    pub index_path: String,
}

/// How a corpus version was ingested, mirrored 1:1 with the
/// `corpus_versions.source` TEXT column (lowercase strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusSource {
    /// CI packer pushed the corpus artifact directly (`POST /corpus`).
    Push,
    /// The platform pulled the corpus artifact from an upstream source.
    Pull,
}

impl CorpusSource {
    /// The lowercase wire/DB representation, e.g. `"push"`.
    pub fn as_str(self) -> &'static str {
        match self {
            CorpusSource::Push => "push",
            CorpusSource::Pull => "pull",
        }
    }
}

impl std::str::FromStr for CorpusSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "push" => Ok(CorpusSource::Push),
            "pull" => Ok(CorpusSource::Pull),
            other => Err(format!("unknown corpus source: {other}")),
        }
    }
}

/// Per-repo summary row returned by `CorpusStore::list_repos` — the latest
/// ingested version plus how many versions exist in total for the repo.
#[derive(Debug, Clone)]
pub struct CorpusRepoSummary {
    pub repo_name: String,
    pub version_count: i64,
    pub latest: CorpusVersionMeta,
}

/// A single file row returned by `CorpusStore::get_file`.
#[derive(Debug, Clone)]
pub struct CorpusFileRow {
    pub path: String,
    pub content: Vec<u8>,
    pub content_hash: String,
    pub size_bytes: i64,
    pub is_markdown: bool,
}

/// Finding severity for a contract-validation rule (see
/// `algos::corpus_contract`). `off` (config-time suppression) is not a
/// variant here — that's handled by callers filtering findings out, never a
/// value a pure algorithm produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warn,
}

/// A single contract-validation finding.
///
/// This serialized shape is load-bearing beyond this crate: it is the
/// element type of the CI packer's `--json` output and of the platform's
/// HTTP-422 validation-failure body, so field names/casing must not drift
/// without updating both consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractFinding {
    pub rule: String,
    pub file: String,
    pub line: Option<u32>,
    pub detail: String,
    pub severity: Severity,
}

/// Generate the canonical JSON Schema (2020-12 dialect) for [`CorpusManifest`].
///
/// This is the single source of truth for the manifest contract — both the
/// CI packer's own validation and the platform validator (A6) must agree
/// with this output, not with a hand-maintained copy.
pub fn manifest_json_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(CorpusManifest);
    serde_json::to_value(&schema).expect("schemars schema serializes to JSON")
}

/// `.akashic/docs.toml` — the pull-bootstrap docs contract (Task 9, B4),
/// detected in a repo checkout that has no CI docs-packer of its own. The
/// canonical shape lives here (not in `akashic-ingestion::corpus_pull`,
/// which only consumes it) so both the platform's own parser
/// (`corpus_pull::load_contract`) and the docs-kit kit's `akashic` plugin —
/// via the `get_docs_schema("docs-toml")` MCP tool (E5) — validate against
/// the SAME schema.
///
/// Deliberately NOT `deny_unknown_fields`, unlike [`CorpusManifest`]: this
/// contract has sections this task doesn't model at all (`[snippets]`,
/// `[check]`), and both this Rust deserializer and the kit packer's own ajv
/// schema must tolerate them rather than reject the file outright. No
/// `deny_unknown_fields` also means [`docs_toml_json_schema`] must NOT
/// declare `additionalProperties: false` — an artifact-schema author
/// tightening that later would silently break real `.akashic/docs.toml`
/// files that use those sections (see `corpus_pull`'s own
/// `docs_contract_tolerates_unconsumed_snippets_and_check_sections` test).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DocsToml {
    /// Path (repo-root-relative) to the C/C++ header carrying
    /// `#define X_VERSION_{MAJOR,MINOR,PATCH}` lines, parsed via
    /// `algos::corpus_contract::parse_version_header`.
    pub version_header: String,
    /// Repo-root-relative directory containing the docs tree to ingest.
    pub docs_root: String,
    /// `manifest.index` — repo-root-relative, e.g. `docs/README.md`.
    pub index: String,
    #[serde(default)]
    pub languages: Vec<String>,
    /// Optional platform slug override; defaults to the ingest request's
    /// own repo name when absent.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub corpus: DocsTomlCorpusSection,
}

/// The `[corpus]` table of a [`DocsToml`] contract.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct DocsTomlCorpusSection {
    /// Globs, relative to `docs_root` (not to the repo root), excluded from
    /// the collected file set. Empty = exclude nothing.
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Generate the canonical JSON Schema (2020-12 dialect) for [`DocsToml`]. See
/// [`manifest_json_schema`] for the sibling artifact-schema function; this
/// is the single source of truth for the pull-bootstrap contract.
pub fn docs_toml_json_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(DocsToml);
    serde_json::to_value(&schema).expect("schemars schema serializes to JSON")
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{CorpusSource, DeriveStatus};

    #[test]
    fn derive_status_as_str_from_str_round_trips_all_variants() {
        for status in [
            DeriveStatus::Pending,
            DeriveStatus::Running,
            DeriveStatus::Complete,
            DeriveStatus::Failed,
        ] {
            let s = status.as_str();
            let parsed = DeriveStatus::from_str(s).unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn derive_status_from_str_rejects_unknown_string() {
        assert!(DeriveStatus::from_str("bogus").is_err());
    }

    #[test]
    fn corpus_source_as_str_from_str_round_trips_all_variants() {
        for source in [CorpusSource::Push, CorpusSource::Pull] {
            let s = source.as_str();
            let parsed = CorpusSource::from_str(s).unwrap();
            assert_eq!(parsed, source);
        }
    }

    #[test]
    fn corpus_source_from_str_rejects_unknown_string() {
        assert!(CorpusSource::from_str("bogus").is_err());
    }
}
