use secrecy::ExposeSecret;
use std::env;

use crate::error::{ConfigError, Violation};
use crate::types::Config;

/// Forbidden placeholder values rejected when `AKASHIC_ENV=production`.
///
/// Centralizing the list here lets future tracks (B1, B5, etc.) extend it in
/// one place. Comparison is case-sensitive; the canonical placeholder is the
/// literal `changeme` string A1 established in `.env.example`.
pub const FORBIDDEN_PLACEHOLDERS: &[&str] = &[
    "changeme",
    "akashic_secret",
    "secret",
    "password",
    "admin",
    "https://gitlab.example.com",
    "http://gitlab.example.com",
    "sk-changeme",
    "sk-test",
    "your-api-key",
];

/// Returns true when `AKASHIC_ENV` is set to `production` or `prod`
/// (case-insensitive).
///
/// Any other value (including unset, empty, `dev`, `development`, `test`,
/// `ci`, `staging`) returns false. This is the single switch that gates
/// strict validation.
pub fn is_production() -> bool {
    match env::var("AKASHIC_ENV") {
        Ok(v) => {
            let lower = v.trim().to_ascii_lowercase();
            lower == "production" || lower == "prod"
        }
        Err(_) => false,
    }
}

/// Returns true when the process is running inside a container.
///
/// Detects via `/.dockerenv` (created by Docker Engine) or the
/// `AKASHIC_FORCE_CONTAINER=1` escape hatch (documented in `.env.example`
/// and used by the test suite). Non-Docker container runtimes (podman
/// without docker-compat, kubernetes with non-dockershim runtimes) need
/// the env var override; cgroup-parsing was deliberately not added (see
/// sub-spec R-3).
pub fn is_container() -> bool {
    if env::var("AKASHIC_FORCE_CONTAINER").as_deref() == Ok("1") {
        return true;
    }
    std::path::Path::new("/.dockerenv").exists()
}

fn check_empty(out: &mut Vec<Violation>, field: &'static str, value: &str) {
    if value.is_empty() {
        out.push(Violation::Empty { field });
    }
}

fn check_placeholder(out: &mut Vec<Violation>, field: &'static str, value: &str) {
    if FORBIDDEN_PLACEHOLDERS.contains(&value) {
        out.push(Violation::Placeholder {
            field,
            value: value.to_string(),
        });
    }
}

/// Returns true if `host` is a loopback host string we want to reject inside
/// a container.
///
/// For "special" URL schemes like `http`/`https`, `url::Url::host_str()`
/// returns the IPv6 loopback as `"::1"` *without* brackets. For non-special
/// schemes like `bolt:` or `postgres:` (which the WHATWG URL spec treats as
/// opaque-host schemes), `host_str()` preserves the bracketed form `"[::1]"`.
/// We accept both shapes.
fn is_localhost_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

