use anyhow::Context;
use ipnet::IpNet;
use secrecy::SecretString;
use std::env;

use crate::error::ConfigError;
use crate::types::{
    AiProvider, AlertsConfig, Config, EmbeddingConfig, EmbeddingPrecision, LlmConfig,
};
use crate::validate::is_production;

/// Parse a comma-separated list of GitLab usernames. Trims whitespace and
/// drops empty entries. Empty string → empty vec.
/// Used for `AKASHIC_ADMIN_USERS`.
pub(crate) fn parse_admin_users(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse a comma-separated list of CIDR entries. Empty string → empty vec.
/// Used for `RATE_LIMIT_TRUSTED_PROXIES` and `RATE_LIMIT_ALLOWLIST`.
fn parse_cidr_list(s: &str) -> anyhow::Result<Vec<IpNet>> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            p.parse::<IpNet>()
                .with_context(|| format!("CIDR-parse failed for entry '{p}'"))
        })
        .collect()
}

impl Config {
    /// Load configuration from environment variables (`.env` file supported via dotenvy).
    pub fn from_env() -> Result<Self, ConfigError> {
        dotenvy::dotenv().ok(); // ignore missing .env

        let skip_patterns = env::var("INGEST_SKIP_PATTERNS")
            .unwrap_or_else(|_| "node_modules,vendor,dist,build,.git,__pycache__".into())
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();

        let admin_users = parse_admin_users(&env::var("AKASHIC_ADMIN_USERS").unwrap_or_default());

        let rate_limit_enabled = env::var("RATE_LIMIT_ENABLED")
            .map(|v| !matches!(v.to_lowercase().as_str(), "false" | "0" | "no" | "off"))
            .unwrap_or(true);

        let trusted_proxies_raw = env::var("RATE_LIMIT_TRUSTED_PROXIES").unwrap_or_else(|_| {
            "127.0.0.0/8,::1/128,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16".to_string()
        });
        let rate_limit_trusted_proxies =
            parse_cidr_list(&trusted_proxies_raw).map_err(|e| ConfigError::ParseError {
                var: "RATE_LIMIT_TRUSTED_PROXIES",
                value: trusted_proxies_raw.clone(),
                source: format!("{e:#}"),
            })?;

        let allowlist_raw = env::var("RATE_LIMIT_ALLOWLIST").unwrap_or_default();
        let rate_limit_allowlist =
            parse_cidr_list(&allowlist_raw).map_err(|e| ConfigError::ParseError {
                var: "RATE_LIMIT_ALLOWLIST",
                value: allowlist_raw.clone(),
                source: format!("{e:#}"),
            })?;

        let cfg = Self {
            neo4j_uri: env::var("NEO4J_URI").unwrap_or_else(|_| "bolt://localhost:7687".into()),
            neo4j_user: env::var("NEO4J_USER").unwrap_or_else(|_| "neo4j".into()),
            neo4j_password: SecretString::from(env::var("NEO4J_PASSWORD").map_err(|_| {
                ConfigError::MissingEnv {
                    var: "NEO4J_PASSWORD",
                }
            })?),

            database_url: env::var("DATABASE_URL").map_err(|_| ConfigError::MissingEnv {
                var: "DATABASE_URL",
            })?,

            embedding: EmbeddingConfig {
                provider: AiProvider::from_env(
                    &env::var("EMBEDDING_PROVIDER").unwrap_or_else(|_| "local".into()),
                ),
                api_key: env::var("EMBEDDING_API_KEY").ok().map(SecretString::from),
                model: env::var("EMBEDDING_MODEL")
                    .unwrap_or_else(|_| "text-embedding-3-small".into()),
                base_url: env::var("EMBEDDING_BASE_URL").ok(),
            },

            llm: LlmConfig {
                provider: AiProvider::from_env(
                    &env::var("LLM_PROVIDER").unwrap_or_else(|_| "local".into()),
                ),
                api_key: env::var("LLM_API_KEY").ok().map(SecretString::from),
                model: env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-5-nano".into()),
                base_url: env::var("LLM_BASE_URL").ok(),
            },

            alerts: AlertsConfig {
                webhook_url: env::var("ALERTS_WEBHOOK_URL")
                    .ok()
                    .filter(|s| !s.is_empty()),
                tick_secs: env::var("ALERTS_TICK_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(60),
                cooldown_secs: env::var("ALERTS_COOLDOWN_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(300),
            },

            module_max_files: env::var("MODULE_MAX_FILES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(12),

            module_min_files: env::var("MODULE_MIN_FILES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),

            api_host: env::var("API_HOST").unwrap_or_else(|_| "0.0.0.0".into()),

            gitlab_webhook_secret: env::var("GITLAB_WEBHOOK_SECRET")
                .ok()
                .map(SecretString::from),

            gitlab_url: env::var("GITLAB_URL")
                .unwrap_or_else(|_| "https://gitlab.example.com".into()),
            gitlab_app_id: env::var("GITLAB_APP_ID").unwrap_or_default(),
            gitlab_app_secret: SecretString::from(
                env::var("GITLAB_APP_SECRET").unwrap_or_default(),
            ),
            gitlab_redirect_uri: env::var("GITLAB_REDIRECT_URI")
                .unwrap_or_else(|_| "http://localhost:8081/auth/callback".into()),
            gitlab_web_redirect_uri: env::var("GITLAB_WEB_REDIRECT_URI")
                .unwrap_or_else(|_| "http://localhost:8081/auth/web/callback".into()),
            auth_code_ttl_secs: env::var("AUTH_CODE_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            api_key_ttl_secs: env::var("API_KEY_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(86400),

            gitlab_service_token: env::var("GITLAB_SERVICE_TOKEN")
                .ok()
                .map(SecretString::from),

            frontend_url: env::var("FRONTEND_URL")
                .unwrap_or_else(|_| "http://localhost:3000".into()),

            public_base_url: env::var("PUBLIC_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:8081".into()),

            cors_extra_origins: env::var("CORS_EXTRA_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            cookie_secure: env::var("COOKIE_SECURE")
                .map(|v| v != "false" && v != "0")
                .unwrap_or(true),

            api_port: env::var("API_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8081),

            ingest_clone_dir: env::var("INGEST_CLONE_DIR")
                .unwrap_or_else(|_| "/tmp/akashic-ingest".into()),
            ingest_max_file_size: env::var("INGEST_MAX_FILE_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(102_400),
            ingest_max_lines: env::var("INGEST_MAX_LINES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2000),
            ingest_chunk_max_size: env::var("INGEST_CHUNK_MAX_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5120),
            ingest_concurrent_jobs: env::var("INGEST_CONCURRENT_JOBS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            ingest_skip_patterns: skip_patterns,
            ingest_presets_path: env::var("AKASHIC_PRESETS_PATH").ok(),
            ingest_completeness_threshold: env::var("INGEST_COMPLETENESS_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0),
            admin_users,
            ingest_crawl_max_pages: env::var("INGEST_CRAWL_MAX_PAGES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(100),
            ingest_crawl_delay_ms: env::var("INGEST_CRAWL_DELAY_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(200),
            embedding_precision: EmbeddingPrecision::from_env(
                &env::var("EMBEDDING_PRECISION").unwrap_or_else(|_| "float32".into()),
            ),

            rate_limit_enabled,
            rate_limit_trusted_proxies,
            rate_limit_allowlist,

            mcp_quota_tokens_per_window: env::var("MCP_QUOTA_TOKENS_PER_WINDOW")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100_000),
            mcp_quota_window_secs: env::var("MCP_QUOTA_WINDOW_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3600),
            mcp_quota_enabled: env::var("MCP_QUOTA_ENABLED")
                .ok()
                .is_none_or(|s| matches!(s.as_str(), "true" | "1" | "yes")),

            mcp_passthrough_user_cache_ttl_secs: env::var("MCP_PASSTHROUGH_USER_CACHE_TTL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(60),

            oauth_validation_mode: env::var("OAUTH_VALIDATION_MODE")
                .unwrap_or_else(|_| "warn".into()),

            migrate_on_boot: env::var("MIGRATE_ON_BOOT").unwrap_or_default(),

            ingest_quota_tokens_per_window: env::var("INGEST_QUOTA_TOKENS_PER_WINDOW")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5_000_000),
            ingest_quota_window_secs: env::var("INGEST_QUOTA_WINDOW_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3600),
            ingest_quota_enabled: env::var("INGEST_QUOTA_ENABLED")
                .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
                .unwrap_or(false),

            // Same bool-parsing convention as `ingest_quota_enabled` above
            // (case-insensitive true/1/yes, default false).
            mcp_cimd_allow_loopback: env::var("MCP_CIMD_ALLOW_LOOPBACK")
                .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
                .unwrap_or(false),
        };

        if is_production() {
            cfg.validate_for_production()?;
        }

        Ok(cfg)
    }
}

#[cfg(test)]
mod admin_users_tests {
    use super::*;

    #[test]
    fn parse_admin_users_splits_trims_and_drops_empties() {
        assert_eq!(parse_admin_users("tonyhu"), vec!["tonyhu".to_string()]);
        assert_eq!(
            parse_admin_users(" tonyhu , alice "),
            vec!["tonyhu".to_string(), "alice".to_string()]
        );
        assert!(parse_admin_users("").is_empty());
        assert!(parse_admin_users("  , ,").is_empty());
    }
}

#[cfg(test)]
mod rate_limit_config_tests {
    use super::*;

    #[test]
    fn parse_cidr_list_accepts_well_formed_entries() {
        let parsed = parse_cidr_list("127.0.0.0/8,::1/128,10.0.0.0/8").unwrap();
        assert_eq!(parsed.len(), 3);
    }

    #[test]
    fn parse_cidr_list_rejects_garbage_with_named_var() {
        let err = parse_cidr_list("not-a-cidr").unwrap_err().to_string();
        assert!(
            err.contains("not-a-cidr"),
            "error must name the bad entry; got: {err}"
        );
    }

    #[test]
    fn parse_cidr_list_treats_empty_as_empty_vec() {
        let parsed = parse_cidr_list("").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_cidr_list_trims_whitespace_around_entries() {
        let parsed = parse_cidr_list(" 127.0.0.0/8 , ::1/128 ").unwrap();
        assert_eq!(parsed.len(), 2);
    }
}
