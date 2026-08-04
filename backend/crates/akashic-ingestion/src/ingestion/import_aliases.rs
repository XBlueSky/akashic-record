//! Import alias resolution for TypeScript (`tsconfig.json` path aliases),
//! Go (`go.mod` module prefix), and Cargo workspace crates.
//!
//! `normalize` rewrites a raw import specifier to a repo-relative path when
//! one of the alias rules matches. `discover` reads project config files from
//! the repo root to populate the alias map.
//!
//! The rewrites run at the TOP of `resolve_import_target`, before any of the
//! existing file/dir/module matching logic. If no alias matches, `None` is
//! returned and the caller continues with the raw specifier unchanged.

use std::collections::HashMap;
use std::path::Path;

use tracing::warn;

/// Alias map built from project configuration files.
#[derive(Debug, Default, Clone)]
pub struct ImportAliases {
    /// TypeScript/JavaScript path aliases: `(prefix, replacement_dir)`.
    ///
    /// The trailing `*` has already been stripped from both sides.
    /// Example: `("@/", "src/")` maps `@/components/X` → `src/components/X`.
    pub ts_paths: Vec<(String, String)>,

    /// Go module path from `go.mod` (e.g. `"github.com/org/repo"`).
    ///
    /// Imports beginning with `<module>/` are rewritten to the local path
    /// by stripping this prefix.
    pub go_module: Option<String>,

    /// Cargo workspace crate name → member `src/` directory.
    ///
    /// Example: `{"my_lib" → "crates/my_lib/src"}`.
    /// Used to rewrite `my_lib::util::helper` → `crates/my_lib/src/util/helper`.
    pub cargo_crates: HashMap<String, String>,
}

/// Rewrite a raw import specifier to a repo-relative path using the alias map.
///
/// Returns `Some(path)` when an alias matches; `None` when none applies (the
/// existing resolver handles the rest).  Order: TypeScript → Go → Cargo; the
/// first match wins.
///
/// ## Non-matching cases (always return `None`)
/// - Same-crate Rust paths: `crate::`, `self::`, `super::`
/// - External third-party names: `react`, `std::x`
/// - Relative paths: `./util`, `../shared`
pub fn normalize(raw_target: &str, aliases: &ImportAliases) -> Option<String> {
    // ── TypeScript path aliases ──────────────────────────────────────────────
    // Pick the LONGEST matching prefix (most specific): an earlier, generic
    // alias like "@/" must not shadow a more specific "@/components/" that
    // appears later in ts_paths.
    let mut best: Option<(&String, &String)> = None;
    for (prefix, repl) in &aliases.ts_paths {
        if raw_target.starts_with(prefix.as_str())
            && best.is_none_or(|(bp, _)| prefix.len() > bp.len())
        {
            best = Some((prefix, repl));
        }
    }
    if let Some((prefix, repl)) = best {
        let tail = &raw_target[prefix.len()..];
        return Some(format!("{repl}{tail}"));
    }

    // ── Go module prefix ─────────────────────────────────────────────────────
    // A non-empty module guards against a garbage `Some("")` (which would make
    // the `<module>/` prefix the bare `"/"` and match every absolute-looking
    // specifier). `discover_go_mod` never stores an empty module, but
    // `normalize` is public and callers build `ImportAliases` directly.
    if let Some(ref go_mod) = aliases.go_module
        && !go_mod.is_empty()
    {
        // Bare module path with no trailing segment (`raw == module`) is the
        // package at the repo ROOT: rewrite to the empty repo-relative path
        // so the downstream resolver matches the root module (keyed "") if
        // it exists, instead of falling through and emitting no edge.
        if raw_target == go_mod {
            return Some(String::new());
        }
        // Sub-package import: strip the exact `<module>/` prefix.
        let prefix = format!("{go_mod}/");
        if raw_target.starts_with(prefix.as_str()) {
            return Some(raw_target[prefix.len()..].to_string());
        }
    }

    // ── Cargo workspace crate ────────────────────────────────────────────────
    // `crate::`, `self::`, `super::` are handled by EXT-3c, not here.
    if raw_target.contains("::")
        && !raw_target.starts_with("crate::")
        && !raw_target.starts_with("self::")
        && !raw_target.starts_with("super::")
        && !raw_target.contains("::super::")
    {
        let (head, rest) = raw_target.split_once("::")?;

        // Try the crate name as-is, then with `-` ↔ `_` normalisation.
        let member_src = aliases
            .cargo_crates
            .get(head)
            .or_else(|| {
                // Normalise: replace `-` with `_` in the head and try again.
                let normalised = head.replace('-', "_");
                aliases.cargo_crates.get(&normalised)
            })
            .or_else(|| {
                // Normalise: replace `_` with `-` in the head and try again.
                let normalised = head.replace('_', "-");
                aliases.cargo_crates.get(&normalised)
            });

        if let Some(src_dir) = member_src {
            // Convert the rest of the path from `::` notation to `/`.
            let local_path = rest.replace("::", "/");
            return Some(format!("{src_dir}/{local_path}"));
        }
    }

    None
}