impl Config {
    /// Validate that the config is production-ready.
    ///
    /// Collects **all** violations into a single error so the operator sees
    /// the full failure list at once instead of fixing one issue and
    /// re-running. Called automatically by `from_env` when `is_production()`
    /// returns true; exposed as a public method so tests can drive it
    /// directly.
    pub fn validate_for_production(&self) -> Result<(), ConfigError> {
        let mut violations: Vec<Violation> = Vec::new();
        let containerized = is_container();

        // ── Required-non-empty fields (AC-2) ─────────────────────────────
        check_empty(&mut violations, "GITLAB_APP_ID", &self.gitlab_app_id);
        check_empty(
            &mut violations,
            "GITLAB_APP_SECRET",
            self.gitlab_app_secret.expose_secret(),
        );
        check_empty(
            &mut violations,
            "NEO4J_PASSWORD",
            self.neo4j_password.expose_secret(),
        );
        check_empty(&mut violations, "DATABASE_URL", &self.database_url);
        check_empty(&mut violations, "FRONTEND_URL", &self.frontend_url);

        // ── Forbidden-placeholder fields (AC-3) ──────────────────────────
        check_placeholder(&mut violations, "GITLAB_URL", &self.gitlab_url);
        check_placeholder(
            &mut violations,
            "NEO4J_PASSWORD",
            self.neo4j_password.expose_secret(),
        );
        check_placeholder(
            &mut violations,
            "GITLAB_APP_SECRET",
            self.gitlab_app_secret.expose_secret(),
        );
        check_placeholder(&mut violations, "GITLAB_APP_ID", &self.gitlab_app_id);
        if let Some(s) = self.gitlab_webhook_secret.as_ref() {
            check_placeholder(&mut violations, "GITLAB_WEBHOOK_SECRET", s.expose_secret());
        }
        if let Some(s) = self.gitlab_service_token.as_ref() {
            check_placeholder(&mut violations, "GITLAB_SERVICE_TOKEN", s.expose_secret());
        }
        if let Some(s) = self.llm.api_key.as_ref() {
            check_placeholder(&mut violations, "LLM_API_KEY", s.expose_secret());
        }
        if let Some(s) = self.embedding.api_key.as_ref() {
            check_placeholder(&mut violations, "EMBEDDING_API_KEY", s.expose_secret());
        }

        // DATABASE_URL: re-parse and check the password segment against the
        // placeholder list. Skip the parse when the field is empty — the
        // empty case is already covered by the `check_empty` call above,
        // and parsing `""` would add a redundant `InvalidSecretShape`
        // violation that muddies the diagnostic.
        if !self.database_url.is_empty() {
            match url::Url::parse(&self.database_url) {
                Ok(u) => {
                    if let Some(pw) = u.password()
                        && FORBIDDEN_PLACEHOLDERS.contains(&pw)
                    {
                        violations.push(Violation::Placeholder {
                            field: "DATABASE_URL",
                            value: format!("password=\"{pw}\""),
                        });
                    }
                    // Container-localhost check (AC-4) for DATABASE_URL.
                    if containerized
                        && let Some(host) = u.host_str()
                        && is_localhost_host(host)
                    {
                        violations.push(Violation::ContainerLocalhost {
                            field: "DATABASE_URL",
                            host: host.to_string(),
                        });
                    }
                }
                Err(e) => violations.push(Violation::InvalidSecretShape {
                    field: "DATABASE_URL",
                    reason: e.to_string(),
                }),
            }
        }

        // ── Container-localhost check for NEO4J_URI (AC-4) ───────────────
        if containerized {
            match url::Url::parse(&self.neo4j_uri) {
                Ok(u) => {
                    if let Some(host) = u.host_str()
                        && is_localhost_host(host)
                    {
                        violations.push(Violation::ContainerLocalhost {
                            field: "NEO4J_URI",
                            host: host.to_string(),
                        });
                    }
                }
                Err(e) => violations.push(Violation::InvalidSecretShape {
                    field: "NEO4J_URI",
                    reason: e.to_string(),
                }),
            }
        }

        if violations.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::ValidationFailed { violations })
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for `Config::from_env`, `validate_for_production`,
    //! `is_production`, `is_container`, and the `Debug` redaction.
    //!
    //! All env-mutating tests in this module share the `config_env`
    //! `serial_test` group so they run sequentially and cannot race on
    //! `AKASHIC_ENV` / `AKASHIC_FORCE_CONTAINER`. Per the sub-spec AC-7,
    //! this is required for zero-flake CI behavior.

    use crate::*;
    use secrecy::SecretString;
    use serial_test::serial;
    use std::env;

    #[test]
    #[serial(config_env)]
    fn forbidden_placeholders_contains_changeme() {
        assert!(FORBIDDEN_PLACEHOLDERS.contains(&"changeme"));
        assert!(FORBIDDEN_PLACEHOLDERS.contains(&"akashic_secret"));
        assert!(FORBIDDEN_PLACEHOLDERS.contains(&"https://gitlab.example.com"));
    }

