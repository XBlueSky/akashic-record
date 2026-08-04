//! LLM-powered virtual module grouping for oversized directories.
//!
//! When a directory contains more than `module_max_files` files,
//! we ask a fast LLM to group them into logical sub-modules with
//! descriptive architectural names. The LLM receives rich AST context
//! (chunk types, names, signatures) for semantic understanding.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use akashic_llm::LlmProvider;

/// File metadata passed to the grouping engine — includes AST symbols.
pub struct FileGroupingMeta {
    pub filename: String,
    pub symbols: Vec<(String, String, Option<String>)>, // (chunk_type, name, signature)
}

/// Input: file metadata for LLM grouping — includes AST symbols for semantic context.
#[derive(Serialize)]
struct FileEntry {
    filename: String,
    symbols: Vec<SymbolEntry>,
}

/// A single AST symbol extracted from a file.
#[derive(Serialize)]
struct SymbolEntry {
    #[serde(rename = "type")]
    sym_type: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sig: Option<String>,
}

/// LLM response schema.
#[derive(Deserialize)]
struct GroupingResponse {
    groups: Vec<Group>,
}

#[derive(Deserialize)]
struct Group {
    group_name: String,
    files: Vec<String>,
}

/// Result of virtual grouping: maps each filename to its assigned group name.
pub struct VirtualGroupResult {
    /// Vec of (group_name, vec_of_filenames)
    pub groups: Vec<(String, Vec<String>)>,
}

/// Determines if a module needs virtual splitting and performs it.
///
/// Returns `None` if the module is within limits, or `Some(VirtualGroupResult)`.
pub async fn maybe_split_module(
    module_max_files: u32,
    llm: &dyn LlmProvider,
    module_path: &str,
    files: &[FileGroupingMeta],
) -> Option<VirtualGroupResult> {
    let file_count = files.len() as u32;
    if file_count <= module_max_files {
        return None;
    }

    info!(
        module_path,
        file_count, "Module exceeds threshold, triggering LLM virtual grouping"
    );

    match llm_group(llm, module_path, files).await {
        Ok(result) => Some(result),
        Err(e) => {
            warn!("LLM grouping failed for {module_path}: {e:#}. Falling back to prefix grouping.");
            Some(prefix_fallback(files))
        }
    }
}

/// Call the LLM to group files into logical sub-modules.
async fn llm_group(
    llm: &dyn LlmProvider,
    module_path: &str,
    files: &[FileGroupingMeta],
) -> Result<VirtualGroupResult> {
    let file_count = files.len();
    let target_groups = ((file_count as f64) / 6.0).ceil().max(2.0) as u32;

    let entries: Vec<FileEntry> = files
        .iter()
        .map(|f| FileEntry {
            filename: f.filename.clone(),
            symbols: f
                .symbols
                .iter()
                .map(|(t, n, s)| SymbolEntry {
                    sym_type: t.clone(),
                    name: n.clone(),
                    sig: s.clone(),
                })
                .collect(),
        })
        .collect();

    let payload_json = serde_json::to_string(&entries)?;

    let prompt = format!(
        r#"You are a software architect analyzing the "{module_path}" directory ({file_count} files).
Group these files into approximately {target_groups} logical sub-modules based on their purpose, the classes/functions they define, and their domain responsibility.

STRICT RULES:
1. EVERY file must appear in exactly one group. Do not omit any file.
2. FORBIDDEN group names: "Miscellaneous", "Others", "Common", "General", "Utilities", "Misc", "Ungrouped", "Remaining". These are lazy and meaningless.
3. Group names MUST be 2-3 words maximum. Use concise domain-driven names, e.g.: "HTTP Routing", "TLS & Auth", "Nginx Config", "Build Scripts", "Test Fixtures", "Memory Mgmt".
4. Each group should have 3-8 files. Avoid groups with only 1 file unless it's truly standalone.
5. Group by semantic purpose, NOT by file extension or naming prefix.

Return strictly JSON (no markdown fences, no explanation):
{{"groups": [{{"group_name": "...", "files": ["filename1.ext", "filename2.ext"]}}]}}

Files with their symbols:
{payload_json}"#,
    );

    let r = llm.generate_json(&prompt).await?;
    let cleaned = akashic_llm::extract_json(&r.text);

    let parsed: GroupingResponse =
        serde_json::from_str(cleaned).context("Failed to parse LLM grouping JSON")?;

    Ok(VirtualGroupResult {
        groups: parsed
            .groups
            .into_iter()
            .map(|g| (g.group_name, g.files))
            .collect(),
    })
}

/// Deterministic fallback: group files by common filename prefix.
fn prefix_fallback(files: &[FileGroupingMeta]) -> VirtualGroupResult {
    use std::collections::HashMap;

    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for f in files {
        let prefix = f
            .filename
            .split(['_', '-', '.'])
            .next()
            .unwrap_or("ungrouped")
            .to_string();
        groups.entry(prefix).or_default().push(f.filename.clone());
    }

    // Merge tiny groups (< 2 files) into the largest existing group
    let mut result: Vec<(String, Vec<String>)> = Vec::new();
    let mut orphans: Vec<String> = Vec::new();
    for (prefix, filenames) in groups {
        if filenames.len() < 2 {
            orphans.extend(filenames);
        } else {
            result.push((prefix, filenames));
        }
    }

    if !orphans.is_empty() {
        if let Some(largest) = result.iter_mut().max_by_key(|(_, files)| files.len()) {
            largest.1.extend(orphans);
        } else {
            result.push(("All Files".into(), orphans));
        }
    }

    VirtualGroupResult { groups: result }
}
