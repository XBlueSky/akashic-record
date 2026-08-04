use super::*;

// ── RepoService (8 methods) ────────────────────────────────────────────────────

/// Use-case port for repository-metadata and lifecycle operations.
///
/// Covers: listing repos and branches, fetching per-repo permissions and
/// detail, deleting a repo (PG + Neo4j fan-out), proxying GitLab branch
/// metadata, health / readiness probes, and SSE event streaming.
///
/// **A1 note:** all methods are orchestrated entirely inside `akashic-server`
/// handlers and are stubbed with `anyhow::bail!` labeled `(A2)`. The trait
/// compiles and enforces the port contract; A2 will migrate each handler.
#[async_trait]
pub trait RepoService: Send + Sync {
    /// List all ingested repositories (Neo4j + `sources` join).
    ///
    /// HTTP: `GET /api/v1/repos`
    async fn list_repos(&self) -> crate::DomainResult<Vec<RepoInfo>>;

    /// List known branches for a repository (Neo4j + PG branch metadata).
    ///
    /// HTTP: `GET /api/v1/repos/:name/branches`
    async fn list_branches(&self, repo: String) -> crate::DomainResult<Vec<BranchInfo>>;

    /// Resolve the calling user's GitLab access level for a repo.
    ///
    /// HTTP: `GET /api/v1/repos/:name/permissions`
    async fn get_repo_permissions(
        &self,
        repo: String,
        user_token: Option<String>,
    ) -> crate::DomainResult<RepoPermissions>;

    /// Full repo detail: source config + counts + latest job.
    ///
    /// HTTP: `GET /api/v1/repos/:name/detail`
    async fn get_repo_detail(&self, repo: String) -> crate::DomainResult<RepoDetail>;

    /// Delete all PG and Neo4j data for a repository (10+ cascaded deletes).
    ///
    /// HTTP: `DELETE /api/v1/repos/:name`
    async fn delete_repo(&self, repo: String) -> crate::DomainResult<()>;

    /// Proxy GitLab branch list for a repo using the caller's OAuth token.
    ///
    /// HTTP: `GET /api/v1/gitlab/branches?repo=<name>`
    async fn gitlab_branches(
        &self,
        repo: String,
        user_token: String,
    ) -> crate::DomainResult<Vec<GitLabBranch>>;

    /// Basic health check (DB connectivity + config sanity).
    ///
    /// HTTP: `GET /health`
    async fn health(&self) -> crate::DomainResult<HealthStatus>;

    /// Readiness probe: all services ready to serve traffic.
    ///
    /// HTTP: `GET /ready`
    async fn ready(&self) -> crate::DomainResult<HealthStatus>;

    /// MERGE a `(Repository)-[:HAS_BRANCH]->(Branch)` node from a GitLab push /
    /// MR webhook. The write twin of `list_branches`' graph read.
    ///
    /// HTTP (event): `POST /api/v1/webhooks/gitlab`
    async fn sync_branch(
        &self,
        repo: String,
        branch: String,
        commit_hash: String,
    ) -> crate::DomainResult<()>;

    /// MERGE a `(Repository)-[:HAS_TAG]->(Tag)` node from a GitLab tag webhook.
    ///
    /// HTTP (event): `POST /api/v1/webhooks/gitlab`
    async fn sync_tag(
        &self,
        repo: String,
        tag: String,
        commit_hash: String,
    ) -> crate::DomainResult<()>;
}

// ── RepoService domain response types ─────────────────────────────────────────

/// A repository entry in the `list_repos` response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RepoInfo {
    pub name: String,
    pub last_synced_at: Option<String>,
    pub source_type: Option<String>,
}

/// A branch entry in the `list_branches` response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BranchInfo {
    pub name: String,
    pub last_commit_hash: Option<String>,
}

/// Per-repo GitLab access level + derived permission flags.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RepoPermissions {
    pub access_level: i32,
    pub can_edit: bool,
    pub can_delete: bool,
}

/// Full repository detail (source config + counts + latest job).
#[derive(Debug, serde::Serialize)]
pub struct RepoDetail {
    pub name: String,
    pub source_type: Option<String>,
    pub seed_url: Option<String>,
    pub crawl_depth: Option<i16>,
    pub url_pattern: Option<String>,
    pub git_url: Option<String>,
    pub total_chunks: i64,
    pub total_modules: i64,
    pub total_documents: i64,
    pub total_sections: i64,
    pub latest_job: Option<IngestStatus>,
}

/// A GitLab branch item proxied from the GitLab API.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GitLabBranch {
    pub name: String,
    pub default: bool,
}

/// Health / readiness status.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HealthStatus {
    pub status: String,
    pub details: std::collections::HashMap<String, String>,
}
