use std::fmt;

use crate::types::Config;

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Manual Debug — `SecretString` fields already redact, but we also
        // want to redact any database password we'll later parse out of
        // `database_url`. Print the URL-shape only, with the password segment
        // replaced by [REDACTED] when present.
        f.debug_struct("Config")
            .field("neo4j_uri", &self.neo4j_uri)
            .field("neo4j_user", &self.neo4j_user)
            .field("neo4j_password", &"[REDACTED]")
            .field("database_url", &redact_dsn_password(&self.database_url))
            .field("embedding", &self.embedding)
            .field("llm", &self.llm)
            .field("module_max_files", &self.module_max_files)
            .field("module_min_files", &self.module_min_files)
            .field("api_host", &self.api_host)
            .field(
                "gitlab_webhook_secret",
                &self.gitlab_webhook_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("gitlab_url", &self.gitlab_url)
            .field("gitlab_app_id", &self.gitlab_app_id)
            .field("gitlab_app_secret", &"[REDACTED]")
            .field("gitlab_redirect_uri", &self.gitlab_redirect_uri)
            .field("gitlab_web_redirect_uri", &self.gitlab_web_redirect_uri)
            .field("auth_code_ttl_secs", &self.auth_code_ttl_secs)
            .field("api_key_ttl_secs", &self.api_key_ttl_secs)
            .field(
                "gitlab_service_token",
                &self.gitlab_service_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("frontend_url", &self.frontend_url)
            .field("public_base_url", &self.public_base_url)
            .field("cors_extra_origins", &self.cors_extra_origins)
            .field("cookie_secure", &self.cookie_secure)
            .field("api_port", &self.api_port)
            .field("ingest_clone_dir", &self.ingest_clone_dir)
            .field("ingest_max_file_size", &self.ingest_max_file_size)
            .field("ingest_max_lines", &self.ingest_max_lines)
            .field("ingest_chunk_max_size", &self.ingest_chunk_max_size)
            .field("ingest_concurrent_jobs", &self.ingest_concurrent_jobs)
            .field("ingest_skip_patterns", &self.ingest_skip_patterns)
            .field("ingest_presets_path", &self.ingest_presets_path)
            .field("ingest_crawl_max_pages", &self.ingest_crawl_max_pages)
            .field("ingest_crawl_delay_ms", &self.ingest_crawl_delay_ms)
            .field("embedding_precision", &self.embedding_precision)
            .field(
                "mcp_quota_tokens_per_window",
                &self.mcp_quota_tokens_per_window,
            )
            .field("mcp_quota_window_secs", &self.mcp_quota_window_secs)
            .field("mcp_quota_enabled", &self.mcp_quota_enabled)
            .field(
                "mcp_passthrough_user_cache_ttl_secs",
                &self.mcp_passthrough_user_cache_ttl_secs,
            )
            .field("oauth_validation_mode", &self.oauth_validation_mode)
            .field("migrate_on_boot", &self.migrate_on_boot)
            .field("mcp_cimd_allow_loopback", &self.mcp_cimd_allow_loopback)
            .finish()
    }
}

/// Redact the password component of a DSN-shaped string for `Debug` output.
///
/// Defense in depth:
/// - On successful parse with a password segment, replace the password with
///   `[REDACTED]` and re-emit.
/// - On successful parse with no password, return the URL unchanged (no
///   secret to leak).
/// - On parse failure, return the literal `[UNPARSEABLE-DSN]` rather than
///   echoing the raw value — a malformed DSN may still contain a
///   password-shaped substring, so we never log it.
fn redact_dsn_password(dsn: &str) -> String {
    match url::Url::parse(dsn) {
        Ok(mut u) if u.password().is_some() => {
            // url::Url::set_password takes Option<&str>; ignore the error
            // case (only fails for cannot-be-base URLs, which DSNs aren't).
            let _ = u.set_password(Some("[REDACTED]"));
            u.to_string()
        }
        Ok(u) => u.to_string(),
        Err(_) => "[UNPARSEABLE-DSN]".to_string(),
    }
}
