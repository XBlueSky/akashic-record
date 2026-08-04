//! GitLab OAuth runtime validation suite.
//!
//! Single source-of-truth `validate()` runs 6 checks (3 static + 3 network)
//! against the configured GitLab OAuth setup. Three surfaces consume it (via
//! `GitLabGateway::runtime_report`): startup-time gating (OAUTH_VALIDATION_MODE),
//! /api/v1/health/oauth, and the `check-oauth` CLI subcommand.
//!
//! Moved verbatim from akashic-identity::oauth_runtime; the network checks own
//! the only probe HTTP.

use std::time::Duration;

use akashic_config::Config;
use akashic_domain::types::{CheckResult, ValidationReport};
use secrecy::ExposeSecret;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationMode {
    Off,
    Warn,
    Strict,
}

impl ValidationMode {
    pub fn from_env_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" | "no" => ValidationMode::Off,
            "strict" | "fail" | "hard" => ValidationMode::Strict,
            _ => ValidationMode::Warn,
        }
    }
}

pub fn check_redirect_uri_consistent(cfg: &Config) -> CheckResult {
    let expected = format!(
        "{}/auth/callback",
        cfg.public_base_url.trim_end_matches('/')
    );
    if cfg.gitlab_redirect_uri == expected {
        CheckResult::ok("redirect_uri_consistent", format!("matches {expected}"))
    } else {
        CheckResult::warn(
            "redirect_uri_consistent",
            format!(
                "GITLAB_REDIRECT_URI=`{}` but expected `{expected}` (computed from PUBLIC_BASE_URL)",
                cfg.gitlab_redirect_uri,
            ),
            Some(
                "Either set GITLAB_REDIRECT_URI to match, or update PUBLIC_BASE_URL. \
                 Mismatch means the device-flow `verification_uri` will be wrong."
                    .into(),
            ),
        )
    }
}

pub fn check_web_redirect_uri_consistent(cfg: &Config) -> CheckResult {
    let expected = format!(
        "{}/auth/web/callback",
        cfg.public_base_url.trim_end_matches('/')
    );
    if cfg.gitlab_web_redirect_uri == expected {
        CheckResult::ok("web_redirect_uri_consistent", format!("matches {expected}"))
    } else {
        CheckResult::warn(
            "web_redirect_uri_consistent",
            format!(
                "GITLAB_WEB_REDIRECT_URI=`{}` but expected `{expected}` (computed from PUBLIC_BASE_URL)",
                cfg.gitlab_web_redirect_uri,
            ),
            Some("Either set GITLAB_WEB_REDIRECT_URI to match, or update PUBLIC_BASE_URL.".into()),
        )
    }
}

