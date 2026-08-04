//! Rust `Cargo.toml` manifest [`SpecialExtractor`] (dependency-manifest track).
//!
//! `Cargo.toml` is well-formed TOML, parsed with the crate's `toml`
//! dependency. The goal is the Rust **crate dependency graph**: which crates
//! this manifest depends on. This is the Rust analog of the npm
//! `package.json` extractor.
//!
//! ## Chunk model
//! - ONE **manifest** chunk (`chunk_type = "manifest"`) spanning the whole file
//!   (line 1 .. last line, byte 0 .. `source.len()`). `name`/`fqn` = the
//!   `[package].name` field; if absent (e.g. a workspace-root manifest with
//!   only `[workspace]`), the manifest directory name, else the literal
//!   `"workspace"` when a `[workspace]` table is present, else `"cargo"`.
//!   `parent_fqn = None`. `signature = "<name>@<version>"` (version from
//!   `[package].version`, omitted with no `@` if absent). `content` = the whole
//!   source. `visibility = Public`. `[package].edition` is recorded in
//!   `metadata.fields["edition"]` when present, and a workspace-root manifest is
//!   tagged `metadata.fields["workspace"]="true"`.
//!
//! ## Edge model — the dependency graph (all [`Provenance::Static`])
//! For each entry in `[dependencies]`, `[dev-dependencies]`,
//! `[build-dependencies]`, AND any `[target.*.dependencies]` /
//! `[target.*.dev-dependencies]` / `[target.*.build-dependencies]` table, one
//! [`EdgeKind::Import`] edge is emitted. The edge `source` is the manifest
//! chunk's `Name`; the `target` is `Name { name: <crate>, module_specifier:
//! Some(<spec>) }` where `<spec>` is derived from BOTH dependency forms:
//! - String form (`serde = "1.0"`) → `crate@1.0`.
//! - Table form (`serde = { version = "1", features = [...] }`) → `crate@1`
//!   (the `version` key). If the table has no `version`, the requirement is a
//!   `git` or `path` source: `crate@git:<url>` or `crate@path:<p>`. With none of
//!   those, the specifier is just `crate`.
//!
//! `metadata.fields["dep_type"]` records the group (`dependencies` /
//! `dev-dependencies` / `build-dependencies`). For target-specific tables the
//! target triple/cfg is recorded in `metadata.fields["target"]`; the `dep_type`
//! is the inner group name. Version requirements are recorded verbatim, NOT
//! resolved. `[features]`, `[lib]`, `[bin]`, `[workspace]`, etc. emit no edges.
//!
//! ## Robustness
//! Malformed TOML does NOT error the pipeline: a parse failure yields an empty
//! [`ExtractionOutput`] (no chunk, no edge). Missing/oddly-typed fields are
//! tolerated (treated as absent).
//!
//! ## Matching / scope
//! `filename_matches` covers ONLY the exact name `cargo.toml`
//! (`registry::lookup_for_path` lowercases the basename, and this is already
//! lowercase). `extensions` is EMPTY on purpose — claiming `.toml` would hijack
//! every TOML file in a repo; generic `.toml` stays the fallback extractor.

use crate::types::*;
use toml::Value;

pub static CARGO_TOML: CargoTomlExtractor = CargoTomlExtractor;

pub struct CargoTomlExtractor;

impl SpecialExtractor for CargoTomlExtractor {
    fn name(&self) -> &'static str {
        "cargo-toml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        // Intentionally empty: matching by the canonical filename only, so we do
        // NOT hijack arbitrary `.toml` files (generic `.toml` -> fallback).
        &[]
    }

    fn filename_matches(&self) -> &'static [&'static str] {
        // `registry::lookup_for_path` lowercases the basename before matching,
        // so this MUST be lowercase. This is the canonical Cargo manifest name.
        &["cargo.toml"]
    }

    fn parse(&self, source: &str, rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        Ok(parse_cargo_toml(source, rel_path))
    }
}

/// The three non-target dependency tables, in stable order, paired with the
/// `dep_type` metadata value emitted for each.
const DEP_GROUPS: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

fn parse_cargo_toml(source: &str, rel_path: &str) -> ExtractionOutput {
    let mut out = ExtractionOutput::default();

    // Malformed TOML: degrade gracefully to an empty output (do not poison the
    // whole ingestion pipeline).
    let Ok(root) = toml::from_str::<Value>(source) else {
        return out;
    };
    let Some(table) = root.as_table() else {
        return out;
    };

    let package = table.get("package").and_then(Value::as_table);
    let is_workspace = table.get("workspace").is_some();

    // Crate name: `[package].name`, else the manifest directory name, else
    // "workspace" when a `[workspace]` table is present, else "cargo".
    let pkg_name = package
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| manifest_dir_name(rel_path, is_workspace));

    let version = package
        .and_then(|p| p.get("version"))
        .and_then(Value::as_str);
    let signature = match version {
        Some(v) if !v.is_empty() => format!("{pkg_name}@{v}"),
        _ => pkg_name.clone(),
    };

    let total_lines = source.matches('\n').count() + 1;

    // ── Manifest chunk ──────────────────────────────────────────────
    let mut metadata = ChunkMetadata::default();
    if let Some(v) = version {
        metadata
            .fields
            .insert("version".to_string(), MetadataValue::String(v.to_string()));
    }
    if let Some(edition) = package
        .and_then(|p| p.get("edition"))
        .and_then(Value::as_str)
    {
        metadata.fields.insert(
            "edition".to_string(),
            MetadataValue::String(edition.to_string()),
        );
    }
    if is_workspace {
        metadata.fields.insert(
            "workspace".to_string(),
            MetadataValue::String("true".to_string()),
        );
    }

    out.chunks.push(RawChunk {
        chunk_type: "manifest".into(),
        name: pkg_name.clone(),
        fqn: Some(pkg_name.clone()),
        parent_fqn: None,
        start_line: 1,
        end_line: total_lines,
        start_byte: 0,
        end_byte: source.len(),
        signature: Some(signature),
        content: source.to_string(),
        doc: None,
        receiver: None,
        is_async: false,
        is_static: false,
        is_const: false,
        is_exported: false,
        visibility: Visibility::Public,
        metadata,
    });

    // ── Dependency Import edges (the dependency graph) ──────────────
    let manifest_source = EdgeEndpoint::Name {
        name: pkg_name.clone(),
        module_specifier: None,
    };

    // Top-level dependency tables.
    for &group in DEP_GROUPS {
        if let Some(deps) = table.get(group).and_then(Value::as_table) {
            emit_dep_edges(deps, group, None, &manifest_source, &mut out);
        }
    }

    // Target-specific dependency tables: `[target.<triple>.dependencies]` etc.
    // are nested under `[target]` as `target.<key>.<group>`.
    if let Some(targets) = table.get("target").and_then(Value::as_table) {
        for (triple, cfg) in targets {
            let Some(cfg_table) = cfg.as_table() else {
                continue;
            };
            for &group in DEP_GROUPS {
                if let Some(deps) = cfg_table.get(group).and_then(Value::as_table) {
                    emit_dep_edges(deps, group, Some(triple), &manifest_source, &mut out);
                }
            }
        }
    }

    out
}

