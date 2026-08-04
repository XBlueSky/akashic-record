use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum BaselineStep {
    Grep {
        pattern: String,
        #[serde(default)]
        glob: Option<String>,
    },
    Read {
        file: String,
        #[serde(default)]
        lines: Option<[usize; 2]>,
    },
}

#[derive(Debug, Deserialize)]
pub struct AkashicStep {
    pub tool: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct Question {
    pub id: String,
    pub category: String,
    pub question: String,
    pub ground_truth: Vec<String>,
    pub baseline_plan: Vec<BaselineStep>,
    pub akashic_plan: Vec<AkashicStep>,
}

/// Load and validate the question fixture. Each entry must have a non-empty
/// `id` and a non-empty `ground_truth` (a question with no answer facts cannot
/// be scored).
pub fn load_questions(path: &Path) -> Result<Vec<Question>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading question fixture {}", path.display()))?;
    let qs: Vec<Question> = serde_json::from_str(&raw)
        .with_context(|| format!("parsing question fixture {}", path.display()))?;
    for q in &qs {
        if q.id.trim().is_empty() {
            bail!("question fixture has an entry with an empty id");
        }
        if q.ground_truth.is_empty() {
            bail!(
                "question '{}' has empty ground_truth (cannot score an answer)",
                q.id
            );
        }
    }
    Ok(qs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_a_sample_fixture() {
        let json = r#"[
          {"id":"q1","category":"who-calls","question":"who calls foo?",
           "ground_truth":["bar","baz"],
           "baseline_plan":[{"op":"grep","pattern":"foo","glob":"**/*.rs"},
                            {"op":"read","file":"src/x.rs","lines":[1,40]}],
           "akashic_plan":[{"tool":"traverse_code_calls","args":{"symbol":"foo","direction":"callers"}}]}
        ]"#;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(json.as_bytes()).unwrap();
        let qs = load_questions(f.path()).unwrap();
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0].id, "q1");
        assert_eq!(qs[0].ground_truth, vec!["bar", "baz"]);
        assert_eq!(qs[0].baseline_plan.len(), 2);
        assert!(matches!(qs[0].baseline_plan[0], BaselineStep::Grep { .. }));
        assert!(matches!(qs[0].baseline_plan[1], BaselineStep::Read { .. }));
        assert_eq!(qs[0].akashic_plan[0].tool, "traverse_code_calls");
    }

    #[test]
    fn rejects_empty_ground_truth() {
        let json = r#"[{"id":"bad","category":"x","question":"?","ground_truth":[],"baseline_plan":[],"akashic_plan":[]}]"#;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(json.as_bytes()).unwrap();
        assert!(load_questions(f.path()).is_err());
    }
}
