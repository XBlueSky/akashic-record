//! `ReqwestGitLabGateway` — the only `reqwest::Client` in the backend besides
//! the embed/llm provider clients. Implements `akashic_domain::ports::gitlab`.
//!
//! All GitLab HTTP lives here, moved verbatim from the former inline call-sites
//! in akashic-identity / akashic-ingestion / akashic-http.

use std::sync::Arc;

use akashic_config::Config;
use akashic_domain::ports::gitlab::{GitLabGateway, GitLabToken, GitLabUser};
use akashic_domain::ports::services::GitLabBranch;
use akashic_domain::types::ValidationReport;
use async_trait::async_trait;
use secrecy::ExposeSecret;

mod oauth_runtime;
pub mod oauth_runtime_cli;
pub use oauth_runtime::ValidationMode;
pub use oauth_runtime_cli::print_human_report;

pub struct ReqwestGitLabGateway {
    client: reqwest::Client,
    config: Arc<Config>,
}

impl ReqwestGitLabGateway {
    pub fn new(client: reqwest::Client, config: Arc<Config>) -> Self {
        Self { client, config }
    }

    fn base(&self) -> &str {
        self.config.gitlab_url.trim_end_matches('/')
    }
}

#[async_trait]
impl GitLabGateway for ReqwestGitLabGateway {
    async fn exchange_oauth_code(
        &self,
        code: &str,
        redirect_uri: &str,
    ) -> anyhow::Result<GitLabToken> {
        let token_url = format!("{}/oauth/token", self.config.gitlab_url);
        let resp = self
            .client
            .post(&token_url)
            .form(&[
                ("client_id", self.config.gitlab_app_id.as_str()),
                (
                    "client_secret",
                    self.config.gitlab_app_secret.expose_secret(),
                ),
                ("code", code),
                ("grant_type", "authorization_code"),
                ("redirect_uri", redirect_uri),
            ])
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("gitlab_token_request_failed: {e}"))?;
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("gitlab_token_parse_failed: {e}"))?;
        let access_token = json["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no_access_token"))?
            .to_string();
        Ok(GitLabToken { access_token })
    }

    async fn fetch_user(&self, access_token: &str) -> anyhow::Result<GitLabUser> {
        let user_url = format!("{}/api/v4/user", self.config.gitlab_url);
        let resp = self
            .client
            .get(&user_url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("gitlab_user_request_failed: {e}"))?;
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("gitlab_user_parse_failed: {e}"))?;
        Ok(GitLabUser {
            id: json["id"].as_i64(),
            username: json["username"].as_str().unwrap_or("unknown").to_string(),
            name: json["name"].as_str().map(String::from),
            avatar_url: json["avatar_url"].as_str().map(String::from),
        })
    }

    async fn validate_passthrough(&self, pat: &str) -> anyhow::Result<Option<GitLabUser>> {
        // Fail-closed HTTP half of the passthrough probe (tombstone/LRU stay in
        // akashic-identity). Verbatim status handling from the former
        // store.rs::validate_passthrough_token cache-miss block.
        let url = format!("{}/api/v4/user", self.base());
        let resp = match self.client.get(&url).bearer_auth(pat).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(event = "passthrough_upstream_err", error = %e);
                return Ok(None);
            }
        };
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            if (500..600).contains(&status) {
                tracing::warn!(event = "passthrough_upstream_err", status);
            } else {
                tracing::info!(event = "mcp_auth_passthrough_invalid", status);
            }
            return Ok(None);
        }
        let json: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(event = "passthrough_upstream_err", error = "json parse");
                return Ok(None);
            }
        };
        Ok(Some(GitLabUser {
            id: json["id"].as_i64(),
            username: json["username"].as_str().unwrap_or_default().to_string(),
            name: json["name"].as_str().map(String::from),
            avatar_url: json["avatar_url"].as_str().map(String::from),
        }))
    }

    async fn project_access_level(&self, repo: &str, token: &str) -> anyhow::Result<i32> {
        let search_term = repo.rsplit('/').next().unwrap_or(repo);
        let resp = self
            .client
            .get(format!("{}/api/v4/projects", self.config.gitlab_url))
            .bearer_auth(token)
            .query(&[("membership", "true"), ("search", search_term)])
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitLab API error: {e}"))?;
        let projects: Vec<serde_json::Value> = resp.json().await.unwrap_or_default();
        for p in &projects {
            tracing::debug!(
                path = %p["path_with_namespace"],
                project_access = %p["permissions"]["project_access"],
                group_access = %p["permissions"]["group_access"],
                "GitLab project permission"
            );
        }
        let level = select_gitlab_project(&projects, repo, search_term).map_or(0, |p| {
            let pl = p["permissions"]["project_access"]["access_level"]
                .as_i64()
                .unwrap_or(0);
            let gl = p["permissions"]["group_access"]["access_level"]
                .as_i64()
                .unwrap_or(0);
            pl.max(gl)
        });
        Ok(level as i32)
    }

    async fn list_branches(&self, repo: &str, token: &str) -> anyhow::Result<Vec<GitLabBranch>> {
        let encoded_repo = urlencoding::encode(repo).into_owned();
        let base_url = format!(
            "{}/api/v4/projects/{}/repository/branches",
            self.config.gitlab_url, encoded_repo
        );
        let mut all_items: Vec<GitLabBranch> = Vec::new();
        let mut page = 1u32;
        loop {
            let url = format!("{base_url}?per_page=100&page={page}");
            let resp = self
                .client
                .get(&url)
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("GitLab API error: {e}"))?;
            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(status, body, "GitLab branches API failed");
                anyhow::bail!("GitLab returned {status}");
            }
            let next_page: Option<u32> = resp
                .headers()
                .get("x-next-page")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let branches: Vec<serde_json::Value> = resp
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to parse GitLab response: {e}"))?;
            if branches.is_empty() {
                break;
            }
            all_items.extend(branches.iter().filter_map(|b| {
                Some(GitLabBranch {
                    name: b["name"].as_str()?.to_string(),
                    default: b["default"].as_bool().unwrap_or(false),
                })
            }));
            match next_page {
                Some(np) if np > page => page = np,
                _ => break,
            }
        }
        Ok(all_items)
    }

    async fn runtime_report(&self) -> ValidationReport {
        oauth_runtime::validate(&self.config, &self.client).await
    }
}

