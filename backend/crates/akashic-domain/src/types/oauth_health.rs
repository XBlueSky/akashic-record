//! OAuth-config health-check value types.
//!
//! Port-agnostic result types for the GitLab OAuth runtime validation suite.
//! Returned by `crate::ports::gitlab::GitLabGateway::runtime_report` and cached
//! on `AppState.oauth_health_cache`. The check *logic* lives in the adapter
//! crate `akashic-gitlab`; only the shapes live here.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl CheckStatus {
    /// Roll-up: max(Ok, Warn, Fail) where Fail > Warn > Ok.
    pub fn worst(self, other: CheckStatus) -> CheckStatus {
        use CheckStatus::{Fail, Ok, Warn};
        match (self, other) {
            (Fail, _) | (_, Fail) => Fail,
            (Warn, _) | (_, Warn) => Warn,
            _ => Ok,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: &'static str,
    pub status: CheckStatus,
    pub detail: String,
    pub remediation: Option<String>,
    pub elapsed_ms: u64,
}

impl CheckResult {
    pub fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Ok,
            detail: detail.into(),
            remediation: None,
            elapsed_ms: 0,
        }
    }
    pub fn warn(
        name: &'static str,
        detail: impl Into<String>,
        remediation: Option<String>,
    ) -> Self {
        Self {
            name,
            status: CheckStatus::Warn,
            detail: detail.into(),
            remediation,
            elapsed_ms: 0,
        }
    }
    pub fn fail(
        name: &'static str,
        detail: impl Into<String>,
        remediation: Option<String>,
    ) -> Self {
        Self {
            name,
            status: CheckStatus::Fail,
            detail: detail.into(),
            remediation,
            elapsed_ms: 0,
        }
    }
    pub fn with_elapsed(mut self, elapsed_ms: u64) -> Self {
        self.elapsed_ms = elapsed_ms;
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub overall: CheckStatus,
    pub checks: Vec<CheckResult>,
}

impl ValidationReport {
    pub fn from_checks(checks: Vec<CheckResult>) -> Self {
        let overall = checks
            .iter()
            .fold(CheckStatus::Ok, |acc, c| acc.worst(c.status));
        Self {
            generated_at: chrono::Utc::now(),
            overall,
            checks,
        }
    }

    pub fn n_ok(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Ok)
            .count()
    }
    pub fn n_warn(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Warn)
            .count()
    }
    pub fn n_fail(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Fail)
            .count()
    }
}