pub fn check_https_in_production(cfg: &Config) -> CheckResult {
    if !cfg.is_production() {
        return CheckResult::ok(
            "https_in_production",
            "skipped (not AKASHIC_ENV=production)",
        );
    }
    let mut violations: Vec<String> = Vec::new();
    for (name, value) in [
        ("GITLAB_URL", &cfg.gitlab_url),
        ("PUBLIC_BASE_URL", &cfg.public_base_url),
        ("FRONTEND_URL", &cfg.frontend_url),
    ] {
        if !value.starts_with("https://") {
            violations.push(format!("{name}=`{value}`"));
        }
    }
    if violations.is_empty() {
        CheckResult::ok("https_in_production", "all URLs use https")
    } else {
        CheckResult::fail(
            "https_in_production",
            format!("non-HTTPS URLs in production: {}", violations.join(", ")),
            Some(
                "Public-facing URLs must be HTTPS in production. Update env vars or remove AKASHIC_ENV=production."
                    .into(),
            ),
        )
    }
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn check_gitlab_url_reachable(cfg: &Config, http: &reqwest::Client) -> CheckResult {
    let url = format!("{}/api/v4/version", cfg.gitlab_url.trim_end_matches('/'));
    let start = std::time::Instant::now();
    let resp = match http.get(&url).timeout(PROBE_TIMEOUT).send().await {
        Ok(r) => r,
        Err(e) => {
            return CheckResult::fail(
                "gitlab_url_reachable",
                format!("connect/transport error: {e}"),
                Some(format!(
                    "Verify GITLAB_URL ({}) is reachable from this host. Check DNS, firewall, and TLS.",
                    cfg.gitlab_url,
                )),
            )
            .with_elapsed(start.elapsed().as_millis() as u64);
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        return CheckResult::fail(
            "gitlab_url_reachable",
            format!("GET {url} returned HTTP {status}"),
            Some(format!(
                "Verify GITLAB_URL ({}) points to a GitLab instance, not a proxy or auth wall.",
                cfg.gitlab_url,
            )),
        )
        .with_elapsed(start.elapsed().as_millis() as u64);
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => {
            return CheckResult::fail(
                "gitlab_url_reachable",
                "response was not JSON".to_string(),
                Some(format!(
                    "GITLAB_URL ({}) returned non-JSON to /api/v4/version. Possibly behind a captive portal or SSO proxy.",
                    cfg.gitlab_url,
                )),
            )
            .with_elapsed(start.elapsed().as_millis() as u64);
        }
    };
    let version = body
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    CheckResult::ok(
        "gitlab_url_reachable",
        format!("GitLab {version} at {}", cfg.gitlab_url),
    )
    .with_elapsed(start.elapsed().as_millis() as u64)
}

pub async fn check_oauth_token_endpoint_reachable(
    cfg: &Config,
    http: &reqwest::Client,
) -> CheckResult {
    let url = format!("{}/oauth/token", cfg.gitlab_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "grant_type": "password",
        "username": "_b5_probe_",
        "password": "_b5_probe_",
    });
    let start = std::time::Instant::now();
    let resp = match http
        .post(&url)
        .timeout(PROBE_TIMEOUT)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return CheckResult::fail(
                "oauth_token_endpoint_reachable",
                format!("connect/transport error: {e}"),
                Some(format!(
                    "Verify {url} is reachable from this host. Likely the OAuth path is blocked or routed elsewhere.",
                )),
            )
            .with_elapsed(start.elapsed().as_millis() as u64);
        }
    };
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let status_code = resp.status().as_u16();
    if (500..600).contains(&status_code) {
        return CheckResult::warn(
            "oauth_token_endpoint_reachable",
            format!("GitLab returned HTTP {status_code} on /oauth/token probe"),
            Some(
                "GitLab is up but its OAuth endpoint errored. Likely transient; if persistent, check GitLab logs."
                    .to_string(),
            ),
        )
        .with_elapsed(elapsed_ms);
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => {
            return CheckResult::fail(
                "oauth_token_endpoint_reachable",
                format!("/oauth/token returned non-JSON (HTTP {status_code})"),
                Some(
                    "GitLab's /oauth/token returned HTML or other non-OAuth shape. Check for a proxy or auth wall in front of GitLab."
                        .to_string(),
                ),
            )
            .with_elapsed(elapsed_ms);
        }
    };
    if body.get("error").and_then(|v| v.as_str()).is_some() {
        CheckResult::ok(
            "oauth_token_endpoint_reachable",
            "/oauth/token responded with structured error".to_string(),
        )
        .with_elapsed(elapsed_ms)
    } else {
        CheckResult::fail(
            "oauth_token_endpoint_reachable",
            format!("/oauth/token returned HTTP {status_code} without `error` field"),
            Some(
                "Response did not match OAuth error shape. Check GITLAB_URL points to a real GitLab /oauth path, not a generic API gateway."
                    .to_string(),
            ),
        )
        .with_elapsed(elapsed_ms)
    }
}