/// Pick the GitLab project matching `repo_name`. Prefers an exact
/// `path_with_namespace` match; otherwise matches the last path segment
/// exactly (no loose `ends_with` — that over-granted access). Moved verbatim
/// from akashic-http::api::routes::mod.rs / akashic-ingestion::services.rs.
fn select_gitlab_project<'a>(
    projects: &'a [serde_json::Value],
    repo_name: &str,
    search_term: &str,
) -> Option<&'a serde_json::Value> {
    if let Some(p) = projects
        .iter()
        .find(|p| p["path_with_namespace"].as_str() == Some(repo_name))
    {
        return Some(p);
    }
    if search_term.is_empty() {
        return None;
    }
    projects.iter().find(|p| {
        p["path_with_namespace"]
            .as_str()
            .and_then(|s| s.rsplit('/').next())
            == Some(search_term)
    })
}

#[cfg(test)]
mod select_project_tests {
    use super::select_gitlab_project;
    use serde_json::json;

    #[test]
    fn select_project_rejects_suffix_overgrant() {
        let projects = [json!({"path_with_namespace": "group/other-repo"})];
        // "repo" must NOT match "group/other-repo" (the old ends_with logic did).
        assert!(select_gitlab_project(&projects, "repo", "repo").is_none());
    }

    #[test]
    fn select_project_rejects_empty_search_term() {
        let projects = [json!({"path_with_namespace": "group/anything"})];
        assert!(select_gitlab_project(&projects, "", "").is_none());
    }

    #[test]
    fn select_project_matches_exact_last_segment() {
        let projects = [json!({"path_with_namespace": "group/repo"})];
        assert!(select_gitlab_project(&projects, "repo", "repo").is_some());
    }

    #[test]
    fn select_project_prefers_exact_full_path() {
        let projects = [
            json!({"path_with_namespace": "a/repo"}),
            json!({"path_with_namespace": "b/repo"}),
        ];
        let p = select_gitlab_project(&projects, "b/repo", "repo").unwrap();
        assert_eq!(p["path_with_namespace"], "b/repo");
    }
}
