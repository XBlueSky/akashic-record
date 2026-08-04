/// Events pushed to all connected SSE clients.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type")]
pub enum AppEvent {
    /// Ingestion job status changed.
    #[serde(rename = "job_update")]
    JobUpdate {
        repo_name: String,
        status: String,
        processed_files: Option<i32>,
        total_files: Option<i32>,
        total_chunks: Option<i32>,
    },
    /// Repo list changed (source added or repo deleted).
    #[serde(rename = "repos_changed")]
    ReposChanged,
    /// C2: server is about to shut down. Subscribers should treat this as a
    /// final event and prepare to reconnect after `reconnect_hint_secs`.
    #[serde(rename = "server_shutting_down")]
    ServerShuttingDown { reconnect_hint_secs: u64 },
}
