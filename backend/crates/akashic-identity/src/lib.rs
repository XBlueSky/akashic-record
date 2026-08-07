//! `akashic-identity` — core auth: token/session store + OAuth identity. The
//! quota cross-cut was carved into `akashic-quota` (Slice B). HTTP handler
//! files (account, exchange, middleware, oauth, oauth_device, web) live in
//! `akashic-http`; `mcp_middleware` lives in `akashic-mcp`.

pub mod services;
pub mod store;
pub mod types;

pub use store::AuthStore;

use axum::http::HeaderMap;

/// Extract the `ak_session` cookie value from a request's headers.
///
/// Canonical parser shared by every session-cookie consumer
/// (`middleware::require_auth`, `oauth_device::extract_session_user`,
/// `web`, `api::routes::health_oauth`) so the cookie format is parsed in
/// exactly one place. Returns None if the header is absent, non-UTF-8, or
/// carries no non-empty `ak_session=` pair.
pub fn session_cookie_value(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get("cookie")?.to_str().ok()?;
    cookie_header.split(';').find_map(|part| {
        let val = part.trim().strip_prefix("ak_session=")?.trim();
        (!val.is_empty()).then(|| val.to_string())
    })
}

#[cfg(test)]
pub mod test_support {
    use akashic_config::{
        AiProvider, AlertsConfig, Config, EmbeddingConfig, EmbeddingPrecision, LlmConfig,
    };
    use secrecy::SecretString;

    /// Minimal `Config` for tests — mirrors the fixture in `akashic-server`'s
    /// `auth::test_support::test_config_minimal`. All required fields
    /// populated; overrideable per-test via field assignment.
    pub fn test_config_minimal() -> Config {
        Config {
            neo4j_uri: "bolt://localhost:7687".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: SecretString::from("akashic_secret".to_string()),
            database_url: std::env::var("DATABASE_URL").unwrap_or_else(|_| {
                format!(
                    "postgres://{}:{}@localhost:5433/akashic",
                    "akashic", "akashic_secret"
                )
            }),
            embedding: EmbeddingConfig {
                provider: AiProvider::Local,
                api_key: None,
                model: "text-embedding-3-small".into(),
                base_url: None,
            },
            llm: LlmConfig {
                provider: AiProvider::Local,
                api_key: None,
                model: "gpt-5-nano".into(),
                base_url: None,
            },
            alerts: AlertsConfig::default(),
            module_max_files: 12,
            module_min_files: 3,
            api_host: "0.0.0.0".into(),
            gitlab_webhook_secret: None,
            gitlab_url: "http://unused".into(),
            gitlab_app_id: "test-app-id".into(),
            gitlab_app_secret: SecretString::from("test-app-secret".to_string()),
            gitlab_redirect_uri: "http://localhost:8081/auth/callback".into(),
            gitlab_web_redirect_uri: "http://localhost:8081/auth/web/callback".into(),
            auth_code_ttl_secs: 300,
            api_key_ttl_secs: 86400,
            gitlab_service_token: None,
            frontend_url: "http://localhost:3000".into(),
            public_base_url: "http://localhost:8081".into(),
            cors_extra_origins: vec![],
            cookie_secure: false,
            api_port: 8081,
            ingest_clone_dir: "/tmp/akashic-ingest".into(),
            ingest_max_file_size: 102_400,
            ingest_max_lines: 2000,
            ingest_chunk_max_size: 5120,
            ingest_concurrent_jobs: 1,
            ingest_skip_patterns: vec![],
            ingest_presets_path: None,
            ingest_completeness_threshold: 1.0,
            admin_users: vec![],
            ingest_crawl_max_pages: 100,
            ingest_crawl_delay_ms: 200,
            embedding_precision: EmbeddingPrecision::Float32,
            rate_limit_enabled: false,
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
            mcp_cimd_allow_loopback: false,
        }
    }
}
