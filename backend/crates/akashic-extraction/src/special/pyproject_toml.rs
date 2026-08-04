//! Python `pyproject.toml` manifest [`SpecialExtractor`] (dependency-manifest
//! track).
//!
//! `pyproject.toml` is well-formed TOML, parsed with the crate's `toml`
//! dependency. The goal is the Python **package dependency graph**: which
//! distributions this project depends on. This is the Python analog of the npm
//! `package.json` / Rust `Cargo.toml` extractors, but Python has THREE common
//! dependency-declaration schemes that this extractor detects and handles.
//!
//! ## Chunk model
//! - ONE **manifest** chunk (`chunk_type = "manifest"`) spanning the whole file
//!   (line 1 .. last line, byte 0 .. `source.len()`). `name` = `[project].name`
//!   (PEP 621) else `[tool.poetry].name` (Poetry); if absent, the manifest
//!   directory name, else the literal `"pyproject"`. `parent_fqn = None`.
//!   `signature = "<name>@<version>"` (version from `[project].version` or
//!   `[tool.poetry].version`, omitted if absent). `content` = the whole source.
//!   `visibility = Public`.
//!
//! ## Edge model — the dependency graph (all [`Provenance::Static`])
//! Each dependency becomes one [`EdgeKind::Import`] edge whose `source` is the
//! manifest `Name` and whose `target` is `Name { name: <distribution>,
//! module_specifier: Some(<requirement>) }`. `metadata.fields["dep_type"]`
//! records the source group. Three schemes are handled:
//!
//! - **PEP 621** — `[project].dependencies` is an ARRAY of PEP 508 requirement
//!   strings (`"requests>=2.0"`, `"flask"`, `"foo[extra]>=1; python<'3.9'"`).
//!   The distribution name is the leading identifier parsed up to the first
//!   version operator / whitespace / `[` (extras) / `;` (marker) / `@` (URL).
//!   `module_specifier` is the FULL requirement string. `dep_type =
//!   "dependencies"`. `[project.optional-dependencies]` is a table of
//!   group → array; each group's deps use `dep_type = <group-name>`.
//! - **Poetry** — `[tool.poetry.dependencies]` is a TABLE (`name = "^1.2"` or
//!   `name = { version = "^1.2", ... }`), like Cargo. `dep_type = "poetry"`.
//!   The implicit `python` constraint entry is SKIPPED (it is the interpreter
//!   requirement, not a distribution dependency). Table values without a
//!   `version` fall back to a `git:`/`path:`/`url:` source specifier.
//! - **build-system** — `[build-system].requires` is an ARRAY of PEP 508
//!   strings (the build backend deps); emitted with `dep_type =
//!   "build-system"`.
//!
//! PEP 735 `[dependency-groups]` is intentionally NOT handled (documented
//! out-of-scope; rare relative to the three schemes above).
//!
//! ## Robustness
//! Malformed TOML does NOT error the pipeline: a parse failure yields an empty
//! [`ExtractionOutput`].
//!
//! ## Matching / scope
//! `filename_matches` covers ONLY the exact name `pyproject.toml`
//! (case-folded by `registry::lookup_for_path`). `extensions` is EMPTY — generic
//! `.toml` stays the fallback extractor.

use crate::types::*;
use toml::Value;

pub static PYPROJECT_TOML: PyprojectTomlExtractor = PyprojectTomlExtractor;

pub struct PyprojectTomlExtractor;

impl SpecialExtractor for PyprojectTomlExtractor {
    fn name(&self) -> &'static str {
        "pyproject-toml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        // Intentionally empty: matching by the canonical filename only, so we do
        // NOT hijack arbitrary `.toml` files (generic `.toml` -> fallback).
        &[]
    }

    fn filename_matches(&self) -> &'static [&'static str] {
        // `registry::lookup_for_path` lowercases the basename before matching,
        // so this MUST be lowercase. This is the canonical Python manifest name.
        &["pyproject.toml"]
    }

    fn parse(&self, source: &str, rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        Ok(parse_pyproject_toml(source, rel_path))
    }
}