// ─── Config discovery ────────────────────────────────────────────────────────

/// Read project configuration files from `repo_root` and build the alias map.
///
/// Missing or unparseable files are silently skipped (their ecosystem simply
/// contributes nothing to the map).  I/O errors are logged at WARN level and
/// never propagate.
pub fn discover(repo_root: &Path) -> ImportAliases {
    let mut aliases = ImportAliases::default();

    discover_tsconfig(repo_root, &mut aliases);
    discover_go_mod(repo_root, &mut aliases);
    discover_cargo_workspace(repo_root, &mut aliases);

    aliases
}

// ── TypeScript / JavaScript ───────────────────────────────────────────────────

fn discover_tsconfig(repo_root: &Path, aliases: &mut ImportAliases) {
    // Try `tsconfig.json` first, then `jsconfig.json`.
    for filename in &["tsconfig.json", "jsconfig.json"] {
        let path = repo_root.join(filename);
        if !path.exists() {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                warn!(?path, err = %e, "Could not read {filename}");
                continue;
            }
        };

        let stripped = strip_json_comments(&raw);
        let json: serde_json::Value = match serde_json::from_str(&stripped) {
            Ok(v) => v,
            Err(e) => {
                warn!(?path, err = %e, "Could not parse {filename} (after comment stripping)");
                continue;
            }
        };

        let opts = match json.get("compilerOptions") {
            Some(v) => v,
            None => continue,
        };

        // baseUrl defaults to ".".
        let base_url = opts
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_string();

        let paths_map = match opts.get("paths").and_then(|v| v.as_object()) {
            Some(m) => m,
            None => continue,
        };

        for (pattern, targets) in paths_map {
            // We only handle wildcard patterns: `"@/*"` / `"src/*"` etc.
            let prefix = pattern.trim_end_matches('*').to_string();

            // Take the first target only.
            let first_target = match targets.as_array().and_then(|arr| arr.first()) {
                Some(v) => v,
                None => continue,
            };
            let target_str = match first_target.as_str() {
                Some(s) => s,
                None => continue,
            };

            // Strip the trailing `*` (wildcard), then strip a leading `./`
            // from the target so it can be cleanly joined to baseUrl.
            let target_tail = target_str.trim_end_matches('*').trim_start_matches("./");

            // Fold baseUrl into the replacement for ALL targets (both `./`-style
            // and bare ones). A `"."`/empty baseUrl contributes nothing and must
            // not produce a leading `./`. `../` targets are left as-is.
            let repl = if target_str.trim_end_matches('*').starts_with("../") {
                // Parent-relative target; baseUrl folding would be ambiguous.
                target_str.trim_end_matches('*').to_string()
            } else if base_url.is_empty() || base_url == "." {
                target_tail.to_string()
            } else {
                format!("{base_url}/{target_tail}")
            };

            aliases.ts_paths.push((prefix, repl));
        }
    }
}