    #[test]
    #[serial(config_env)]
    fn is_production_recognizes_canonical_values() {
        let saved = env::var("AKASHIC_ENV").ok();

        unsafe { env::remove_var("AKASHIC_ENV") };
        assert!(!is_production(), "unset is not production");

        for v in ["dev", "development", "test", "ci", "staging", ""] {
            unsafe { env::set_var("AKASHIC_ENV", v) };
            assert!(!is_production(), "{v:?} is not production");
        }

        for v in [
            "production",
            "PRODUCTION",
            "Production",
            "prod",
            "PROD",
            " prod ",
        ] {
            unsafe { env::set_var("AKASHIC_ENV", v) };
            assert!(is_production(), "{v:?} is production");
        }

        match saved {
            Some(v) => unsafe { env::set_var("AKASHIC_ENV", v) },
            None => unsafe { env::remove_var("AKASHIC_ENV") },
        }
    }

    #[test]
    #[serial(config_env)]
    fn debug_redacts_all_secret_fields() {
        // Build a Config with values that would obviously leak if Debug
        // expanded them. We don't need a full `from_env` here — just
        // construct one directly from struct literals.
        let cfg = Config {
            neo4j_uri: "bolt://neo4j:7687".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: SecretString::from("CANARY-NEO4J-PASSWORD".to_string()),
            database_url: format!(
                "postgres://akashic:{}@postgres:5432/akashic",
                "CANARY-DB-PW"
            ),
            embedding: EmbeddingConfig {
                provider: AiProvider::Local,
                api_key: Some(SecretString::from("CANARY-EMBED-KEY".to_string())),
                model: "m".into(),
                base_url: None,
            },
            llm: LlmConfig {
                provider: AiProvider::Local,
                api_key: Some(SecretString::from("CANARY-LLM-KEY".to_string())),
                model: "m".into(),
                base_url: None,
            },
            alerts: AlertsConfig::default(),
            module_max_files: 12,
            module_min_files: 3,
            mcp_sse_host: "0.0.0.0".into(),
            mcp_sse_port: 8080,
            gitlab_webhook_secret: Some(SecretString::from("CANARY-WEBHOOK".to_string())),
            gitlab_url: "https://gitlab.example.com".into(),
            gitlab_app_id: "id".into(),
            gitlab_app_secret: SecretString::from("CANARY-APP-SECRET".to_string()),
            gitlab_redirect_uri: "x".into(),
            gitlab_web_redirect_uri: "x".into(),
            auth_code_ttl_secs: 300,
            api_key_ttl_secs: 86400,
            gitlab_service_token: Some(SecretString::from("CANARY-SERVICE-TOKEN".to_string())),
            frontend_url: "x".into(),
            public_base_url: "http://localhost:8081".into(),
            cors_extra_origins: vec![],
            cookie_secure: true,
            api_port: 8081,
            ingest_clone_dir: "x".into(),
            ingest_max_file_size: 0,
            ingest_max_lines: 0,
            ingest_chunk_max_size: 0,
            ingest_concurrent_jobs: 1,
            ingest_skip_patterns: vec![],
            ingest_presets_path: None,
            ingest_completeness_threshold: 1.0,
            admin_users: vec![],
            ingest_crawl_max_pages: 0,
            ingest_crawl_delay_ms: 0,
            embedding_precision: EmbeddingPrecision::Float32,
            rate_limit_enabled: true,
            rate_limit_trusted_proxies: vec![],
            rate_limit_allowlist: vec![],
            mcp_quota_tokens_per_window: 100_000,
            mcp_quota_window_secs: 3600,
            mcp_quota_enabled: true,
            mcp_passthrough_user_cache_ttl_secs: 60,
            oauth_validation_mode: "warn".into(),
            migrate_on_boot: "auto".into(),
            ingest_quota_tokens_per_window: 5_000_000,
            ingest_quota_window_secs: 3600,
            ingest_quota_enabled: false,
        };
        let dbg = format!("{cfg:?}");
        for canary in [
            "CANARY-NEO4J-PASSWORD",
            "CANARY-DB-PW",
            "CANARY-EMBED-KEY",
            "CANARY-LLM-KEY",
            "CANARY-WEBHOOK",
            "CANARY-APP-SECRET",
            "CANARY-SERVICE-TOKEN",
        ] {
            assert!(
                !dbg.contains(canary),
                "Debug leaked secret-shaped value containing {canary:?}: {dbg}"
            );
        }
        // The non-secret URL host should still be visible.
        assert!(dbg.contains("postgres:5432"));
    }