/// Emit one Import edge per entry in a dependency table.
fn emit_dep_edges(
    deps: &toml::Table,
    group: &str,
    target: Option<&str>,
    source: &EdgeEndpoint,
    out: &mut ExtractionOutput,
) {
    for (dep, spec) in deps {
        let req = dep_requirement(spec);
        let module_specifier = match req {
            Some(r) if !r.is_empty() => format!("{dep}@{r}"),
            _ => dep.clone(),
        };
        let mut meta = EdgeMetadata::default();
        meta.fields.insert(
            "dep_type".to_string(),
            MetadataValue::String(group.to_string()),
        );
        if let Some(t) = target {
            meta.fields
                .insert("target".to_string(), MetadataValue::String(t.to_string()));
        }
        out.edges.push(RawEdge {
            source: source.clone(),
            target: EdgeEndpoint::Name {
                name: dep.clone(),
                module_specifier: Some(module_specifier),
            },
            kind: EdgeKind::Import,
            provenance: Provenance::Static,
            line: None,
            metadata: meta,
        });
    }
}

/// Derive the version requirement string for a dependency value, handling both
/// the string form (`"1.0"`) and the table form. For the table form: prefer
/// `version`, else describe the source as `git:<url>` / `path:<p>`; if none of
/// those is present, return `None` (specifier becomes the bare crate name).
fn dep_requirement(spec: &Value) -> Option<String> {
    match spec {
        Value::String(s) => Some(s.clone()),
        Value::Table(t) => {
            if let Some(v) = t.get("version").and_then(Value::as_str) {
                Some(v.to_string())
            } else if let Some(g) = t.get("git").and_then(Value::as_str) {
                Some(format!("git:{g}"))
            } else {
                t.get("path")
                    .and_then(Value::as_str)
                    .map(|p| format!("path:{p}"))
            }
        }
        _ => None,
    }
}

/// Derive a crate name from the manifest's directory when `[package].name` is
/// absent: the parent directory's basename. Falls back to `"workspace"` (when a
/// `[workspace]` table is present) or `"cargo"`.
fn manifest_dir_name(rel_path: &str, is_workspace: bool) -> String {
    let parent = rel_path.rsplit_once('/').map_or("", |(dir, _file)| dir);
    let dir_base = parent.rsplit('/').next().unwrap_or("");
    if !dir_base.is_empty() {
        dir_base.to_string()
    } else if is_workspace {
        "workspace".to_string()
    } else {
        "cargo".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_toml_degrades_to_empty() {
        // Unterminated string / bad syntax: invalid TOML must NOT error.
        let out = parse_cargo_toml("[package]\nname = \"x", "app/Cargo.toml");
        assert!(out.chunks.is_empty());
        assert!(out.edges.is_empty());
    }

    #[test]
    fn name_falls_back_to_manifest_dir() {
        // No [package] -> use the parent directory basename.
        let out = parse_cargo_toml("[workspace]\nmembers = []\n", "crates/widgets/Cargo.toml");
        assert_eq!(out.chunks.len(), 1);
        assert_eq!(out.chunks[0].name, "widgets");
    }

    #[test]
    fn version_extracted_from_string_and_table_forms() {
        let src = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
                   [dependencies]\nserde = \"1.0\"\ntokio = { version = \"1\", features = [\"full\"] }\n";
        let out = parse_cargo_toml(src, "Cargo.toml");
        let specs: Vec<_> = out
            .edges
            .iter()
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name {
                    module_specifier, ..
                } => module_specifier.clone(),
                _ => None,
            })
            .collect();
        assert!(specs.contains(&"serde@1.0".to_string()));
        assert!(specs.contains(&"tokio@1".to_string()));
    }

    #[test]
    fn git_and_path_specifiers() {
        let src = "[package]\nname = \"app\"\n\n[dependencies]\n\
                   a = { git = \"https://example.com/a.git\" }\n\
                   b = { path = \"../b\" }\n";
        let out = parse_cargo_toml(src, "Cargo.toml");
        let specs: Vec<_> = out
            .edges
            .iter()
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name {
                    module_specifier, ..
                } => module_specifier.clone(),
                _ => None,
            })
            .collect();
        assert!(specs.contains(&"a@git:https://example.com/a.git".to_string()));
        assert!(specs.contains(&"b@path:../b".to_string()));
    }
}
