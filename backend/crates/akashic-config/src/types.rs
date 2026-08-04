use ipnet::IpNet;
use secrecy::SecretString;
use serde::Deserialize;

/// AI provider selection — shared across embedding and LLM configs.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AiProvider {
    Local,
    #[serde(alias = "openai")]
    OpenAi,
    Anthropic,
    Ollama,
    Custom,
}

impl AiProvider {
    pub(crate) fn from_env(val: &str) -> Self {
        match val.to_lowercase().as_str() {
            "openai" => Self::OpenAi,
            "anthropic" => Self::Anthropic,
            "ollama" => Self::Ollama,
            "custom" => Self::Custom,
            "local" => Self::Local,
            _ => Self::Local,
        }
    }
}

/// Embedding service configuration.
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    pub provider: AiProvider,
    pub api_key: Option<SecretString>,
    pub model: String,
    pub base_url: Option<String>,
}

/// LLM service configuration (for reasoning tasks: module grouping, EXPLAINS verification).
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub provider: AiProvider,
    pub api_key: Option<SecretString>,
    pub model: String,
    pub base_url: Option<String>,
}

/// D6 alerting config. `webhook_url=None` means LogSink mode (alerts
/// emit at tracing::warn! instead of POSTing to the webhook). `tick_secs`
/// and `cooldown_secs` are overridable mainly for tests.
#[derive(Debug, Clone)]
pub struct AlertsConfig {
    pub webhook_url: Option<String>,
    pub tick_secs: u64,
    pub cooldown_secs: u64,
}

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            webhook_url: None,
            tick_secs: 60,
            cooldown_secs: 300,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EmbeddingPrecision {
    Float32,
    Float16,
}

impl EmbeddingPrecision {
    pub fn from_env(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "float16" | "f16" | "half" | "halfvec" => Self::Float16,
            _ => Self::Float32,
        }
    }
}

/// Top-level application configuration, loaded from environment variables.
#[derive(Clone)]
pub struct Config {
    // Neo4j
    pub neo4j_uri: String,
    pub neo4j_user: String,
    pub neo4j_password: SecretString,

    // PostgreSQL
    pub database_url: String,

    // Embedding
    pub embedding: EmbeddingConfig,

    // LLM
    pub llm: LlmConfig,

    // Alerts (D6)
    pub alerts: AlertsConfig,

    // Virtual module grouping
    pub module_max_files: u32,
    pub module_min_files: u32,

    // MCP SSE server
    pub mcp_sse_host: String,
    pub mcp_sse_port: u16,
    // GitLab webhook
    pub gitlab_webhook_secret: Option<SecretString>,

    // GitLab OAuth
    pub gitlab_url: String,
    pub gitlab_app_id: String,
    pub gitlab_app_secret: SecretString,
    pub gitlab_redirect_uri: String,
    pub gitlab_web_redirect_uri: String,
    pub auth_code_ttl_secs: u64,
    pub api_key_ttl_secs: u64,

    // GitLab service token (for git clone during ingestion)
    pub gitlab_service_token: Option<SecretString>,

    // Frontend URL (for OAuth redirect after login)
    pub frontend_url: String,

    // Public base URL of this API server (used to build device-flow URIs)
    pub public_base_url: String,

    // CORS
    pub cors_extra_origins: Vec<String>,

    // Cookie security
    pub cookie_secure: bool,

    // REST API server
    pub api_port: u16,

    // Ingestion
    pub ingest_clone_dir: String,
    pub ingest_max_file_size: usize,
    pub ingest_max_lines: usize,
    pub ingest_chunk_max_size: usize,
    pub ingest_concurrent_jobs: usize,
    pub ingest_skip_patterns: Vec<String>,

    /// Optional runtime presets file merged over the embedded adapter preset
    /// table (env: AKASHIC_PRESETS_PATH). Entries with a `host_pattern`
    /// already in the embedded table replace it; new patterns append.
    pub ingest_presets_path: Option<String>,

    /// Minimum fraction of source files that must parse successfully for an
    /// ingest to commit (Roadmap F). `1.0` (default) means any parse failure
    /// aborts the whole ingest with zero database writes.
    pub ingest_completeness_threshold: f64,

    /// GitLab usernames allowed to approve/reject onboarding submissions.
    /// Empty ⇒ all admin endpoints are Forbidden (fail-closed). Env: AKASHIC_ADMIN_USERS.
    pub admin_users: Vec<String>,
    pub ingest_crawl_max_pages: usize,
    pub ingest_crawl_delay_ms: u64,
    pub embedding_precision: EmbeddingPrecision,

    // Rate limit (A6)
    pub rate_limit_enabled: bool,
    pub rate_limit_trusted_proxies: Vec<IpNet>,
    pub rate_limit_allowlist: Vec<IpNet>,

    // MCP quota (B3)
    pub mcp_quota_tokens_per_window: u32,
    pub mcp_quota_window_secs: i64,
    pub mcp_quota_enabled: bool,

    // MCP passthrough user cache (B4)
    pub mcp_passthrough_user_cache_ttl_secs: u64,

    // OAuth validation mode (B5)
    pub oauth_validation_mode: String,

    // Migration discipline (C3) — env-driven schema policy at startup.
    // Parsed at use site via `crate::migrate::MigrateOnBoot::from_env_str`.
    // Stored as String to avoid a compile-time dep on the migrate module.
    pub migrate_on_boot: String,

    // Ingestion quota (A2d-4) — separate budget from the MCP quota so a
    // normal multi-page ingestion run never conflicts with interactive quotas.
    // `ingest_quota_enabled = false` (default) means track-only: spend is
    // recorded against user_id=-1 but never blocked. Set to true to enforce.
    pub ingest_quota_tokens_per_window: u32,
    pub ingest_quota_window_secs: i64,
    pub ingest_quota_enabled: bool,
}

impl Config {
    pub fn vector_type(&self, dim: usize) -> String {
        match self.embedding_precision {
            EmbeddingPrecision::Float32 => format!("vector({dim})"),
            EmbeddingPrecision::Float16 => format!("halfvec({dim})"),
        }
    }

    pub fn cosine_ops(&self) -> &'static str {
        match self.embedding_precision {
            EmbeddingPrecision::Float32 => "vector_cosine_ops",
            EmbeddingPrecision::Float16 => "halfvec_cosine_ops",
        }
    }

    /// True iff `AKASHIC_ENV` is set to `production` or `prod`. Used by
    /// `oauth_runtime::https_in_production` static check (B5).
    ///
    /// Delegates to the free [`is_production`] helper so prod detection is
    /// identical on every input. Previously this method compared the raw
    /// value without trim/lowercase, so non-canonical casing/whitespace
    /// (e.g. `" Production "`, `"PRODUCTION"`) was treated as production by
    /// `validate_for_production` (via the free fn) but non-production by the
    /// migrate-on-boot and OAuth-HTTPS gates (via this method) — closes that
    /// inconsistent-prod-detection finding.
    pub fn is_production(&self) -> bool {
        crate::is_production()
    }
}