pub async fn check_client_credentials_valid(cfg: &Config, http: &reqwest::Client) -> CheckResult {
    let url = format!("{}/oauth/token", cfg.gitlab_url.trim_end_matches('/'));
    let start = std::time::Instant::now();
    let resp = match http
        .post(&url)
        .timeout(PROBE_TIMEOUT)
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", cfg.gitlab_app_id.as_str()),
            ("client_secret", cfg.gitlab_app_secret.expose_secret()),
        ])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return CheckResult::warn(
                "client_credentials_valid",
                format!("could not reach OAuth token endpoint: {e}"),
                Some(
                    "Network issue prevented active validation. The static config still passed; GitLab may be down."
                        .to_string(),
                ),
            )
            .with_elapsed(start.elapsed().as_millis() as u64);
        }
    };
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let status_code = resp.status().as_u16();

    if status_code == 200 {
        return CheckResult::ok(
            "client_credentials_valid",
            "credentials accepted by GitLab".to_string(),
        )
        .with_elapsed(elapsed_ms);
    }

    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    let error_str = body.get("error").and_then(|v| v.as_str()).unwrap_or("");

    if status_code == 401 && error_str == "invalid_client" {
        return CheckResult::fail(
            "client_credentials_valid",
            "GitLab returned 401 invalid_client".to_string(),
            Some(
                "Verify GITLAB_APP_ID and GITLAB_APP_SECRET against the application in GitLab → Admin → Applications. Re-issue if the secret has been rotated."
                    .to_string(),
            ),
        )
        .with_elapsed(elapsed_ms);
    }

    if status_code == 400 && error_str == "unauthorized_client" {
        return CheckResult::warn(
            "client_credentials_valid",
            "GitLab returned 400 unauthorized_client (app type does not allow client_credentials grant)".to_string(),
            Some(
                "App is registered as `public` in GitLab — client_credentials grant requires `confidential` type. Static config still passed; recreate the app as confidential to enable active validation."
                    .to_string(),
            ),
        )
        .with_elapsed(elapsed_ms);
    }

    if (500..600).contains(&status_code) {
        return CheckResult::warn(
            "client_credentials_valid",
            format!("GitLab returned HTTP {status_code}"),
            Some("GitLab is errored; not necessarily a config problem.".to_string()),
        )
        .with_elapsed(elapsed_ms);
    }

    CheckResult::warn(
        "client_credentials_valid",
        format!("unexpected response: HTTP {status_code} error={error_str}"),
        Some("Could not actively validate credentials. Static config passed.".to_string()),
    )
    .with_elapsed(elapsed_ms)
}

/// Run all 6 checks (3 static + 3 network) and roll up into a `ValidationReport`.
/// Network checks run in parallel via `tokio::join!`; total wall-clock is bounded
/// by the slowest single network probe (5s timeout each).
pub async fn validate(cfg: &Config, http: &reqwest::Client) -> ValidationReport {
    // Static checks first (negligible cost).
    let s1 = check_redirect_uri_consistent(cfg);
    let s2 = check_web_redirect_uri_consistent(cfg);
    let s3 = check_https_in_production(cfg);

    // Network checks in parallel.
    let (n1, n2, n3) = tokio::join!(
        check_gitlab_url_reachable(cfg, http),
        check_oauth_token_endpoint_reachable(cfg, http),
        check_client_credentials_valid(cfg, http),
    );

    ValidationReport::from_checks(vec![n1, n2, n3, s1, s2, s3])
}

#[cfg(test)]
mod tests {
    use super::*;
    use akashic_domain::types::CheckStatus;
    use akashic_test_support::test_config_minimal;
    use serial_test::serial;

    #[test]
    fn check_status_worst_rollup() {
        use CheckStatus::*;
        assert_eq!(Ok.worst(Ok), Ok);
        assert_eq!(Ok.worst(Warn), Warn);
        assert_eq!(Warn.worst(Fail), Fail);
        assert_eq!(Fail.worst(Ok), Fail);
        assert_eq!(Warn.worst(Warn), Warn);
    }

    #[test]
    fn report_overall_rollup() {
        let r = ValidationReport::from_checks(vec![
            CheckResult::ok("a", "fine"),
            CheckResult::warn("b", "meh", None),
            CheckResult::ok("c", "fine"),
        ]);
        assert_eq!(r.overall, CheckStatus::Warn);
        assert_eq!(r.n_ok(), 2);
        assert_eq!(r.n_warn(), 1);
        assert_eq!(r.n_fail(), 0);
    }

    #[test]
    fn validation_mode_from_env_str() {
        assert_eq!(ValidationMode::from_env_str("off"), ValidationMode::Off);
        assert_eq!(
            ValidationMode::from_env_str("STRICT"),
            ValidationMode::Strict
        );
        assert_eq!(ValidationMode::from_env_str("warn"), ValidationMode::Warn);
        assert_eq!(ValidationMode::from_env_str(""), ValidationMode::Warn);
        assert_eq!(
            ValidationMode::from_env_str("garbage"),
            ValidationMode::Warn
        );
    }

