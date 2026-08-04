//! Preset registry: a TOML table mapping doc-site hosts to their extraction
//! adapter, consulted by `AdapterResolver` for the pinned-host resolution step.
//! The checked-in table (compile-time `include_str!`) ships empty; deployments
//! pin their own sites via a runtime file (`AKASHIC_PRESETS_PATH`) merged on
//! top by [`PresetRegistry::load_merged`].

/// One preset entry. `adapter_id` may reference an adapter not yet registered;
/// the resolver handles that by warning + falling back.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct PresetEntry {
    /// Exact host (`"wiki.example.com"`) or a `"*.suffix"` glob.
    pub host_pattern: String,
    pub adapter_id: String,
    /// Opaque per-preset config handed to the adapter (read by A2b+ adapters).
    #[serde(default)]
    pub adapter_config: Option<toml::Value>,
    /// `false` = exclude from any bulk/auto crawl (consumer: A2b+/D).
    #[serde(default = "default_true")]
    pub auto_ingest: bool,
    /// Optional per-preset crawl overrides (consumer: A2b+).
    #[serde(default)]
    pub crawl_limits: Option<RegistryCrawlLimits>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct RegistryCrawlLimits {
    pub depth: Option<u8>,
    pub max_pages: Option<usize>,
    pub delay_ms: Option<u64>,
}

fn default_true() -> bool {
    true
}

/// Failure loading a runtime presets file. Composition roots treat this as a
/// fatal startup misconfiguration — silently dropping operator presets would
/// be worse than refusing to boot.
#[derive(Debug, thiserror::Error)]
pub enum PresetLoadError {
    #[error("cannot read presets file {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse presets file {path}: {source}")]
    Parse {
        path: std::path::PathBuf,
        source: Box<toml::de::Error>,
    },
}

/// Merge runtime entries over a base table: an entry with the same
/// `host_pattern` replaces the base entry in place; new patterns append.
fn merge_entries(base: Vec<PresetEntry>, runtime: Vec<PresetEntry>) -> Vec<PresetEntry> {
    let mut out = base;
    for e in runtime {
        match out.iter_mut().find(|b| b.host_pattern == e.host_pattern) {
            Some(existing) => *existing = e,
            None => out.push(e),
        }
    }
    out
}

/// In-memory preset table.
#[derive(Clone, Debug, Default)]
pub struct PresetRegistry {
    pub(crate) entries: Vec<PresetEntry>,
}

#[derive(serde::Deserialize)]
struct PresetFile {
    #[serde(default)]
    preset: Vec<PresetEntry>,
}

impl PresetRegistry {
    /// Parse the checked-in `presets.toml`. A parse failure is a build-shipped
    /// bug — it panics here and is caught by the parse test in CI, never in prod.
    pub fn load() -> Self {
        let file: PresetFile =
            toml::from_str(include_str!("presets.toml")).expect("presets.toml must parse");
        Self {
            entries: file.preset,
        }
    }

    /// Embedded presets merged with an optional runtime presets file (same
    /// TOML schema; deployments point `AKASHIC_PRESETS_PATH` at it). Runtime
    /// entries with a `host_pattern` already in the embedded table replace it;
    /// new patterns append.
    pub fn load_merged(runtime_path: Option<&std::path::Path>) -> Result<Self, PresetLoadError> {
        let mut reg = Self::load();
        if let Some(path) = runtime_path {
            let text = std::fs::read_to_string(path).map_err(|source| PresetLoadError::Read {
                path: path.to_owned(),
                source,
            })?;
            let file: PresetFile =
                toml::from_str(&text).map_err(|source| PresetLoadError::Parse {
                    path: path.to_owned(),
                    source: Box::new(source),
                })?;
            reg.entries = merge_entries(reg.entries, file.preset);
        }
        Ok(reg)
    }