fn parse_pyproject_toml(source: &str, rel_path: &str) -> ExtractionOutput {
    let mut out = ExtractionOutput::default();

    // Malformed TOML: degrade gracefully to an empty output.
    let Ok(root) = toml::from_str::<Value>(source) else {
        return out;
    };
    let Some(table) = root.as_table() else {
        return out;
    };

    let project = table.get("project").and_then(Value::as_table);
    let poetry = table
        .get("tool")
        .and_then(Value::as_table)
        .and_then(|t| t.get("poetry"))
        .and_then(Value::as_table);

    // Project name: [project].name (PEP 621), else [tool.poetry].name, else dir.
    let pkg_name = project
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .or_else(|| poetry.and_then(|p| p.get("name")).and_then(Value::as_str))
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| manifest_dir_name(rel_path));

    let version = project
        .and_then(|p| p.get("version"))
        .and_then(Value::as_str)
        .or_else(|| {
            poetry
                .and_then(|p| p.get("version"))
                .and_then(Value::as_str)
        });
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

    let manifest_source = EdgeEndpoint::Name {
        name: pkg_name.clone(),
        module_specifier: None,
    };

    // ── PEP 621: [project].dependencies (array of PEP 508 strings) ──
    if let Some(deps) = project
        .and_then(|p| p.get("dependencies"))
        .and_then(Value::as_array)
    {
        emit_pep508_edges(deps, "dependencies", &manifest_source, &mut out);
    }

    // ── PEP 621: [project.optional-dependencies] (table of group->array) ──
    if let Some(groups) = project
        .and_then(|p| p.get("optional-dependencies"))
        .and_then(Value::as_table)
    {
        for (group, arr) in groups {
            if let Some(deps) = arr.as_array() {
                emit_pep508_edges(deps, group, &manifest_source, &mut out);
            }
        }
    }

    // ── Poetry: [tool.poetry.dependencies] (table, like Cargo) ──
    if let Some(deps) = poetry
        .and_then(|p| p.get("dependencies"))
        .and_then(Value::as_table)
    {
        for (dep, spec) in deps {
            // The implicit `python` interpreter constraint is not a dependency.
            if dep == "python" {
                continue;
            }
            let req = poetry_requirement(spec);
            let module_specifier = match req {
                Some(r) if !r.is_empty() => format!("{dep}@{r}"),
                _ => dep.clone(),
            };
            push_import(&manifest_source, dep, module_specifier, "poetry", &mut out);
        }
    }

    // ── build-system.requires (array of PEP 508 strings) ──
    if let Some(reqs) = table
        .get("build-system")
        .and_then(Value::as_table)
        .and_then(|b| b.get("requires"))
        .and_then(Value::as_array)
    {
        emit_pep508_edges(reqs, "build-system", &manifest_source, &mut out);
    }

    out
}

/// Emit one Import edge per PEP 508 requirement string in `deps`.
fn emit_pep508_edges(
    deps: &[Value],
    dep_type: &str,
    source: &EdgeEndpoint,
    out: &mut ExtractionOutput,
) {
    for item in deps {
        let Some(req) = item.as_str() else { continue };
        let req = req.trim();
        if req.is_empty() {
            continue;
        }
        let name = pep508_name(req);
        if name.is_empty() {
            continue;
        }
        push_import(source, &name, req.to_string(), dep_type, out);
    }
}

/// Push a single dependency Import edge.
fn push_import(
    source: &EdgeEndpoint,
    name: &str,
    module_specifier: String,
    dep_type: &str,
    out: &mut ExtractionOutput,
) {
    let mut meta = EdgeMetadata::default();
    meta.fields.insert(
        "dep_type".to_string(),
        MetadataValue::String(dep_type.to_string()),
    );
    out.edges.push(RawEdge {
        source: source.clone(),
        target: EdgeEndpoint::Name {
            name: name.to_string(),
            module_specifier: Some(module_specifier),
        },
        kind: EdgeKind::Import,
        provenance: Provenance::Static,
        line: None,
        metadata: meta,
    });
}

