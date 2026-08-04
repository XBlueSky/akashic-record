use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde::Deserialize;
use tracing::{info, warn};

use akashic_context::AppState;

/// GitLab webhook event types we handle.
#[derive(Debug, Deserialize)]
#[serde(tag = "object_kind")]
#[serde(rename_all = "snake_case")]
enum GitLabEvent {
    Push(PushEvent),
    TagPush(TagPushEvent),
    MergeRequest(MergeRequestEvent),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct PushEvent {
    project: Project,
    #[serde(rename = "ref")]
    ref_name: String,
    after: String,
}

#[derive(Debug, Deserialize)]
struct TagPushEvent {
    project: Project,
    #[serde(rename = "ref")]
    ref_name: String,
    after: String,
}

#[derive(Debug, Deserialize)]
struct MergeRequestEvent {
    project: Project,
    object_attributes: MergeRequestAttributes,
}

#[derive(Debug, Deserialize)]
struct MergeRequestAttributes {
    source_branch: String,
    target_branch: String,
    #[allow(dead_code)]
    state: String,
}

#[derive(Debug, Deserialize)]
struct Project {
    #[allow(dead_code)]
    id: i64,
    path_with_namespace: String,
}

/// Build the webhook router.
pub fn router() -> Router<AppState> {
    Router::new().route("/webhook/gitlab", post(handle_webhook))
}

/// POST /webhook/gitlab — process GitLab webhook events.
async fn handle_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(event): Json<GitLabEvent>,
) -> StatusCode {
    // Optional secret validation
    if let Some(expected_secret) = state
        .config
        .gitlab_webhook_secret
        .as_ref()
        .map(secrecy::ExposeSecret::expose_secret)
    {
        let token = headers
            .get("X-Gitlab-Token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if token != expected_secret {
            warn!("Webhook secret mismatch");
            return StatusCode::UNAUTHORIZED;
        }
    }

    match event {
        GitLabEvent::Push(e) => {
            let branch = e
                .ref_name
                .strip_prefix("refs/heads/")
                .unwrap_or(&e.ref_name);
            if let Err(err) =
                sync_branch(&state, &e.project.path_with_namespace, branch, &e.after).await
            {
                warn!(%err, "Failed to sync push event");
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
            info!(repo = %e.project.path_with_namespace, branch, "Push event synced");
        }
        GitLabEvent::TagPush(e) => {
            let tag = e.ref_name.strip_prefix("refs/tags/").unwrap_or(&e.ref_name);
            if let Err(err) = sync_tag(&state, &e.project.path_with_namespace, tag, &e.after).await
            {
                warn!(%err, "Failed to sync tag push event");
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
            info!(repo = %e.project.path_with_namespace, tag, "Tag push event synced");
        }
        GitLabEvent::MergeRequest(e) => {
            let repo = &e.project.path_with_namespace;
            // Ensure both source and target branches exist
            for branch in [
                &e.object_attributes.source_branch,
                &e.object_attributes.target_branch,
            ] {
                if let Err(err) = sync_branch(&state, repo, branch, "").await {
                    warn!(%err, branch, "Failed to sync MR branch");
                }
            }
            info!(repo, source = %e.object_attributes.source_branch, target = %e.object_attributes.target_branch, "MR event synced");
        }
        GitLabEvent::Unknown => {
            info!("Ignoring unknown webhook event type");
        }
    }

    StatusCode::OK
}

/// MERGE a repository and branch node via `RepoService::sync_branch`.
///
/// Thinned (A2a): the Cypher lives in `Neo4jRepoGraphRepo::sync_branch`, reached
/// through the repo service so the handler no longer touches `state.db`.
async fn sync_branch(
    state: &AppState,
    repo_name: &str,
    branch_name: &str,
    commit_hash: &str,
) -> anyhow::Result<()> {
    state
        .repo_service
        .sync_branch(
            repo_name.to_string(),
            branch_name.to_string(),
            commit_hash.to_string(),
        )
        .await
        .map_err(anyhow::Error::from)
}

/// MERGE a repository and tag node via `RepoService::sync_tag`.
///
/// Thinned (A2a): the Cypher lives in `Neo4jRepoGraphRepo::sync_tag`, reached
/// through the repo service so the handler no longer touches `state.db`.
async fn sync_tag(
    state: &AppState,
    repo_name: &str,
    tag_name: &str,
    commit_hash: &str,
) -> anyhow::Result<()> {
    state
        .repo_service
        .sync_tag(
            repo_name.to_string(),
            tag_name.to_string(),
            commit_hash.to_string(),
        )
        .await
        .map_err(anyhow::Error::from)
}
