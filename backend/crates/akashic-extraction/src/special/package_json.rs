//! npm `package.json` manifest [`SpecialExtractor`] (dependency-manifest track).
//!
//! Unlike the line-oriented infra extractors (Dockerfile / gitlab-ci /
//! Makefile), `package.json` is well-formed JSON, so it is parsed with the
//! crate's EXISTING `serde_json` dependency (no new Cargo dep). The goal is the
//! npm **package dependency graph**: which packages this manifest depends on.
//!
//! ## Chunk model
//! - ONE **manifest** chunk (`chunk_type = "manifest"`) spanning the whole file
//!   (line 1 .. last line, byte 0 .. `source.len()`). `name`/`fqn` = the
//!   top-level `"name"` field; if absent, the package directory name (the parent
//!   dir of the file), else the literal `"package"`. `parent_fqn = None`.
//!   `signature = "<name>@<version>"` (version omitted with no `@` if absent).
//!   `content` = the whole source. `visibility = Public`. The `version` and
//!   `private` JSON fields, when present, are recorded in `metadata.fields`.
//! - ONE **script** chunk per entry in the `"scripts"` object
//!   (`chunk_type = "script"`). `name` = the script key (e.g. `build`);
//!   `fqn = "<pkgname>.<scriptkey>"` (DOTTED, so scripts nest under the package
//!   in the FQN namespace); `parent_fqn = <pkgname>`. `content` = the script
//!   command string. The script key is LOCATED in the source (search for the
//!   `"key"` JSON key, like markdown.rs locates headings) for accurate
//!   `start_line`/`start_byte`; the span covers just the key token.
//!   `visibility = Public`.
//!
//! ## Edge model — the dependency graph (all [`Provenance::Static`])
//! For each entry in `"dependencies"`, `"devDependencies"`,
//! `"peerDependencies"`, and `"optionalDependencies"`, one
//! [`EdgeKind::Import`] edge is emitted. The edge `source` is the manifest
//! chunk's `Name`; the `target` is `Name { name: <pkg>, module_specifier:
//! Some("<pkg>@<versionRange>") }`. `metadata.fields["dep_type"]` records which
//! group it came from (`dependencies` / `devDependencies` /
//! `peerDependencies` / `optionalDependencies`). Version ranges are recorded
//! verbatim (in the module_specifier), NOT resolved. Non-dependency fields emit
//! no edges.
//!
//! ## Robustness
//! Malformed JSON does NOT error the pipeline: a parse failure yields an empty
//! [`ExtractionOutput`] (no chunk, no edge). Missing/oddly-typed fields are
//! tolerated (treated as absent).
//!
//! ## Matching / scope
//! `filename_matches` covers ONLY the exact name `package.json`
//! (`registry::lookup_for_path` lowercases the basename, and this is already
//! lowercase). `extensions` is EMPTY on purpose — claiming `.json` would hijack
//! every JSON file in a repo; generic `.json` stays the fallback extractor.

use crate::types::*;
use serde_json::Value;

pub static PACKAGE_JSON: PackageJsonExtractor = PackageJsonExtractor;

pub struct PackageJsonExtractor;

impl SpecialExtractor for PackageJsonExtractor {
    fn name(&self) -> &'static str {
        "package-json"
    }

    fn extensions(&self) -> &'static [&'static str] {
        // Intentionally empty: matching by the canonical filename only, so we do
        // NOT hijack arbitrary `.json` files (generic `.json` -> fallback).
        &[]
    }

    fn filename_matches(&self) -> &'static [&'static str] {
        // `registry::lookup_for_path` lowercases the basename before matching,
        // so this MUST be lowercase. This is the canonical npm manifest name.
        &["package.json"]
    }

    fn parse(&self, source: &str, rel_path: &str) -> Result<ExtractionOutput, ExtractionError> {
        Ok(parse_package_json(source, rel_path))
    }
}

