//! Human-readable formatter for OAuth runtime validation reports.
//! Used by the `check-oauth` CLI subcommand. Plain text (no color in v1).

use akashic_domain::types::{CheckStatus, ValidationReport};

/// Render a `ValidationReport` as a plain-text table to stdout.
pub fn print_human_report(report: &ValidationReport) {
    let bar = "─".repeat(70);
    println!("Akashic Record — OAuth runtime validation");
    println!("{bar}");
    for c in &report.checks {
        let tag = match c.status {
            CheckStatus::Ok => "OK   ",
            CheckStatus::Warn => "WARN ",
            CheckStatus::Fail => "FAIL ",
        };
        println!(
            "[{tag}]  {:<32}  {:>5} ms  {}",
            c.name, c.elapsed_ms, c.detail,
        );
        if let Some(rem) = &c.remediation {
            for line in rem.lines() {
                println!("         → {line}");
            }
        }
    }
    println!("{bar}");
    let overall = match report.overall {
        CheckStatus::Ok => "OK",
        CheckStatus::Warn => "WARN",
        CheckStatus::Fail => "FAIL",
    };
    println!(
        "Overall: {overall}  ({} ok, {} warn, {} fail)",
        report.n_ok(),
        report.n_warn(),
        report.n_fail(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use akashic_domain::types::CheckResult;

    #[test]
    fn print_human_report_does_not_panic() {
        let r = ValidationReport::from_checks(vec![
            CheckResult::ok("gitlab_url_reachable", "GitLab 16.0.0").with_elapsed(87),
            CheckResult::fail(
                "client_credentials_valid",
                "GitLab returned 401 invalid_client",
                Some("Verify GITLAB_APP_ID and GITLAB_APP_SECRET.".into()),
            )
            .with_elapsed(142),
        ]);
        print_human_report(&r);
    }
}