/// Strip `//`-style line comments and trailing commas from a JSON string so
/// that JSONC / tsconfig files parse with the standard `serde_json` parser.
///
/// This is a best-effort pass; it does not handle `/* … */` block comments or
/// escaped strings containing `//`, but covers the vast majority of real-world
/// tsconfig files.
fn strip_json_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_string = false;
    let mut escape_next = false;
    let mut chars = src.chars().peekable();

    while let Some(ch) = chars.next() {
        if escape_next {
            out.push(ch);
            escape_next = false;
            continue;
        }
        if in_string {
            if ch == '\\' {
                escape_next = true;
            } else if ch == '"' {
                in_string = false;
            }
            out.push(ch);
            continue;
        }
        // Outside a string literal.
        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            // Line comment — consume until newline.
            for c in chars.by_ref() {
                if c == '\n' {
                    out.push('\n');
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }

    // Remove trailing commas before `}` / `]` (another common JSONC quirk).
    remove_trailing_commas(&out)
}

fn remove_trailing_commas(src: &str) -> String {
    // Simple regex-free approach: scan for `,` followed only by whitespace
    // and then a `}` or `]`.
    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b',' {
            // Look ahead past whitespace.
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r')
            {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                // Skip the comma.
                i += 1;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ── Go module ─────────────────────────────────────────────────────────────────

fn discover_go_mod(repo_root: &Path, aliases: &mut ImportAliases) {
    let path = repo_root.join("go.mod");
    if !path.exists() {
        return;
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            warn!(?path, err = %e, "Could not read go.mod");
            return;
        }
    };
    for line in raw.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("module ") {
            let module = rest.split_whitespace().next().unwrap_or("").to_string();
            if !module.is_empty() {
                aliases.go_module = Some(module);
            }
            return; // `module` directive appears once, near the top.
        }
    }
}

// ── Cargo workspace ───────────────────────────────────────────────────────────

fn discover_cargo_workspace(repo_root: &Path, aliases: &mut ImportAliases) {
    let root_toml_path = repo_root.join("Cargo.toml");
    if !root_toml_path.exists() {
        return;
    }
    let raw = match std::fs::read_to_string(&root_toml_path) {
        Ok(s) => s,
        Err(e) => {
            warn!(path = ?root_toml_path, err = %e, "Could not read root Cargo.toml");
            return;
        }
    };

    let root_val: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            warn!(path = ?root_toml_path, err = %e, "Could not parse root Cargo.toml");
            return;
        }
    };

    // Only proceed when this is a workspace manifest.
    let members_raw = match root_val
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
    {
        Some(arr) => arr.clone(),
        None => return,
    };

    // Expand member patterns (plain paths and simple `dir/*` globs).
    let mut member_dirs: Vec<String> = Vec::new();
    for item in &members_raw {
        let pattern = match item.as_str() {
            Some(s) => s,
            None => continue,
        };
        if pattern.ends_with("/*") {
            // Glob: list immediate subdirectories.
            let base = pattern.trim_end_matches("/*");
            let glob_dir = repo_root.join(base);
            match std::fs::read_dir(&glob_dir) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            let rel = format!("{}/{}", base, entry.file_name().to_string_lossy());
                            member_dirs.push(rel);
                        }
                    }
                }
                Err(e) => {
                    warn!(dir = ?glob_dir, err = %e, "Could not list workspace member glob dir");
                }
            }
        } else {
            member_dirs.push(pattern.to_string());
        }
    }

    // For each member directory, read its `Cargo.toml` to get the crate name.
    for member in member_dirs {
        let member_toml_path = repo_root.join(&member).join("Cargo.toml");
        let member_raw = match std::fs::read_to_string(&member_toml_path) {
            Ok(s) => s,
            Err(_) => continue, // Non-existent member dirs are silently skipped.
        };
        let member_val: toml::Value = match toml::from_str(&member_raw) {
            Ok(v) => v,
            Err(e) => {
                warn!(path = ?member_toml_path, err = %e, "Could not parse member Cargo.toml");
                continue;
            }
        };
        let crate_name = match member_val
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
        {
            Some(n) => n.to_string(),
            None => continue,
        };

        let src_dir = format!("{member}/src");
        aliases.cargo_crates.insert(crate_name, src_dir);
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalize: TypeScript path aliases ───────────────────────────────────

    #[test]
    fn ts_alias_at_slash_maps_to_src() {
        let aliases = ImportAliases {
            ts_paths: vec![("@/".to_string(), "src/".to_string())],
            ..Default::default()
        };
        assert_eq!(
            normalize("@/components/Button", &aliases),
            Some("src/components/Button".to_string())
        );
    }

    #[test]
    fn ts_alias_tilde_slash_maps_to_lib() {
        let aliases = ImportAliases {
            ts_paths: vec![("~/".to_string(), "lib/".to_string())],
            ..Default::default()
        };
        assert_eq!(
            normalize("~/utils/format", &aliases),
            Some("lib/utils/format".to_string())
        );
    }

    #[test]
    fn ts_alias_no_match_returns_none() {
        let aliases = ImportAliases {
            ts_paths: vec![("@/".to_string(), "src/".to_string())],
            ..Default::default()
        };
        assert_eq!(normalize("react", &aliases), None);
        assert_eq!(normalize("./util", &aliases), None);
    }

    // ── normalize: Go module prefix ───────────────────────────────────────────

    #[test]
    fn go_module_prefix_stripped() {
        let aliases = ImportAliases {
            go_module: Some("github.com/org/repo".to_string()),
            ..Default::default()
        };
        assert_eq!(
            normalize("github.com/org/repo/pkg/server", &aliases),
            Some("pkg/server".to_string())
        );
    }

    #[test]
    fn go_external_package_returns_none() {
        let aliases = ImportAliases {
            go_module: Some("github.com/org/repo".to_string()),
            ..Default::default()
        };
        assert_eq!(normalize("github.com/other/lib", &aliases), None);
        assert_eq!(normalize("fmt", &aliases), None);
    }

    // ── normalize: Cargo workspace crates ────────────────────────────────────

    #[test]
    fn cargo_crate_maps_to_member_src() {
        let mut crates = HashMap::new();
        crates.insert("my_lib".to_string(), "crates/my_lib/src".to_string());
        let aliases = ImportAliases {
            cargo_crates: crates,
            ..Default::default()
        };
        assert_eq!(
            normalize("my_lib::util::helper", &aliases),
            Some("crates/my_lib/src/util/helper".to_string())
        );
    }

    #[test]
    fn cargo_crate_hyphen_underscore_normalisation() {
        let mut crates = HashMap::new();
        crates.insert("my-lib".to_string(), "crates/my-lib/src".to_string());
        let aliases = ImportAliases {
            cargo_crates: crates,
            ..Default::default()
        };
        // `my_lib::x` should match crate `my-lib` (underscore → hyphen).
        assert_eq!(
            normalize("my_lib::x", &aliases),
            Some("crates/my-lib/src/x".to_string())
        );
    }

    #[test]
    fn cargo_same_crate_paths_return_none() {
        // `crate::`, `self::`, `super::` must NOT be handled here (EXT-3c covers them).
        let aliases = ImportAliases::default();
        assert_eq!(normalize("crate::foo::bar", &aliases), None);
        assert_eq!(normalize("self::helper", &aliases), None);
        assert_eq!(normalize("super::sibling", &aliases), None);
    }

    #[test]
    fn bare_relative_and_external_return_none() {
        let aliases = ImportAliases::default();
        assert_eq!(normalize("./util", &aliases), None);
        assert_eq!(normalize("react", &aliases), None);
        assert_eq!(normalize("std::collections::HashMap", &aliases), None);
    }

    // ── discover: integration tests using temp directories ───────────────────

    #[test]
    fn discover_tsconfig_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::write(
            root.join("tsconfig.json"),
            r#"{
  // compiler options
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@/*": ["src/*"],
      "~/*": ["lib/*"],
    }
  }
}"#,
        )
        .unwrap();

        let aliases = discover(root);
        assert!(
            aliases
                .ts_paths
                .contains(&("@/".to_string(), "src/".to_string())),
            "ts_paths = {:?}",
            aliases.ts_paths
        );
        assert!(
            aliases
                .ts_paths
                .contains(&("~/".to_string(), "lib/".to_string())),
            "ts_paths = {:?}",
            aliases.ts_paths
        );
    }

    #[test]
    fn discover_tsconfig_base_url_folded_for_non_dotslash_target() {
        // baseUrl "./src" + non-`./`-prefixed target "app/*" should fold the
        // baseUrl into the replacement: `@app/foo` → `src/app/foo`.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::write(
            root.join("tsconfig.json"),
            r#"{
  "compilerOptions": {
    "baseUrl": "./src",
    "paths": {
      "@app/*": ["app/*"]
    }
  }
}"#,
        )
        .unwrap();

        let aliases = discover(root);
        assert_eq!(
            normalize("@app/foo", &aliases),
            Some("src/app/foo".to_string()),
            "ts_paths = {:?}",
            aliases.ts_paths
        );
    }

    #[test]
    fn discover_tsconfig_dot_base_url_non_dotslash_target_no_leading_dot() {
        // baseUrl "." + non-`./`-prefixed target "app/*" must NOT yield a
        // leading `./`: `@app/foo` → `app/foo`.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::write(
            root.join("tsconfig.json"),
            r#"{
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@app/*": ["app/*"]
    }
  }
}"#,
        )
        .unwrap();

        let aliases = discover(root);
        assert_eq!(
            normalize("@app/foo", &aliases),
            Some("app/foo".to_string()),
            "ts_paths = {:?}",
            aliases.ts_paths
        );
    }

    #[test]
    fn discover_tsconfig_base_url_folded_for_dotslash_target() {
        // baseUrl "./src" + `./`-prefixed target "./app/*" should also fold:
        // `@app/foo` → `src/app/foo` (confirms both branches fold baseUrl).
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::write(
            root.join("tsconfig.json"),
            r#"{
  "compilerOptions": {
    "baseUrl": "./src",
    "paths": {
      "@app/*": ["./app/*"]
    }
  }
}"#,
        )
        .unwrap();

        let aliases = discover(root);
        assert_eq!(
            normalize("@app/foo", &aliases),
            Some("src/app/foo".to_string()),
            "ts_paths = {:?}",
            aliases.ts_paths
        );
    }

    #[test]
    fn discover_go_mod() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::write(
            root.join("go.mod"),
            "module github.com/acme/myapp\n\ngo 1.21\n",
        )
        .unwrap();

        let aliases = discover(root);
        assert_eq!(aliases.go_module.as_deref(), Some("github.com/acme/myapp"));
    }

    #[test]
    fn discover_cargo_workspace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        // Root workspace Cargo.toml.
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[workspace]
members = ["crates/alpha", "crates/beta"]
"#,
        )
        .unwrap();

        // Member `alpha`.
        let alpha_dir = root.join("crates/alpha/src");
        std::fs::create_dir_all(&alpha_dir).unwrap();
        std::fs::write(
            root.join("crates/alpha/Cargo.toml"),
            "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

        // Member `beta`.
        let beta_dir = root.join("crates/beta/src");
        std::fs::create_dir_all(&beta_dir).unwrap();
        std::fs::write(
            root.join("crates/beta/Cargo.toml"),
            "[package]\nname = \"beta\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

        let aliases = discover(root);
        assert_eq!(
            aliases.cargo_crates.get("alpha").map(|s| s.as_str()),
            Some("crates/alpha/src")
        );
        assert_eq!(
            aliases.cargo_crates.get("beta").map(|s| s.as_str()),
            Some("crates/beta/src")
        );
    }

    #[test]
    fn discover_missing_files_returns_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let aliases = discover(tmp.path());
        assert!(aliases.ts_paths.is_empty());
        assert!(aliases.go_module.is_none());
        assert!(aliases.cargo_crates.is_empty());
    }

    #[test]
    fn ts_alias_prefers_longest_prefix_not_first() {
        // A generic prefix listed FIRST must not shadow a longer, more specific
        // one listed later (the old code returned the first starts_with match).
        let aliases = ImportAliases {
            ts_paths: vec![
                ("@/".to_string(), "src/".to_string()),
                (
                    "@/components/".to_string(),
                    "src/lib/components/".to_string(),
                ),
            ],
            ..Default::default()
        };
        assert_eq!(
            normalize("@/components/Button", &aliases).as_deref(),
            Some("src/lib/components/Button"),
        );
    }

    // ── normalize: Go bare-module-path edge cases (finding #1) ────────────────

    #[test]
    fn go_bare_module_path_maps_to_repo_root() {
        // An import EQUAL to the module path itself (no trailing segment) is the
        // package at the repo root; it must rewrite to the empty repo-relative
        // path, NOT fall through to `None`.
        let aliases = ImportAliases {
            go_module: Some("github.com/org/repo".to_string()),
            ..Default::default()
        };
        assert_eq!(
            normalize("github.com/org/repo", &aliases),
            Some(String::new())
        );
        // The trailing-segment (sub-package) case is unaffected by the fix.
        assert_eq!(
            normalize("github.com/org/repo/pkg/server", &aliases),
            Some("pkg/server".to_string())
        );
        // A different module that merely shares a prefix is still external: the
        // bare-match must be EXACT, not a prefix of a longer foreign path.
        assert_eq!(normalize("github.com/org/repository", &aliases), None);
    }

    #[test]
    fn go_empty_module_never_matches() {
        // A garbage `Some("")` module must not turn the prefix into a bare `"/"`
        // (which would match every specifier) nor map empty input to the root.
        let aliases = ImportAliases {
            go_module: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(normalize("", &aliases), None);
        assert_eq!(normalize("/anything", &aliases), None);
        assert_eq!(normalize("github.com/x/y", &aliases), None);
    }
}