    /// Helper: set a complete, *valid* prod-shaped env.
    ///
    /// Tests then mutate one variable to reproduce a single violation and
    /// assert that exactly that violation fires.
    fn set_valid_prod_env() {
        unsafe { env::set_var("AKASHIC_ENV", "production") };
        unsafe { env::set_var("NEO4J_URI", "bolt://neo4j:7687") };
        unsafe { env::set_var("NEO4J_USER", "neo4j") };
        unsafe { env::set_var("NEO4J_PASSWORD", "S3cure-Pa55w0rd-X9!") };
        unsafe {
            env::set_var(
                "DATABASE_URL",
                format!(
                    "postgres://akashic:{}@postgres:5432/akashic",
                    "S3cure-Pa55w0rd-Y!"
                ),
            )
        };
        unsafe { env::set_var("GITLAB_URL", "https://gitlab.acme.example.org") };
        unsafe { env::set_var("GITLAB_APP_ID", "real-app-id-abcdef") };
        unsafe { env::set_var("GITLAB_APP_SECRET", "real-app-secret-S3cure!") };
        unsafe { env::set_var("FRONTEND_URL", "https://akashic.acme.example.org") };
        unsafe { env::remove_var("GITLAB_WEBHOOK_SECRET") };
        unsafe { env::remove_var("GITLAB_SERVICE_TOKEN") };
        unsafe { env::remove_var("EMBEDDING_API_KEY") };
        unsafe { env::remove_var("LLM_API_KEY") };
        unsafe { env::remove_var("AKASHIC_FORCE_CONTAINER") };
    }

    fn clear_prod_env() {
        for v in [
            "AKASHIC_ENV",
            "NEO4J_URI",
            "NEO4J_USER",
            "NEO4J_PASSWORD",
            "DATABASE_URL",
            "GITLAB_URL",
            "GITLAB_APP_ID",
            "GITLAB_APP_SECRET",
            "FRONTEND_URL",
            "GITLAB_WEBHOOK_SECRET",
            "GITLAB_SERVICE_TOKEN",
            "EMBEDDING_API_KEY",
            "LLM_API_KEY",
            "AKASHIC_FORCE_CONTAINER",
        ] {
            unsafe { env::remove_var(v) };
        }
    }

    #[test]
    #[serial(config_env)]
    fn valid_prod_config_passes() {
        set_valid_prod_env();
        let cfg = Config::from_env().expect("valid prod config should load");
        cfg.validate_for_production()
            .expect("valid prod config should pass validation");
        clear_prod_env();
    }

