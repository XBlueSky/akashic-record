//! Saga and step status enums + branch-name parsing — pure, DB-free.
//!
//! Moved from `akashic-curation::saga::mod` (SagaStatus/StepStatus) and
//! `akashic-curation::notes::saga` (extract_issue_from_branch) (A1 Task 2).

use std::sync::LazyLock;

use regex::Regex;

/// Saga status values (stored as TEXT in PostgreSQL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SagaStatus {
    Running,
    Completed,
    Compensating,
    Failed,
}

impl SagaStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Compensating => "compensating",
            Self::Failed => "failed",
        }
    }
}

/// Step status values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Done,
    Compensating,
    Compensated,
    Failed,
}

impl StepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Compensating => "compensating",
            Self::Compensated => "compensated",
            Self::Failed => "failed",
        }
    }
}

static WP_BRANCH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^wp/([A-Z]+)/(\d+)$").unwrap());

/// Parse branch names like `wp/PROJ/123456` into `PROJ-123456`.
///
/// Returns `None` for branches that don't match the Workplus naming convention.
pub fn extract_issue_from_branch(branch: &str) -> Option<String> {
    let caps = WP_BRANCH_RE.captures(branch)?;
    Some(format!("{}-{}", &caps[1], &caps[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saga_status_as_str_roundtrip() {
        assert_eq!(SagaStatus::Running.as_str(), "running");
        assert_eq!(SagaStatus::Completed.as_str(), "completed");
        assert_eq!(SagaStatus::Compensating.as_str(), "compensating");
        assert_eq!(SagaStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn step_status_as_str_roundtrip() {
        assert_eq!(StepStatus::Pending.as_str(), "pending");
        assert_eq!(StepStatus::Done.as_str(), "done");
        assert_eq!(StepStatus::Compensating.as_str(), "compensating");
        assert_eq!(StepStatus::Compensated.as_str(), "compensated");
        assert_eq!(StepStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn extract_issue_from_branch_valid() {
        assert_eq!(
            extract_issue_from_branch("wp/PROJ/123456"),
            Some("PROJ-123456".to_string())
        );
        assert_eq!(
            extract_issue_from_branch("wp/LIB/78901"),
            Some("LIB-78901".to_string())
        );
    }

    #[test]
    fn extract_issue_from_branch_invalid() {
        assert_eq!(extract_issue_from_branch("main"), None);
        assert_eq!(extract_issue_from_branch("feature/foo"), None);
        assert_eq!(extract_issue_from_branch("wp/proj/123456"), None); // lowercase project
        assert_eq!(extract_issue_from_branch("wp/PROJ/abc"), None); // non-numeric ID
    }
}
