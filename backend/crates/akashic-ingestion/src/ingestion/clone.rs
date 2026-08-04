use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

/// Clone a git repository (or use a local path) into a temp directory.
///
/// If `local_path` is provided, returns it directly (no clone needed).
/// Otherwise, performs a shallow git clone using the GitLab service token.
pub fn clone_repo(
    repo_name: &str,
    git_ref: &str,
    gitlab_url: &str,
    service_token: Option<&str>,
    clone_dir: &str,
    local_path: Option<&str>,
) -> Result<PathBuf> {
    // Local path: just validate it exists
    if let Some(path) = local_path {
        let p = Path::new(path);
        anyhow::ensure!(p.exists(), "Local path does not exist: {path}");
        anyhow::ensure!(p.is_dir(), "Local path is not a directory: {path}");
        info!(path, "Using local path for ingestion");
        return Ok(p.to_path_buf());
    }

    // Git clone
    let dest = Path::new(clone_dir).join(repo_name.replace('/', "_"));
    if dest.exists() {
        std::fs::remove_dir_all(&dest).context("Failed to clean previous clone dir")?;
    }
    std::fs::create_dir_all(&dest).context("Failed to create clone dir")?;

    let clone_url = if let Some(token) = service_token {
        // Strip protocol, inject token
        let base = gitlab_url.trim_end_matches('/');
        let base_no_proto = base
            .strip_prefix("https://")
            .or_else(|| base.strip_prefix("http://"))
            .unwrap_or(base);
        format!("https://oauth2:{token}@{base_no_proto}/{repo_name}.git")
    } else {
        format!("{}/{repo_name}.git", gitlab_url.trim_end_matches('/'))
    };

    info!(repo_name, git_ref, "Cloning repository");

    let mut builder = git2::build::RepoBuilder::new();
    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.depth(1);
    builder.fetch_options(fetch_opts);
    builder.branch(git_ref);

    builder
        .clone(&clone_url, &dest)
        .context(format!("git clone failed for {repo_name}"))?;

    info!(dest = %dest.display(), "Clone complete");
    Ok(dest)
}