    #[test]
    #[serial(config_env)]
    fn empty_gitlab_app_id_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("GITLAB_APP_ID", "") };
        let cfg = Config::from_env();
        clear_prod_env();
        let err = cfg.expect_err("empty GITLAB_APP_ID should fail validation");
        match err {
            ConfigError::ValidationFailed { violations } => {
                assert!(
                    violations.iter().any(|v| matches!(
                        v,
                        Violation::Empty {
                            field: "GITLAB_APP_ID"
                        }
                    )),
                    "expected Empty(GITLAB_APP_ID), got {violations:?}"
                );
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[test]
    #[serial(config_env)]
    fn empty_gitlab_app_secret_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("GITLAB_APP_SECRET", "") };
        let cfg = Config::from_env();
        clear_prod_env();
        let err = cfg.expect_err("empty GITLAB_APP_SECRET should fail validation");
        match err {
            ConfigError::ValidationFailed { violations } => {
                assert!(
                    violations.iter().any(|v| matches!(
                        v,
                        Violation::Empty {
                            field: "GITLAB_APP_SECRET"
                        }
                    )),
                    "expected Empty(GITLAB_APP_SECRET), got {violations:?}"
                );
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[test]
    #[serial(config_env)]
    fn empty_neo4j_password_fails() {
        // NEO4J_PASSWORD is required at env-read time in from_env; setting
        // it to "" is the relevant prod-validation case.
        set_valid_prod_env();
        unsafe { env::set_var("NEO4J_PASSWORD", "") };
        let cfg = Config::from_env();
        clear_prod_env();
        let err = cfg.expect_err("empty NEO4J_PASSWORD should fail validation");
        match err {
            ConfigError::ValidationFailed { violations } => {
                assert!(
                    violations.iter().any(|v| matches!(
                        v,
                        Violation::Empty {
                            field: "NEO4J_PASSWORD"
                        }
                    )),
                    "expected Empty(NEO4J_PASSWORD), got {violations:?}"
                );
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[test]
    #[serial(config_env)]
    fn empty_database_url_fails() {
        // Design choice: `env::var` returns `Ok("")` for an empty-but-set
        // variable, so `from_env` does NOT raise `MissingEnv` here — empty
        // and unset are deliberately distinct error shapes for operator
        // clarity ("X is empty" vs "X is unset"). This test pins the empty
        // case to `Violation::Empty`; a separate semantics test would
        // cover the unset/`MissingEnv` path.
        set_valid_prod_env();
        unsafe { env::set_var("DATABASE_URL", "") };
        let cfg = Config::from_env();
        clear_prod_env();
        let err = cfg.expect_err("empty DATABASE_URL should fail validation");
        match err {
            ConfigError::ValidationFailed { violations } => {
                assert!(
                    violations.iter().any(|v| matches!(
                        v,
                        Violation::Empty {
                            field: "DATABASE_URL"
                        }
                    )),
                    "expected Empty(DATABASE_URL), got {violations:?}"
                );
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[test]
    #[serial(config_env)]
    fn empty_frontend_url_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("FRONTEND_URL", "") };
        let cfg = Config::from_env();
        clear_prod_env();
        let err = cfg.expect_err("empty FRONTEND_URL should fail validation");
        match err {
            ConfigError::ValidationFailed { violations } => {
                assert!(
                    violations.iter().any(|v| matches!(
                        v,
                        Violation::Empty {
                            field: "FRONTEND_URL"
                        }
                    )),
                    "expected Empty(FRONTEND_URL), got {violations:?}"
                );
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    fn assert_placeholder_violation(err: ConfigError, expected_field: &'static str) {
        match err {
            ConfigError::ValidationFailed { violations } => {
                assert!(
                    violations.iter().any(|v| matches!(
                        v,
                        Violation::Placeholder { field, .. } if *field == expected_field
                    )),
                    "expected Placeholder({expected_field}), got {violations:?}"
                );
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[test]
    #[serial(config_env)]
    fn placeholder_gitlab_url_https_example_com_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("GITLAB_URL", "https://gitlab.example.com") };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_placeholder_violation(cfg.unwrap_err(), "GITLAB_URL");
    }

    #[test]
    #[serial(config_env)]
    fn placeholder_gitlab_url_http_example_com_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("GITLAB_URL", "http://gitlab.example.com") };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_placeholder_violation(cfg.unwrap_err(), "GITLAB_URL");
    }

    #[test]
    #[serial(config_env)]
    fn placeholder_neo4j_password_akashic_secret_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("NEO4J_PASSWORD", "akashic_secret") };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_placeholder_violation(cfg.unwrap_err(), "NEO4J_PASSWORD");
    }

    #[test]
    #[serial(config_env)]
    fn placeholder_neo4j_password_changeme_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("NEO4J_PASSWORD", "changeme") };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_placeholder_violation(cfg.unwrap_err(), "NEO4J_PASSWORD");
    }

    #[test]
    #[serial(config_env)]
    fn placeholder_gitlab_app_secret_changeme_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("GITLAB_APP_SECRET", "changeme") };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_placeholder_violation(cfg.unwrap_err(), "GITLAB_APP_SECRET");
    }

    #[test]
    #[serial(config_env)]
    fn placeholder_gitlab_app_id_changeme_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("GITLAB_APP_ID", "changeme") };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_placeholder_violation(cfg.unwrap_err(), "GITLAB_APP_ID");
    }

    #[test]
    #[serial(config_env)]
    fn placeholder_gitlab_webhook_secret_changeme_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("GITLAB_WEBHOOK_SECRET", "changeme") };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_placeholder_violation(cfg.unwrap_err(), "GITLAB_WEBHOOK_SECRET");
    }

    #[test]
    #[serial(config_env)]
    fn placeholder_llm_api_key_sk_test_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("LLM_API_KEY", "sk-test") };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_placeholder_violation(cfg.unwrap_err(), "LLM_API_KEY");
    }

    #[test]
    #[serial(config_env)]
    fn placeholder_database_url_password_segment_changeme_fails() {
        set_valid_prod_env();
        unsafe {
            env::set_var(
                "DATABASE_URL",
                format!("postgres://akashic:{}@postgres:5432/akashic", "changeme"),
            )
        };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_placeholder_violation(cfg.unwrap_err(), "DATABASE_URL");
    }

    fn assert_container_localhost(err: ConfigError, expected_field: &'static str) {
        match err {
            ConfigError::ValidationFailed { violations } => {
                assert!(
                    violations.iter().any(|v| matches!(
                        v,
                        Violation::ContainerLocalhost { field, .. } if *field == expected_field
                    )),
                    "expected ContainerLocalhost({expected_field}), got {violations:?}"
                );
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[test]
    #[serial(config_env)]
    fn database_url_localhost_in_container_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("AKASHIC_FORCE_CONTAINER", "1") };
        unsafe {
            env::set_var(
                "DATABASE_URL",
                format!(
                    "postgres://akashic:{}@localhost:5432/akashic",
                    "S3cure-Pa55w0rd-Y!"
                ),
            )
        };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_container_localhost(cfg.unwrap_err(), "DATABASE_URL");
    }

    #[test]
    #[serial(config_env)]
    fn database_url_127_in_container_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("AKASHIC_FORCE_CONTAINER", "1") };
        unsafe {
            env::set_var(
                "DATABASE_URL",
                format!(
                    "postgres://akashic:{}@127.0.0.1:5432/akashic",
                    "S3cure-Pa55w0rd-Y!"
                ),
            )
        };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_container_localhost(cfg.unwrap_err(), "DATABASE_URL");
    }

    #[test]
    #[serial(config_env)]
    fn database_url_ipv6_loopback_in_container_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("AKASHIC_FORCE_CONTAINER", "1") };
        unsafe {
            env::set_var(
                "DATABASE_URL",
                format!(
                    "postgres://akashic:{}@[::1]:5432/akashic",
                    "S3cure-Pa55w0rd-Y!"
                ),
            )
        };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_container_localhost(cfg.unwrap_err(), "DATABASE_URL");
    }

    #[test]
    #[serial(config_env)]
    fn neo4j_uri_localhost_in_container_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("AKASHIC_FORCE_CONTAINER", "1") };
        unsafe { env::set_var("NEO4J_URI", "bolt://localhost:7687") };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_container_localhost(cfg.unwrap_err(), "NEO4J_URI");
    }

    #[test]
    #[serial(config_env)]
    fn neo4j_uri_127_in_container_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("AKASHIC_FORCE_CONTAINER", "1") };
        unsafe { env::set_var("NEO4J_URI", "bolt://127.0.0.1:7687") };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_container_localhost(cfg.unwrap_err(), "NEO4J_URI");
    }

    #[test]
    #[serial(config_env)]
    fn neo4j_uri_ipv6_loopback_in_container_fails() {
        set_valid_prod_env();
        unsafe { env::set_var("AKASHIC_FORCE_CONTAINER", "1") };
        unsafe { env::set_var("NEO4J_URI", "bolt://[::1]:7687") };
        let cfg = Config::from_env();
        clear_prod_env();
        assert_container_localhost(cfg.unwrap_err(), "NEO4J_URI");
    }

    #[test]
    #[serial(config_env)]
    fn non_prod_loads_but_explicit_validate_still_errors() {
        // AKASHIC_ENV unset: every dev placeholder is tolerated.
        clear_prod_env();
        unsafe { env::set_var("NEO4J_URI", "bolt://localhost:7687") };
        unsafe { env::set_var("NEO4J_USER", "neo4j") };
        unsafe { env::set_var("NEO4J_PASSWORD", "akashic_secret") };
        unsafe {
            env::set_var(
                "DATABASE_URL",
                format!(
                    "postgres://akashic:{}@localhost:5433/akashic",
                    "akashic_secret"
                ),
            )
        };
        unsafe { env::set_var("GITLAB_URL", "https://gitlab.example.com") };
        unsafe { env::set_var("GITLAB_APP_ID", "") };
        unsafe { env::set_var("GITLAB_APP_SECRET", "") };
        unsafe { env::set_var("FRONTEND_URL", "http://localhost:3000") };

        let cfg = Config::from_env().expect("non-prod should accept dev placeholders");
        // Sanity: validate_for_production still returns Err if called
        // explicitly, but from_env doesn't call it without AKASHIC_ENV.
        assert!(cfg.validate_for_production().is_err());

        clear_prod_env();
    }

    #[test]
    #[serial(config_env)]
    fn localhost_dsn_outside_container_passes() {
        // Without AKASHIC_FORCE_CONTAINER and (typically) without
        // /.dockerenv on the test runner, localhost must NOT be rejected.
        set_valid_prod_env();
        unsafe { env::remove_var("AKASHIC_FORCE_CONTAINER") };
        unsafe {
            env::set_var(
                "DATABASE_URL",
                format!(
                    "postgres://akashic:{}@localhost:5432/akashic",
                    "S3cure-Pa55w0rd-Y!"
                ),
            )
        };
        unsafe { env::set_var("NEO4J_URI", "bolt://localhost:7687") };
        let result = Config::from_env();
        clear_prod_env();

        // Skip if the test runner is itself inside a container with
        // /.dockerenv present (rare but possible in CI). The test asserts
        // its own preconditions before validating behavior.
        if std::path::Path::new("/.dockerenv").exists() {
            eprintln!("test skipped: /.dockerenv exists on this runner");
            return;
        }

        result.expect("localhost DSN should pass when not containerized");
    }

    /// Regression: `Config::is_production` (the method) must agree with the
    /// free `is_production` fn on every input. The method used to compare the
    /// raw `AKASHIC_ENV` value without trim/lowercase, so non-canonical
    /// casing/whitespace diverged between the two prod-detection paths. Both
    /// now delegate to the same helper.
    #[test]
    #[serial(config_env)]
    fn method_and_free_is_production_agree() {
        let saved = env::var("AKASHIC_ENV").ok();

        // A throwaway Config; `is_production` reads the env var, not `&self`,
        // so the field values are irrelevant to this test.
        let cfg = Config {
            neo4j_uri: "bolt://neo4j:7687".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: SecretString::from("pw".to_string()),
            database_url: "postgres://localhost:5432/akashic".into(),
            embedding: EmbeddingConfig {
                provider: AiProvider::Local,
                api_key: None,
                model: "m".into(),
                base_url: None,
            },
            llm: LlmConfig {
                provider: AiProvider::Local,
                api_key: None,
                model: "m".into(),
                base_url: None,
            },
            alerts: AlertsConfig::default(),
            module_max_files: 12,
            module_min_files: 3,
            mcp_sse_host: "0.0.0.0".into(),
            mcp_sse_port: 8080,
            gitlab_webhook_secret: None,
            gitlab_url: "https://gitlab.acme.example.org".into(),
            gitlab_app_id: "id".into(),
            gitlab_app_secret: SecretString::from("s".to_string()),
            gitlab_redirect_uri: "x".into(),
            gitlab_web_redirect_uri: "x".into(),
            auth_code_ttl_secs: 300,
            api_key_ttl_secs: 86400,
            gitlab_service_token: None,
            frontend_url: "x".into(),
            public_base_url: "http://localhost:8081".into(),
            cors_extra_origins: vec![],
            cookie_secure: true,
            api_port: 8081,
            ingest_clone_dir: "x".into(),
            ingest_max_file_size: 0,
            ingest_max_lines: 0,
            ingest_chunk_max_size: 0,
            ingest_concurrent_jobs: 1,
            ingest_skip_patterns: vec![],
            ingest_presets_path: None,
            ingest_completeness_threshold: 1.0,
            admin_users: vec![],
            ingest_crawl_max_pages: 0,
            ingest_crawl_delay_ms: 0,
            embedding_precision: EmbeddingPrecision::Float32,
            rate_limit_enabled: true,
            rate_limit_trusted_proxies: vec![],
            rate_limit_allowlist: vec![],
            mcp_quota_tokens_per_window: 100_000,
            mcp_quota_window_secs: 3600,
            mcp_quota_enabled: true,
            mcp_passthrough_user_cache_ttl_secs: 60,
            oauth_validation_mode: "warn".into(),
            migrate_on_boot: "auto".into(),
            ingest_quota_tokens_per_window: 5_000_000,
            ingest_quota_window_secs: 3600,
            ingest_quota_enabled: false,
        };

        unsafe { env::remove_var("AKASHIC_ENV") };
        assert_eq!(cfg.is_production(), is_production(), "unset");

        // The exact non-canonical inputs called out in the finding, plus the
        // canonical and clearly-non-prod cases.
        for v in [
            " Production ",
            "PRODUCTION",
            "production",
            "Production",
            " prod ",
            "PROD",
            "prod",
            "dev",
            "staging",
            "",
        ] {
            unsafe { env::set_var("AKASHIC_ENV", v) };
            assert_eq!(
                cfg.is_production(),
                is_production(),
                "method and free fn disagree for {v:?}"
            );
        }

        // Spot-check the *value* for the finding's headline cases: all three
        // should now be detected as production by both paths.
        for v in [" Production ", "PRODUCTION", "production"] {
            unsafe { env::set_var("AKASHIC_ENV", v) };
            assert!(cfg.is_production(), "{v:?} should be production (method)");
            assert!(is_production(), "{v:?} should be production (free fn)");
        }

        match saved {
            Some(v) => unsafe { env::set_var("AKASHIC_ENV", v) },
            None => unsafe { env::remove_var("AKASHIC_ENV") },
        }
    }

    #[test]
    fn dsn_roundtrip_no_drift() {
        // R-8: parsing a standard DSN with `url::Url` and re-emitting it
        // must produce the input verbatim. If a future `url` crate update
        // changes the encoding, this test pins the regression.
        let dsn = format!(
            "postgres://akashic:{}@postgres:5432/akashic",
            "S3cure-Pa55w0rd-Y"
        );
        let u = url::Url::parse(&dsn).expect("parse");
        let emitted = u.to_string();
        assert_eq!(emitted, dsn, "DSN round-trip drift");
    }

    #[test]
    #[serial(config_env)]
    fn validation_failure_diagnostic_shape() {
        let err = ConfigError::ValidationFailed {
            violations: vec![
                Violation::Empty {
                    field: "GITLAB_APP_ID",
                },
                Violation::Placeholder {
                    field: "GITLAB_URL",
                    value: "https://gitlab.example.com".into(),
                },
                Violation::Placeholder {
                    field: "NEO4J_PASSWORD",
                    value: "akashic_secret".into(),
                },
                Violation::ContainerLocalhost {
                    field: "DATABASE_URL",
                    host: "localhost".into(),
                },
            ],
        };
        let s = format!("{err}");
        // `Display` opens with a leading newline so the diagnostic block
        // is visually separated from any prior stderr output.
        assert!(
            s.starts_with('\n'),
            "diagnostic should begin with a newline: {s:?}"
        );
        assert!(s.contains("Akashic Record: production config validation failed"));
        assert!(s.contains("AKASHIC_ENV=production"));
        assert!(s.contains("GITLAB_APP_ID: empty (required in production)"));
        assert!(
            s.contains("GITLAB_URL: forbidden placeholder value \"https://gitlab.example.com\"")
        );
        assert!(s.contains("NEO4J_PASSWORD: forbidden placeholder value \"akashic_secret\""));
        assert!(
            s.contains("DATABASE_URL: contains \"localhost\" while running inside a container")
        );
        assert!(s.contains(".env.example"));
        assert!(s.contains("docs/operations/rotation-sop.md"));
        assert!(s.trim_end().ends_with("Aborting startup."));
    }
}