/// Extract the distribution name from a PEP 508 requirement string: the leading
/// identifier up to the first version operator (`<>=!~`), whitespace, `[`
/// (extras), `;` (environment marker), `(` (parenthesised version), or `@`
/// (direct URL reference). The name itself may contain `.`, `-`, `_`.
fn pep508_name(req: &str) -> String {
    let end = req
        .find(|c: char| {
            c.is_whitespace()
                || matches!(c, '<' | '>' | '=' | '!' | '~' | '[' | ';' | '(' | '@' | ',')
        })
        .unwrap_or(req.len());
    req[..end].trim().to_string()
}

/// Derive the requirement string for a Poetry dependency value, handling the
/// string form (`"^1.2"`) and the table form. For the table form: prefer
/// `version`, else describe the source as `git:`/`path:`/`url:`; if none of
/// those is present, return `None` (specifier becomes the bare name).
fn poetry_requirement(spec: &Value) -> Option<String> {
    match spec {
        Value::String(s) => Some(s.clone()),
        Value::Table(t) => {
            if let Some(v) = t.get("version").and_then(Value::as_str) {
                Some(v.to_string())
            } else if let Some(g) = t.get("git").and_then(Value::as_str) {
                Some(format!("git:{g}"))
            } else if let Some(p) = t.get("path").and_then(Value::as_str) {
                Some(format!("path:{p}"))
            } else {
                t.get("url")
                    .and_then(Value::as_str)
                    .map(|u| format!("url:{u}"))
            }
        }
        // An array form (multiple constraints) is uncommon; treat as unspecified.
        _ => None,
    }
}

/// Derive a project name from the manifest's directory when no `name` field is
/// present: the parent directory's basename, else the literal `"pyproject"`.
fn manifest_dir_name(rel_path: &str) -> String {
    let parent = rel_path.rsplit_once('/').map_or("", |(dir, _file)| dir);
    let dir_base = parent.rsplit('/').next().unwrap_or("");
    if dir_base.is_empty() {
        "pyproject".to_string()
    } else {
        dir_base.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_toml_degrades_to_empty() {
        let out = parse_pyproject_toml("[project]\nname = \"x", "app/pyproject.toml");
        assert!(out.chunks.is_empty());
        assert!(out.edges.is_empty());
    }

    #[test]
    fn pep508_name_parsing() {
        assert_eq!(pep508_name("requests>=2.0"), "requests");
        assert_eq!(pep508_name("flask"), "flask");
        assert_eq!(pep508_name("foo[extra]>=1; python_version<'3.9'"), "foo");
        assert_eq!(pep508_name("django ~= 4.2"), "django");
        assert_eq!(pep508_name("pkg @ https://example.com/pkg.whl"), "pkg");
        assert_eq!(pep508_name("numpy==1.26.0"), "numpy");
    }

    #[test]
    fn name_falls_back_to_manifest_dir() {
        let out = parse_pyproject_toml(
            "[build-system]\nrequires = []\n",
            "libs/widgets/pyproject.toml",
        );
        assert_eq!(out.chunks.len(), 1);
        assert_eq!(out.chunks[0].name, "widgets");
    }

    #[test]
    fn poetry_skips_python_and_handles_both_forms() {
        let src = "[tool.poetry]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
                   [tool.poetry.dependencies]\npython = \"^3.11\"\n\
                   requests = \"^2.31\"\nrich = { version = \"13.7\" }\n";
        let out = parse_pyproject_toml(src, "pyproject.toml");
        let targets: Vec<_> = out
            .edges
            .iter()
            .filter_map(|e| match &e.target {
                EdgeEndpoint::Name { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert!(!targets.contains(&"python".to_string()));
        assert!(targets.contains(&"requests".to_string()));
        assert!(targets.contains(&"rich".to_string()));
    }
}