    #[test]
    fn check_status_serializes_lowercase() {
        let v = serde_json::to_string(&CheckStatus::Fail).unwrap();
        assert_eq!(v, "\"fail\"");
        let v = serde_json::to_string(&CheckStatus::Ok).unwrap();
        assert_eq!(v, "\"ok\"");
    }

    fn cfg_with(public_base: &str, redirect: &str, web_redirect: &str) -> Config {
        let mut c = test_config_minimal();
        c.public_base_url = public_base.into();
        c.gitlab_redirect_uri = redirect.into();
        c.gitlab_web_redirect_uri = web_redirect.into();
        c
    }

    #[test]
    fn redirect_uri_consistent_ok_when_matches() {
        let c = cfg_with(
            "https://akashic.example.com",
            "https://akashic.example.com/auth/callback",
            "https://akashic.example.com/auth/web/callback",
        );
        let r = check_redirect_uri_consistent(&c);
        assert_eq!(r.status, CheckStatus::Ok);
    }

    #[test]
    fn redirect_uri_consistent_warn_on_mismatch() {
        let c = cfg_with(
            "https://akashic.example.com",
            "https://other.example.com/auth/callback",
            "https://akashic.example.com/auth/web/callback",
        );
        let r = check_redirect_uri_consistent(&c);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.remediation.is_some());
    }

    #[test]
    fn web_redirect_uri_consistent_warn_on_mismatch() {
        let c = cfg_with(
            "https://akashic.example.com",
            "https://akashic.example.com/auth/callback",
            "https://other.example.com/auth/web/callback",
        );
        let r = check_web_redirect_uri_consistent(&c);
        assert_eq!(r.status, CheckStatus::Warn);
    }

    #[test]
    #[serial]
    fn https_in_production_ok_when_not_prod() {
        let c = test_config_minimal();
        unsafe { std::env::remove_var("AKASHIC_ENV") };
        let r = check_https_in_production(&c);
        assert_eq!(r.status, CheckStatus::Ok);
    }

    #[test]
    #[serial]
    fn https_in_production_fail_on_http_in_prod() {
        let mut c = test_config_minimal();
        c.gitlab_url = "http://gitlab.acme.example.org".into();
        c.public_base_url = "https://akashic.example.com".into();
        c.frontend_url = "http://akashic.example.com".into();
        unsafe { std::env::set_var("AKASHIC_ENV", "production") };
        let r = check_https_in_production(&c);
        unsafe { std::env::remove_var("AKASHIC_ENV") };
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.detail.contains("GITLAB_URL"));
        assert!(r.detail.contains("FRONTEND_URL"));
        assert!(!r.detail.contains("PUBLIC_BASE_URL"));
    }

    use wiremock::matchers::{method as wm_method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cfg_pointing_at(server_uri: &str) -> Config {
        let mut c = test_config_minimal();
        c.gitlab_url = server_uri.to_string();
        c
    }

    #[tokio::test]
    async fn gitlab_url_reachable_ok_on_valid_response() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/api/v4/version"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"version": "16.11.4-ee"})),
            )
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let http = reqwest::Client::new();
        let r = check_gitlab_url_reachable(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Ok);
        assert!(r.detail.contains("16.11.4-ee"), "got: {}", r.detail);
    }

    #[tokio::test]
    async fn gitlab_url_reachable_fail_on_503() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/api/v4/version"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let http = reqwest::Client::new();
        let r = check_gitlab_url_reachable(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.detail.contains("503"));
    }

    #[tokio::test]
    async fn gitlab_url_reachable_fail_on_html_response() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/api/v4/version"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body>captive portal</body></html>")
                    .insert_header("content-type", "text/html"),
            )
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let http = reqwest::Client::new();
        let r = check_gitlab_url_reachable(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.detail.contains("not JSON"));
    }

    #[tokio::test]
    async fn gitlab_url_reachable_fail_on_connection_refused() {
        let cfg = {
            let mut c = test_config_minimal();
            c.gitlab_url = "http://127.0.0.1:1".into();
            c
        };
        let http = reqwest::Client::new();
        let r = check_gitlab_url_reachable(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.detail.starts_with("connect/transport error"));
    }

    #[tokio::test]
    async fn oauth_token_reachable_ok_on_oauth_error_shape() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/oauth/token"))
            .respond_with(ResponseTemplate::new(401).set_body_json(
                serde_json::json!({"error": "invalid_grant", "error_description": "..."}),
            ))
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let http = reqwest::Client::new();
        let r = check_oauth_token_endpoint_reachable(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Ok);
    }

    #[tokio::test]
    async fn oauth_token_reachable_fail_on_html() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html>nope</html>")
                    .insert_header("content-type", "text/html"),
            )
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let http = reqwest::Client::new();
        let r = check_oauth_token_endpoint_reachable(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn oauth_token_reachable_warn_on_5xx() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/oauth/token"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let http = reqwest::Client::new();
        let r = check_oauth_token_endpoint_reachable(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Warn);
    }

    #[tokio::test]
    async fn oauth_token_reachable_fail_on_connection_refused() {
        let mut cfg = test_config_minimal();
        cfg.gitlab_url = "http://127.0.0.1:1".into();
        let http = reqwest::Client::new();
        let r = check_oauth_token_endpoint_reachable(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn oauth_token_reachable_fail_on_json_without_error_field() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"message": "Unauthorized"})),
            ) // valid JSON, no `error` key
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let http = reqwest::Client::new();
        let r = check_oauth_token_endpoint_reachable(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.detail.contains("without `error` field"));
    }

    #[tokio::test]
    async fn client_credentials_valid_ok_on_200() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fake_app_token",
                "token_type": "Bearer",
                "expires_in": 7200
            })))
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let http = reqwest::Client::new();
        let r = check_client_credentials_valid(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Ok);
    }

    #[tokio::test]
    async fn client_credentials_valid_fail_on_invalid_client() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"error": "invalid_client"})),
            )
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let http = reqwest::Client::new();
        let r = check_client_credentials_valid(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.detail.contains("invalid_client"));
        assert!(r.remediation.is_some());
    }

    #[tokio::test]
    async fn client_credentials_valid_warn_on_unauthorized_client() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({"error": "unauthorized_client"})),
            )
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let http = reqwest::Client::new();
        let r = check_client_credentials_valid(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Warn);
    }

    #[tokio::test]
    async fn client_credentials_valid_warn_on_5xx() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/oauth/token"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let http = reqwest::Client::new();
        let r = check_client_credentials_valid(&cfg, &http).await;
        assert_eq!(r.status, CheckStatus::Warn);
    }

    #[tokio::test]
    async fn validate_runs_network_probes_in_parallel() {
        let server = MockServer::start().await;
        // Each path delays 1 second. If serial, total ~3s; if parallel, ~1s.
        Mock::given(wm_method("GET"))
            .and(wm_path("/api/v4/version"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(1))
                    .set_body_json(serde_json::json!({"version": "16.0.0"})),
            )
            .mount(&server)
            .await;

        // Both POST /oauth/token calls (token-endpoint-reachable + client-creds)
        // hit the same mock. The body is a deliberate compromise that makes
        // both checks happy: it has BOTH `access_token` (which makes
        // client_credentials_valid see 200 → Ok) AND `error: shadow` (which
        // makes oauth_token_endpoint_reachable see structured error → Ok).
        // What we care about for parallelism is wall-clock, not statuses.
        Mock::given(wm_method("POST"))
            .and(wm_path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(1))
                    .set_body_json(serde_json::json!({"access_token": "x", "error": "shadow"})),
            )
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let http = reqwest::Client::new();

        let start = std::time::Instant::now();
        let r = validate(&cfg, &http).await;
        let elapsed = start.elapsed();

        // Three parallel 1-second probes should complete in ~1s + overhead,
        // never close to 3s. Allow generous headroom for slow CI: < 2.5s.
        assert!(
            elapsed < Duration::from_millis(2500),
            "validate() took {elapsed:?} — network probes likely ran serially"
        );

        assert_eq!(r.checks.len(), 6);
    }
}