/// The four npm dependency groups, in a stable order, paired with the
/// `dep_type` metadata value emitted for each.
const DEP_GROUPS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

fn parse_package_json(source: &str, rel_path: &str) -> ExtractionOutput {
    let mut out = ExtractionOutput::default();

    // Malformed JSON: degrade gracefully to an empty output (do not poison the
    // whole ingestion pipeline).
    let Ok(root) = serde_json::from_str::<Value>(source) else {
        return out;
    };
    // A non-object top level (e.g. a bare array or scalar) is not a manifest.
    let Some(obj) = root.as_object() else {
        return out;
    };

    // Package name: top-level "name", else the package directory name, else
    // the literal "package".
    let pkg_name = obj
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| package_dir_name(rel_path));

    let version = obj.get("version").and_then(Value::as_str);
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
    if let Some(private) = obj.get("private").and_then(Value::as_bool) {
        metadata
            .fields
            .insert("private".to_string(), MetadataValue::Bool(private));
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

    // ── Script chunks ───────────────────────────────────────────────
    if let Some(scripts) = obj.get("scripts").and_then(Value::as_object) {
        // Confine the per-key search to the `"scripts"` object's byte range so a
        // key that also occurs earlier in the file (e.g. a dependency named the
        // same, or that substring appearing inside an earlier value) cannot
        // mis-locate the script. `scripts_start` is the byte just after the
        // scripts object's opening `{`; if it can't be found we fall back to 0
        // (whole-file search), which is no worse than the previous behaviour.
        let scripts_start = scripts_object_start(source).unwrap_or(0);
        for (key, cmd) in scripts {
            let command = cmd.as_str().unwrap_or("").to_string();
            let (start_line, start_byte, end_byte) = locate_key(source, key, scripts_start);
            out.chunks.push(RawChunk {
                chunk_type: "script".into(),
                name: key.clone(),
                fqn: Some(format!("{pkg_name}.{key}")),
                parent_fqn: Some(pkg_name.clone()),
                start_line,
                end_line: start_line,
                start_byte,
                end_byte,
                signature: Some(key.clone()),
                content: command,
                doc: None,
                receiver: None,
                is_async: false,
                is_static: false,
                is_const: false,
                is_exported: false,
                visibility: Visibility::Public,
                metadata: ChunkMetadata::default(),
            });
        }
    }

    // ── Dependency Import edges (the dependency graph) ──────────────
    let manifest_source = EdgeEndpoint::Name {
        name: pkg_name.clone(),
        module_specifier: None,
    };
    for &group in DEP_GROUPS {
        let Some(deps) = obj.get(group).and_then(Value::as_object) else {
            continue;
        };
        for (dep, range) in deps {
            let range = range.as_str().unwrap_or("");
            let module_specifier = if range.is_empty() {
                dep.clone()
            } else {
                format!("{dep}@{range}")
            };
            let mut meta = EdgeMetadata::default();
            meta.fields.insert(
                "dep_type".to_string(),
                MetadataValue::String(group.to_string()),
            );
            out.edges.push(RawEdge {
                source: manifest_source.clone(),
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

    out
}

/// Locate a JSON object key (e.g. a script name) in the source, returning
/// `(start_line, start_byte, end_byte)` of the quoted key token (the opening
/// quote through the closing quote). The search begins at byte `from` (the
/// start of the enclosing object) rather than byte 0, so a key that also
/// appears earlier in the file — e.g. an identically named dependency, or that
/// substring occurring inside an earlier value — cannot mis-locate the match.
/// Falls back to `(1, 0, 0)` if not found.
fn locate_key(source: &str, key: &str, from: usize) -> (usize, usize, usize) {
    let needle = format!("\"{key}\"");
    // `find` returns an offset relative to the `&source[from..]` slice, so add
    // `from` back to recover an absolute byte offset into `source`. Slicing at
    // `from` is sound because `scripts_object_start` returns a char boundary
    // (it points just past an ASCII `{`).
    match source[from..].find(&needle) {
        Some(rel) => {
            let pos = from + rel;
            let start_line = source[..pos].matches('\n').count() + 1;
            (start_line, pos, pos + needle.len())
        }
        None => (1, 0, 0),
    }
}

/// Byte offset just past the opening `{` of the top-level `"scripts"` object,
/// used to bound the [`locate_key`] search to that object. Finds the
/// `"scripts"` key token, then the next `{` after it. Returns `None` if either
/// can't be found, in which case the caller searches the whole file.
///
/// This is a lightweight textual scan (the file is already known to be valid
/// JSON via the earlier `serde_json` parse); it does not attempt to skip a
/// stray `{` that might appear between the key and the object inside a string
/// value, which cannot occur for a well-formed `"scripts": { ... }` member.
fn scripts_object_start(source: &str) -> Option<usize> {
    let key_pos = source.find("\"scripts\"")?;
    // The object's opening brace is the first `{` after the key token.
    let brace_rel = source[key_pos..].find('{')?;
    Some(key_pos + brace_rel + 1)
}

/// Derive a package name from the manifest's directory when the `"name"` field
/// is absent: the parent directory's basename, else the literal `"package"`.
fn package_dir_name(rel_path: &str) -> String {
    let parent = rel_path.rsplit_once('/').map_or("", |(dir, _file)| dir);
    let dir_base = parent.rsplit('/').next().unwrap_or("");
    if dir_base.is_empty() {
        "package".to_string()
    } else {
        dir_base.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_json_degrades_to_empty() {
        // Trailing comma + truncation: invalid JSON must NOT error the pipeline.
        let out = parse_package_json("{ \"name\": \"x\", ", "app/package.json");
        assert!(out.chunks.is_empty());
        assert!(out.edges.is_empty());
    }

    #[test]
    fn non_object_top_level_is_empty() {
        let out = parse_package_json("[1, 2, 3]", "package.json");
        assert!(out.chunks.is_empty());
        assert!(out.edges.is_empty());
    }

    #[test]
    fn name_falls_back_to_package_dir() {
        // No "name" field -> use the parent directory basename.
        let out = parse_package_json("{ \"version\": \"1.0.0\" }", "libs/widgets/package.json");
        assert_eq!(out.chunks.len(), 1);
        assert_eq!(out.chunks[0].name, "widgets");
        assert_eq!(out.chunks[0].signature.as_deref(), Some("widgets@1.0.0"));
    }

    #[test]
    fn script_key_colliding_with_earlier_dependency_locates_in_scripts_object() {
        // Regression: a script key ("build") that ALSO appears earlier as a
        // dependency name must be located inside the "scripts" object, not at
        // the first textual occurrence (the dependency, which would yield the
        // wrong byte offset).
        let src = "{\n  \"dependencies\": {\n    \"build\": \"^1.0.0\"\n  },\n  \"scripts\": {\n    \"build\": \"tsc\"\n  }\n}\n";
        let out = parse_package_json(src, "app/package.json");
        let script = out
            .chunks
            .iter()
            .find(|c| c.chunk_type == "script" && c.name == "build")
            .expect("script chunk for build");
        // The located key must be the one INSIDE the scripts object: its byte
        // offset must fall after the "scripts" key token, not at the earlier
        // dependency occurrence.
        let scripts_kw = src.find("\"scripts\"").unwrap();
        assert!(
            script.start_byte > scripts_kw,
            "script key located at {} which is before the \"scripts\" object at {}",
            script.start_byte,
            scripts_kw
        );
        // And the located token is genuinely the script key: the source slice
        // at that span is the quoted key.
        assert_eq!(&src[script.start_byte..script.end_byte], "\"build\"");
        // Sanity: it is on the line of the scripts-object "build", not the
        // dependency "build".
        assert_eq!(script.start_line, 6);
    }
}