    /// Match a URL host. Exact-host patterns win over `"*.suffix"` globs; among
    /// globs the longest suffix wins. Returns the matched entry (its adapter_id
    /// may or may not be registered — the resolver decides).
    pub fn match_host(&self, host: &str) -> Option<&PresetEntry> {
        if let Some(e) = self.entries.iter().find(|e| e.host_pattern == host) {
            return Some(e);
        }
        self.entries
            .iter()
            .filter(|e| e.host_pattern.starts_with("*."))
            .filter(|e| host.ends_with(&e.host_pattern[1..])) // ".pages.example.com"
            .max_by_key(|e| e.host_pattern.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_presets_toml_parses_and_ships_empty() {
        // The checked-in table is documentation only — site pins are a
        // deployment concern (AKASHIC_PRESETS_PATH), never baked in.
        let reg = PresetRegistry::load();
        assert!(
            reg.entries.is_empty(),
            "embedded presets.toml must stay empty; pin sites via AKASHIC_PRESETS_PATH"
        );
    }

    #[test]
    fn match_host_exact_beats_glob_and_globs_match_subdomains() {
        let reg = PresetRegistry {
            entries: vec![
                entry("*.pages.example.com", "glob"),
                entry("exact.pages.example.com", "exact"),
            ],
        };
        // exact host wins over the glob
        assert_eq!(
            reg.match_host("exact.pages.example.com")
                .unwrap()
                .adapter_id,
            "exact"
        );
        // a different subdomain matches the glob
        assert_eq!(
            reg.match_host("dit.pages.example.com").unwrap().adapter_id,
            "glob"
        );
        // the bare suffix (no subdomain) does NOT match the glob
        assert!(reg.match_host("pages.example.com").is_none());
        // unrelated host: no match
        assert!(reg.match_host("unrelated.net").is_none());
    }

    fn entry(host_pattern: &str, adapter_id: &str) -> PresetEntry {
        PresetEntry {
            host_pattern: host_pattern.into(),
            adapter_id: adapter_id.into(),
            adapter_config: None,
            auto_ingest: true,
            crawl_limits: None,
        }
    }

    #[test]
    fn merge_replaces_same_host_pattern_and_appends_new() {
        let base = vec![
            entry("wiki.example.com", "mediawiki"),
            entry("*.docs.example.com", "gitbook-static"),
        ];
        let runtime = vec![
            entry("wiki.example.com", "codimd"), // same pattern → replaces
            entry("kb.example.com", "ssr-hydration"), // new → appended
        ];
        let merged = merge_entries(base, runtime);
        assert_eq!(merged.len(), 3);
        assert_eq!(
            merged
                .iter()
                .find(|e| e.host_pattern == "wiki.example.com")
                .unwrap()
                .adapter_id,
            "codimd"
        );
        assert_eq!(
            merged
                .iter()
                .find(|e| e.host_pattern == "kb.example.com")
                .unwrap()
                .adapter_id,
            "ssr-hydration"
        );
        assert_eq!(
            merged
                .iter()
                .find(|e| e.host_pattern == "*.docs.example.com")
                .unwrap()
                .adapter_id,
            "gitbook-static"
        );
    }

    #[test]
    fn load_merged_none_equals_embedded() {
        let merged = PresetRegistry::load_merged(None).unwrap();
        assert_eq!(merged.entries.len(), PresetRegistry::load().entries.len());
    }

    #[test]
    fn load_merged_reads_runtime_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.toml");
        std::fs::write(
            &path,
            r#"
[[preset]]
host_pattern = "wiki.internal.example"
adapter_id   = "mediawiki"
auto_ingest  = false
"#,
        )
        .unwrap();
        let reg = PresetRegistry::load_merged(Some(&path)).unwrap();
        let hit = reg.match_host("wiki.internal.example").unwrap();
        assert_eq!(hit.adapter_id, "mediawiki");
        assert!(!hit.auto_ingest);
    }

    #[test]
    fn load_merged_missing_file_errors() {
        let err =
            PresetRegistry::load_merged(Some(std::path::Path::new("/nonexistent/presets.toml")))
                .unwrap_err();
        assert!(err.to_string().contains("/nonexistent/presets.toml"));
    }

    #[test]
    fn load_merged_invalid_toml_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.toml");
        std::fs::write(&path, "[[preset]]\nhost_pattern = 42\n").unwrap();
        let err = PresetRegistry::load_merged(Some(&path)).unwrap_err();
        assert!(err.to_string().contains("presets.toml"));
    }

    #[test]
    fn longest_suffix_glob_wins() {
        let reg = PresetRegistry {
            entries: vec![
                entry("*.com", "short"),
                entry("*.pages.example.com", "long"),
            ],
        };
        assert_eq!(
            reg.match_host("dit.pages.example.com").unwrap().adapter_id,
            "long"
        );
    }
}
