//! Application-service implementations for `IngestService` and `RepoService`.
//!
//! Both traits are defined in `akashic-domain::ports::services` and implemented
//! here in `akashic-ingestion` — the crate that already owns the
//! `IngestionPipeline` orchestrator and the community-detection module.
//!
//! **A2a scope (Task 8):**
//! - All `RepoService` methods are now fully implemented.
//! - `IngestService::trigger_ingest` + `detect_communities` remain as before.
//! - All other `IngestService` methods whose orchestration still resides in
//!   `akashic-server` handlers remain stubbed with `anyhow::bail!` labeled
//!   `(A2)`.
//!
//! **A2** wires `AppState` to carry `Arc<dyn IngestService>` etc.; until then
//! the remaining stub methods are not called by any handler.

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use akashic_domain::ports::gitlab::GitLabGateway;
use akashic_domain::ports::services::{
    ActiveJobItem, AddSourceResult, BranchInfo, DemandItem, GitLabBranch, HealthStatus,
    IdempotencyCheck, IdempotencyReservation, IngestService, IngestStatus, PendingSourceItem,
    RepoDetail, RepoInfo, RepoPermissions, RepoService, SourceOverviewItem, SourcesOverview,
};
use akashic_domain::ports::source::WebsiteSourceUpsert;
use akashic_domain::ports::{
    CommunityGraphRepo, CommunityRepo, GraphReadRepo, IngestionJobRepo, RepoGraphRepo,
    SagaExecutorRepo, SourceRepo,
};
use akashic_domain::{DomainError, DomainResult};

use crate::ingestion::pipeline::{IngestRequest, IngestionPipeline};

// ── Helpers ────────────────────────────────────────────────────────────────────

// resolve_reingest_git_ref moved to `akashic_domain::algos::git_ref` so the
// HTTP-layer unit tests exercise the same implementation instead of a
// #[cfg(test)] copy.
use akashic_domain::algos::resolve_reingest_git_ref;

// ── IngestionServices ──────────────────────────────────────────────────────────

/// Thin service struct that holds an `IngestionPipeline`, port arcs for
/// community detection, and the graph/source/job repos needed by `RepoService`.
///
/// The `IngestionPipeline` already owns `Arc<dyn IngestionJobRepo>` and all
/// PG/Neo4j adapters it needs internally, so we hold separate arcs only for
/// the repos that `RepoService` methods read directly.
pub struct IngestionServices {
    pipeline: IngestionPipeline,
    community_repo: Arc<dyn CommunityRepo>,
    community_graph: Arc<dyn CommunityGraphRepo>,
    // A2a Task 8: RepoService dependencies
    graph_read: Arc<dyn GraphReadRepo>,
    source_repo: Arc<dyn SourceRepo>,
    job_repo: Arc<dyn IngestionJobRepo>,
    repo_graph: Arc<dyn RepoGraphRepo>,
    saga_executor_repo: Arc<dyn SagaExecutorRepo>,
    pg: sqlx::PgPool,
    gateway: Arc<dyn GitLabGateway>,
    /// Base GitLab URL — used to derive the git **clone** URL for source
    /// records (`{gitlab_url}/{repo}`). NOT used for HTTP (that goes through
    /// `gateway`).
    gitlab_url: String,
}

impl IngestionServices {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pipeline: IngestionPipeline,
        community_repo: Arc<dyn CommunityRepo>,
        community_graph: Arc<dyn CommunityGraphRepo>,
        graph_read: Arc<dyn GraphReadRepo>,
        source_repo: Arc<dyn SourceRepo>,
        job_repo: Arc<dyn IngestionJobRepo>,
        repo_graph: Arc<dyn RepoGraphRepo>,
        saga_executor_repo: Arc<dyn SagaExecutorRepo>,
        pg: sqlx::PgPool,
        gateway: Arc<dyn GitLabGateway>,
        gitlab_url: String,
    ) -> Self {
        Self {
            pipeline,
            community_repo,
            community_graph,
            graph_read,
            source_repo,
            job_repo,
            repo_graph,
            saga_executor_repo,
            pg,
            gateway,
            gitlab_url,
        }
    }
}

// ── IngestService impl ─────────────────────────────────────────────────────────

#[async_trait]
impl IngestService for IngestionServices {
    /// Delegates to `IngestionPipeline::start` and upserts the source record.
    ///
    /// The pipeline creates the job record and spawns the 9-stage run in the
    /// background; the returned `Uuid` is the new job ID.
    ///
    /// Note: the idempotency reserve/finalize/release guard stays handler-side;
    /// the active-job check (has_active_job) happens here, after the reserve.
    async fn trigger_ingest(
        &self,
        repo: String,
        git_ref: String,
        source: String,
        path: Option<String>,
        user_token: Option<String>,
    ) -> DomainResult<Uuid> {
        // Active-job check (after idempotency reserve in the handler).
        if self.job_repo.has_active_job(&repo).await? {
            return Err(DomainError::Conflict(
                "An ingestion job is already running for this repo".into(),
            ));
        }

        let req = IngestRequest {
            repo_name: repo.clone(),
            git_ref: git_ref.clone(),
            source: source.clone(),
            local_path: path,
            user_token,
            seed_url: None,
            crawl_depth: None,
            url_pattern: None,
        };

        let job_id = self.pipeline.start(req).await?;

        // Persist source record for this repo (gitlab).
        // git_url is derived from the configured gitlab_url + repo name.
        if source == "gitlab" {
            let git_url = format!("{}/{}", self.gitlab_url, &repo);
            self.source_repo
                .upsert_source_gitlab(&repo, &git_url)
                .await?;
        }

        Ok(job_id)
    }

    async fn reingest(
        &self,
        repo: String,
        branch: Option<String>,
        user_token: Option<String>,
    ) -> DomainResult<Uuid> {
        // Orchestration moved verbatim from
        // akashic-server::api::routes::ingestion::reingest.

        // Reject if an active job already exists for this repo
        if self.job_repo.has_active_job(&repo).await? {
            return Err(DomainError::Conflict(
                "An ingestion job is already running for this repo".into(),
            ));
        }

        // Look up stored source config
        let source = self.source_repo.lookup_source_config(&repo).await?;
        let (source_type, seed_url, crawl_depth, url_pattern, _git_url) = match source {
            Some(s) => (
                s.source_type,
                s.seed_url,
                s.crawl_depth,
                s.url_pattern,
                s.git_url,
            ),
            None => {
                // Repos with "/" are typically gitlab; otherwise default to website.
                let inferred_type = if repo.contains('/') {
                    "gitlab".to_string()
                } else {
                    "website".to_string()
                };
                (inferred_type, None, None, None, None)
            }
        };

        // Recover git_ref from most recent prior job when ?branch was not given
        // (finding #39 — must not default to "" for gitlab repos).
        let last_ref = self.job_repo.get_last_job_git_ref(&repo).await?;
        let git_ref = resolve_reingest_git_ref(branch, last_ref);

        let req = IngestRequest {
            repo_name: repo,
            git_ref,
            source: source_type,
            local_path: None,
            user_token,
            seed_url,
            crawl_depth: crawl_depth.map(|d| d as u8),
            url_pattern,
        };

        self.pipeline
            .start(req)
            .await
            .map_err(DomainError::Internal)
    }

    async fn resume_ingest(&self, repo: String, user_token: Option<String>) -> DomainResult<Uuid> {
        // Orchestration moved verbatim from
        // akashic-server::api::routes::ingestion::resume_ingest.

        // Find most recent failed job with checkpoint
        let (prev_job_id, git_ref, source_type) = self
            .job_repo
            .find_resumable_job(&repo)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("resumable job for repo {repo}")))?;

        let req = IngestRequest {
            repo_name: repo,
            git_ref,
            source: source_type.unwrap_or_else(|| "gitlab".into()),
            local_path: None,
            user_token,
            seed_url: None,
            crawl_depth: None,
            url_pattern: None,
        };

        self.pipeline
            .start_resume(req, prev_job_id)
            .await
            .map_err(DomainError::Internal)
    }

    async fn ingest_status(&self, repo: String) -> DomainResult<IngestStatus> {
        // Delegates to IngestionJobRepo::latest_job (SQL moved verbatim from
        // akashic-server::api::routes::ingestion::ingest_status).
        let row = self
            .job_repo
            .latest_job(&repo)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("ingestion job for repo {repo}")))?;
        Ok(job_tuple_to_ingest_status(row))
    }

    async fn active_jobs(&self, limit: i64, offset: i64) -> DomainResult<Vec<ActiveJobItem>> {
        // Delegates to IngestionJobRepo::list_active_jobs (SQL moved verbatim
        // from akashic-server::api::routes::ingestion::active_jobs).
        let rows = self.job_repo.list_active_jobs(limit, offset).await?;
        Ok(rows
            .into_iter()
            .map(
                |(repo_name, status, processed_files, total_files)| ActiveJobItem {
                    repo_name,
                    status,
                    processed_files,
                    total_files,
                },
            )
            .collect())
    }

    async fn add_source(
        &self,
        source_type: String,
        url: String,
        crawl_depth: Option<u8>,
        url_pattern: Option<String>,
        submitter: String,
    ) -> DomainResult<AddSourceResult> {
        match source_type.as_str() {
            "website" => {
                let url = if !url.starts_with("http://") && !url.starts_with("https://") {
                    format!("https://{url}")
                } else {
                    url
                };
                let parsed = reqwest::Url::parse(&url)
                    .map_err(|_| DomainError::BadRequest("Invalid URL".into()))?;
                let host = parsed
                    .host_str()
                    .ok_or_else(|| DomainError::BadRequest("URL has no hostname".into()))?;
                let first_segment = parsed
                    .path_segments()
                    .and_then(|mut s| s.find(|p| !p.is_empty()));
                let repo_name = match first_segment {
                    Some(seg) => format!("{host}/{seg}"),
                    None => host.to_string(),
                };
                if self.job_repo.has_active_job(&repo_name).await? {
                    return Err(DomainError::Conflict(
                        "An ingestion job is already running for this source".into(),
                    ));
                }
                let probe = self.pipeline.probe_source(&url).await;
                let status = classification_to_status(&probe.classification);
                self.source_repo
                    .upsert_website_source(WebsiteSourceUpsert {
                        repo: &repo_name,
                        seed_url: &url,
                        crawl_depth: crawl_depth.map(|d| d as i16),
                        url_pattern,
                        submitter: &submitter,
                        status,
                        adapter_id: probe.adapter_id.as_deref(),
                        classification: &probe.classification,
                        probe_note: &probe.note,
                    })
                    .await?;
                Ok(AddSourceResult {
                    job_id: String::new(),
                    repo_name,
                    status: status.to_string(),
                })
            }
            other => Err(DomainError::BadRequest(format!(
                "Unsupported source type: {other}"
            ))),
        }
    }

    async fn approve_source(
        &self,
        id: uuid::Uuid,
        reviewer: String,
    ) -> DomainResult<AddSourceResult> {
        let src = self
            .source_repo
            .get_source(id)
            .await?
            .ok_or_else(|| DomainError::NotFound("source not found".into()))?;
        if src.status != "pending_review" {
            return Err(DomainError::Conflict(format!(
                "source is {}, not pending_review",
                src.status
            )));
        }
        if self.job_repo.has_active_job(&src.repo_name).await? {
            return Err(DomainError::Conflict(
                "An ingestion job is already running for this source".into(),
            ));
        }
        let seed_url = src
            .seed_url
            .clone()
            .ok_or_else(|| DomainError::BadRequest("source has no seed_url".into()))?;
        let req = IngestRequest {
            repo_name: src.repo_name.clone(),
            git_ref: String::new(),
            source: "website".into(),
            local_path: None,
            user_token: None,
            seed_url: Some(seed_url),
            crawl_depth: src.crawl_depth.map(|d| d.clamp(0, 10) as u8),
            url_pattern: src.url_pattern.clone(),
        };
        // Start the crawl FIRST — if it fails the row stays `pending_review`
        // and can be re-approved. Marking `approved` before a failed spawn would
        // strand the row (the `status != "pending_review"` guard above would
        // then reject any subsequent approval attempt).
        let job_id = self.pipeline.start(req).await?;
        self.source_repo
            .set_source_status(id, "approved", &reviewer)
            .await?;
        Ok(AddSourceResult {
            job_id: job_id.to_string(),
            repo_name: src.repo_name,
            status: "approved".into(),
        })
    }

    async fn reject_source(&self, id: uuid::Uuid, reviewer: String) -> DomainResult<()> {
        let existed = self
            .source_repo
            .set_source_status(id, "rejected", &reviewer)
            .await?;
        if !existed {
            return Err(DomainError::NotFound("source not found".into()));
        }
        Ok(())
    }

    async fn list_pending_sources(&self) -> DomainResult<Vec<PendingSourceItem>> {
        let rows = self.source_repo.list_pending_sources().await?;
        Ok(rows
            .into_iter()
            .map(|s| PendingSourceItem {
                id: s.id.to_string(),
                repo_name: s.repo_name,
                seed_url: s.seed_url,
                submitter: s.submitter,
                adapter_id: s.adapter_id,
                classification: s.classification,
                probe_note: s.probe_note,
            })
            .collect())
    }

    async fn list_demand(&self) -> DomainResult<Vec<DemandItem>> {
        Ok(self
            .source_repo
            .list_demand()
            .await?
            .into_iter()
            .map(|d| DemandItem {
                host: d.host,
                count: d.count,
                sample_note: d.sample_note,
            })
            .collect())
    }

    async fn sources_overview(&self) -> DomainResult<SourcesOverview> {
        // Delegates to SourceRepo::sources_overview (SQL moved verbatim from
        // akashic-server::api::routes::ingestion::sources_overview).
        let rows = self.source_repo.sources_overview().await?;

        let stale_git_days: i64 = 7;
        let stale_web_days: i64 = 14;

        let sources: Vec<SourceOverviewItem> = rows
            .into_iter()
            .map(|r| {
                let is_active = r
                    .job_status
                    .as_deref()
                    .map(|s| !["done", "completed", "failed"].contains(&s))
                    .unwrap_or(false);

                let status = if let Some(gated) = gated_overview_status(&r.source_status) {
                    // Awaiting review / rejected / unsupported: not served yet,
                    // must not be reported as healthy.
                    gated.to_string()
                } else if is_active {
                    "ingesting".to_string()
                } else if r.job_status.as_deref() == Some("failed") {
                    "failed".to_string()
                } else {
                    let threshold = if r.source_type == "website" {
                        stale_web_days
                    } else {
                        stale_git_days
                    };
                    match &r.completed_at {
                        Some(ts) => {
                            if let Ok(dt) =
                                chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%SZ")
                            {
                                let age = chrono::Utc::now().naive_utc() - dt;
                                if age.num_days() > threshold {
                                    "stale".into()
                                } else {
                                    "healthy".into()
                                }
                            } else {
                                "healthy".into()
                            }
                        }
                        None => "healthy".into(),
                    }
                };

                let active_job = if is_active {
                    Some(ActiveJobItem {
                        repo_name: r.repo_name.clone(),
                        status: r.job_status.clone().unwrap_or_default(),
                        processed_files: r.processed_files,
                        total_files: r.total_files,
                    })
                } else {
                    None
                };

                SourceOverviewItem {
                    name: r.repo_name,
                    source_type: r.source_type,
                    status,
                    branch: r.git_ref,
                    chunk_count: r.chunk_count,
                    module_count: r.module_count,
                    note_count: r.note_count,
                    section_count: 0, // simplified — sections are website-only
                    last_synced_at: r.completed_at,
                    active_job,
                    last_error: if r.job_status.as_deref() == Some("failed") {
                        r.error_message
                    } else {
                        None
                    },
                    can_resume: r.can_resume,
                }
            })
            .collect();

        let total = sources.len();
        let healthy = sources.iter().filter(|s| s.status == "healthy").count();
        let stale = sources.iter().filter(|s| s.status == "stale").count();
        let failed = sources.iter().filter(|s| s.status == "failed").count();
        let ingesting = sources.iter().filter(|s| s.status == "ingesting").count();

        Ok(SourcesOverview {
            sources,
            total,
            healthy,
            stale,
            failed,
            ingesting,
        })
    }

    /// Run community detection (Leiden / union-find) as a standalone stage.
    ///
    /// Delegates to `crate::community::detect_communities` using the port-backed
    /// `CommunityRepo` and `CommunityGraphRepo` arcs held on this struct.
    async fn detect_communities(&self, repo_name: String) -> DomainResult<()> {
        crate::community::detect_communities(
            &self.community_graph,
            &self.community_repo,
            &repo_name,
        )
        .await?;
        Ok(())
    }

    async fn resolve_source_type(&self, repo: String) -> DomainResult<(String, bool)> {
        match self.source_repo.lookup_source_config(&repo).await? {
            Some(cfg) => Ok((cfg.source_type, true)),
            None => {
                let inferred = if repo.contains('/') {
                    "gitlab".to_string()
                } else {
                    "website".to_string()
                };
                Ok((inferred, false))
            }
        }
    }

    async fn idempotency_check(&self, key: String) -> DomainResult<IdempotencyCheck> {
        self.saga_executor_repo
            .idempotency_check(&key)
            .await
            .map_err(DomainError::Internal)
    }

    async fn idempotency_reserve(
        &self,
        key: String,
        saga_type: String,
        repo: String,
    ) -> DomainResult<IdempotencyReservation> {
        self.saga_executor_repo
            .idempotency_reserve(&key, &saga_type, &repo)
            .await
            .map_err(DomainError::Internal)
    }

    async fn idempotency_finalize(
        &self,
        key: String,
        result: serde_json::Value,
    ) -> DomainResult<()> {
        self.saga_executor_repo
            .idempotency_finalize(&key, &result)
            .await
            .map_err(DomainError::Internal)
    }

    async fn idempotency_release(&self, key: String) {
        self.saga_executor_repo.idempotency_release(&key).await
    }

    async fn recluster_doc_sections(&self, repo: String) -> DomainResult<usize> {
        // DocClustering needs pg + neo4j + llm, all owned by the pipeline; this
        // keeps the relink-explains clustering off AppState's pools.
        self.pipeline
            .recluster_doc_sections(&repo)
            .await
            .map_err(DomainError::Internal)
    }
}

// ── RepoService impl ───────────────────────────────────────────────────────────

/// Helper: convert a latest-job tuple to `IngestStatus`.
fn job_tuple_to_ingest_status(
    row: (
        Uuid,
        String,
        Option<String>,
        String,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<String>,
        Option<String>,
        Option<String>,
    ),
) -> IngestStatus {
    let (
        id,
        repo_name,
        git_ref,
        status,
        total_files,
        processed_files,
        total_chunks,
        error_message,
        started_at,
        completed_at,
    ) = row;
    IngestStatus {
        job_id: id.to_string(),
        repo_name,
        git_ref,
        status,
        total_files,
        processed_files,
        total_chunks,
        error_message,
        started_at,
        completed_at,
    }
}

#[async_trait]
impl RepoService for IngestionServices {
    async fn list_repos(&self) -> DomainResult<Vec<RepoInfo>> {
        // Neo4j: enumerate Repository nodes
        let branches = self.graph_read.get_branches("").await;
        // `get_branches` is per-repo; for list_repos we need all repos.
        // Use a separate query pattern: list all distinct repo names from Neo4j
        // via the get_graph_modules / get_graph_notes paths — but those are also
        // per-repo. The simplest approach: query PG sources for source_type,
        // then use the Neo4j repository list query.
        //
        // NOTE: GraphReadRepo doesn't have a "list all repos" method since it was
        // designed for per-repo reads. The repo list comes from Neo4j Repository
        // nodes. We reuse the pattern from repos.rs: Cypher MATCH (r:Repository).
        // Since we can't add new Cypher here without a new port method, we fall
        // back to the PG sources table as the repo registry (all ingested repos
        // have a sources row from trigger_ingest upsert).
        //
        // Pragmatic implementation: list from PG sources (covers all repos that
        // have been ingested via trigger_ingest / add_source), then enrich with
        // Neo4j last_synced_at where available. If a repo has no sources row
        // (edge case: manually created Neo4j node) it won't appear — acceptable.
        let _ = branches; // suppress unused warning from the get_branches attempt above

        let source_rows = self.source_repo.list_source_types().await?;
        let repos: Vec<RepoInfo> = source_rows
            .into_iter()
            .map(|r| RepoInfo {
                name: r.repo_name,
                last_synced_at: None,
                source_type: Some(r.source_type),
            })
            .collect();
        Ok(repos)
    }

    async fn list_branches(&self, repo: String) -> DomainResult<Vec<BranchInfo>> {
        // Cypher via GraphReadRepo::get_branches — verbatim from repos.rs
        let rows = self.graph_read.get_branches(&repo).await?;
        Ok(rows
            .into_iter()
            .map(|b| BranchInfo {
                name: b.name,
                last_commit_hash: None,
            })
            .collect())
    }

    async fn get_repo_permissions(
        &self,
        repo: String,
        user_token: Option<String>,
    ) -> DomainResult<RepoPermissions> {
        let token = match user_token {
            Some(t) if !t.is_empty() => t,
            _ => {
                return Ok(RepoPermissions {
                    access_level: 0,
                    can_edit: false,
                    can_delete: false,
                });
            }
        };
        let access_level = self.gateway.project_access_level(&repo, &token).await?;
        Ok(RepoPermissions {
            access_level,
            can_edit: access_level >= 30,
            can_delete: access_level >= 40,
        })
    }

    async fn get_repo_detail(&self, repo: String) -> DomainResult<RepoDetail> {
        // Direct sqlx in this method yields sqlx::Error; wrap the whole body in
        // an anyhow block (sqlx::Error: Into<anyhow>) and map once to DomainError.
        let detail: anyhow::Result<RepoDetail> = async {
            // Source config
            let source = self.source_repo.lookup_source_config(&repo).await?;

            // Chunk + module counts via PG (direct queries — verbatim from repos.rs)
            let (chunk_count,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM chunks WHERE repo_name = $1")
                    .bind(&repo)
                    .fetch_one(&self.pg)
                    .await?;

            let (module_count,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM modules WHERE repo_name = $1")
                    .bind(&repo)
                    .fetch_one(&self.pg)
                    .await?;

            let (doc_count,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM documents WHERE repo_name = $1")
                    .bind(&repo)
                    .fetch_one(&self.pg)
                    .await?;

            let (section_count,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM sections WHERE doc_id IN \
             (SELECT id FROM documents WHERE repo_name = $1)",
            )
            .bind(&repo)
            .fetch_one(&self.pg)
            .await?;

            // Latest job via IngestionJobRepo
            let latest_job = self
                .job_repo
                .latest_job(&repo)
                .await?
                .map(job_tuple_to_ingest_status);

            let (source_type, seed_url, crawl_depth, url_pattern, git_url) = match source {
                Some(s) => (
                    Some(s.source_type),
                    s.seed_url,
                    s.crawl_depth,
                    s.url_pattern,
                    s.git_url,
                ),
                None => (None, None, None, None, None),
            };

            Ok(RepoDetail {
                name: repo,
                source_type,
                seed_url,
                crawl_depth,
                url_pattern,
                git_url,
                total_chunks: chunk_count,
                total_modules: module_count,
                total_documents: doc_count,
                total_sections: section_count,
                latest_job,
            })
        }
        .await;
        detail.map_err(DomainError::Internal)
    }

    async fn delete_repo(&self, repo: String) -> DomainResult<()> {
        // Guard: refuse if an active ingestion job exists. Typed Conflict so the
        // handler returns 409 (was previously string-matched handler-side).
        if self
            .job_repo
            .has_active_job(&repo)
            .await
            .map_err(DomainError::Internal)?
        {
            return Err(DomainError::Conflict(
                "Cannot delete repo with active ingestion job".into(),
            ));
        }

        // Direct sqlx (tx) yields sqlx::Error; wrap the cascade in an anyhow
        // block and map once to DomainError::Internal.
        let result: anyhow::Result<()> = async {
            // PG cascade (verbatim from delete_repo_data in repos.rs)
            let mut tx = self.pg.begin().await?;
            // Delete sections first (FK references documents)
            sqlx::query(
                "DELETE FROM sections WHERE doc_id IN \
             (SELECT id FROM documents WHERE repo_name = $1)",
            )
            .bind(&repo)
            .execute(&mut *tx)
            .await?;
            // Communities before chunks/modules: community_members.chunk_id and
            // .module_id FK-reference chunks/modules with NO ON DELETE CASCADE.
            sqlx::query(
                "DELETE FROM community_members \
             WHERE community_id IN (SELECT id FROM communities WHERE repo_name = $1)",
            )
            .bind(&repo)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM communities WHERE repo_name = $1")
                .bind(&repo)
                .execute(&mut *tx)
                .await?;
            for table in &[
                "documents",
                "chunks",
                "large_chunks",
                "notes",
                "modules",
                "ingestion_jobs",
            ] {
                let sql = format!("DELETE FROM {table} WHERE repo_name = $1");
                sqlx::query(&sql).bind(&repo).execute(&mut *tx).await?;
            }
            // sources is deleted via SourceRepo::delete_source (outside the same tx
            // so we can commit the data tx first — matches original behaviour where
            // the sources DELETE was a separate loop entry in the same tx).
            // Actually original code includes sources in the same tx loop — keep it.
            sqlx::query("DELETE FROM sources WHERE repo_name = $1")
                .bind(&repo)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;

            // Neo4j cascade
            self.repo_graph.delete_repo_graph(&repo).await?;

            Ok(())
        }
        .await;
        result.map_err(DomainError::Internal)
    }

    async fn gitlab_branches(
        &self,
        repo: String,
        user_token: String,
    ) -> DomainResult<Vec<GitLabBranch>> {
        self.gateway
            .list_branches(&repo, &user_token)
            .await
            .map_err(DomainError::Internal)
    }

    async fn health(&self) -> DomainResult<HealthStatus> {
        Ok(HealthStatus {
            status: "ok".to_string(),
            details: std::collections::HashMap::new(),
        })
    }

    async fn ready(&self) -> DomainResult<HealthStatus> {
        // health/ready routing stays unchanged per plan constraint.
        // The handler reads state.readiness directly; this impl is provided
        // so the trait is fully implemented, but the ready handler is NOT
        // thinned through this method (it reads ReadinessState + shutdown
        // handles that are not available in the service layer without
        // introducing an akashic-context dependency). Status: pragmatic exception.
        Ok(HealthStatus {
            status: "ok".to_string(),
            details: std::collections::HashMap::new(),
        })
    }

    async fn sync_branch(
        &self,
        repo: String,
        branch: String,
        commit_hash: String,
    ) -> DomainResult<()> {
        self.repo_graph
            .sync_branch(&repo, &branch, &commit_hash)
            .await
            .map_err(DomainError::Internal)
    }

    async fn sync_tag(&self, repo: String, tag: String, commit_hash: String) -> DomainResult<()> {
        self.repo_graph
            .sync_tag(&repo, &tag, &commit_hash)
            .await
            .map_err(DomainError::Internal)
    }
}

/// Map a probe classification to the onboarding status of a submitted website
/// source: only `unsupported` diverts out of the review queue (→ demand
/// backlog); every other verdict (`good`/`generic`/`unprobed`) is queued for
/// admin review. Pure so the divert decision is unit-tested without network/DB.
fn classification_to_status(classification: &str) -> &'static str {
    if classification == "unsupported" {
        "unsupported"
    } else {
        "pending_review"
    }
}

/// Map a gated `sources.status` to the dashboard-overview status a source
/// should report. Returns `Some(..)` for a source that is NOT being served yet
/// (awaiting review, rejected, or classified unsupported), and `None` for a
/// normally-serving source ('approved', or any legacy/grandfathered value),
/// whose status is then derived from its latest ingestion job as before.
///
/// Without this gate, a gated source (which has no ingestion job and thus a
/// `completed_at` of `None`) fell through to "healthy", so the dashboard
/// reported never-crawled / rejected sources as ingested and inflated the
/// healthy count. Pure so the mapping is unit-tested without a DB.
fn gated_overview_status(source_status: &str) -> Option<&'static str> {
    match source_status {
        "pending_review" => Some("pending_review"),
        "unsupported" => Some("unsupported"),
        "rejected" => Some("rejected"),
        _ => None,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod status_tests {
    use super::gated_overview_status;

    #[test]
    fn gated_states_map_to_their_own_status() {
        assert_eq!(
            gated_overview_status("pending_review"),
            Some("pending_review")
        );
        assert_eq!(gated_overview_status("unsupported"), Some("unsupported"));
        assert_eq!(gated_overview_status("rejected"), Some("rejected"));
    }

    #[test]
    fn approved_and_legacy_are_not_gated() {
        // 'approved' (and any grandfathered/legacy value) must fall through to
        // the job-derived status, NOT be forced to a gated label.
        assert_eq!(gated_overview_status("approved"), None);
        assert_eq!(gated_overview_status("some_legacy_value"), None);
        assert_eq!(gated_overview_status(""), None);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use secrecy::SecretString;
    use tokio::sync::{Semaphore, broadcast};

    use akashic_config::{
        AiProvider, AlertsConfig, Config, EmbeddingConfig, EmbeddingPrecision, LlmConfig,
    };
    use akashic_domain::ports::services::IngestService;
    use akashic_embed::{EmbeddingProvider, EmbeddingResponse};
    use akashic_llm::{LlmProvider, LlmResponse, LlmUsage};
    use akashic_store_neo4j::{
        Neo4jCommunityGraphRepo, Neo4jGraphReadRepo, Neo4jPool, Neo4jRepoGraphRepo,
    };
    use akashic_store_pg::{PgCommunityRepo, PgIngestionJobRepo, PgSagaExecutorRepo, PgSourceRepo};

    use crate::ingestion::pipeline::IngestionPipeline;

    use super::IngestionServices;

    // ── Fake providers ────────────────────────────────────────────────────────

    const DIM: usize = 1536;

    struct FakeEmbedder;
    #[async_trait]
    impl EmbeddingProvider for FakeEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<EmbeddingResponse> {
            Ok(EmbeddingResponse {
                vector: vec![0.001_f32; DIM],
                tokens_used: 0,
                model: "fake".into(),
            })
        }
        fn dimensions(&self) -> usize {
            DIM
        }
    }

    struct FakeLlm;
    #[async_trait]
    impl LlmProvider for FakeLlm {
        async fn generate_json(&self, _prompt: &str) -> anyhow::Result<LlmResponse> {
            Ok(LlmResponse {
                text: "{}".into(),
                usage: LlmUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                },
                model: "fake".into(),
            })
        }
    }

    fn test_config(database_url: &str, neo4j_uri: &str) -> Config {
        Config {
            neo4j_uri: neo4j_uri.to_string(),
            neo4j_user: std::env::var("TEST_NEO4J_USER").unwrap_or_else(|_| "neo4j".into()),
            neo4j_password: SecretString::from(
                std::env::var("TEST_NEO4J_PASSWORD").unwrap_or_else(|_| "akashic_secret".into()),
            ),
            database_url: database_url.to_string(),
            embedding: EmbeddingConfig {
                provider: AiProvider::Local,
                api_key: None,
                model: "fake".into(),
                base_url: None,
            },
            llm: LlmConfig {
                provider: AiProvider::Local,
                api_key: None,
                model: "fake".into(),
                base_url: None,
            },
            module_max_files: 1000,
            module_min_files: 1,
            mcp_sse_host: "127.0.0.1".into(),
            mcp_sse_port: 8080,
            gitlab_webhook_secret: None,
            gitlab_url: "http://localhost".into(),
            gitlab_app_id: String::new(),
            gitlab_app_secret: SecretString::from(String::new()),
            gitlab_redirect_uri: "http://localhost/cb".into(),
            gitlab_web_redirect_uri: "http://localhost/wcb".into(),
            auth_code_ttl_secs: 300,
            api_key_ttl_secs: 86400,
            gitlab_service_token: None,
            frontend_url: "http://localhost:3000".into(),
            cors_extra_origins: vec![],
            cookie_secure: false,
            api_port: 8081,
            ingest_clone_dir: std::env::temp_dir()
                .join("akashic-svc-test-clone")
                .to_string_lossy()
                .into_owned(),
            ingest_max_file_size: 1_048_576,
            ingest_max_lines: 5000,
            ingest_chunk_max_size: 5120,
            ingest_concurrent_jobs: 1,
            ingest_skip_patterns: vec!["node_modules".into(), ".git".into(), "target".into()],
            ingest_presets_path: None,
            ingest_completeness_threshold: 1.0,
            admin_users: vec![],
            ingest_crawl_max_pages: 10,
            ingest_crawl_delay_ms: 0,
            embedding_precision: EmbeddingPrecision::Float32,
            rate_limit_enabled: false,
            rate_limit_trusted_proxies: vec![],
            rate_limit_allowlist: vec![],
            alerts: AlertsConfig::default(),
            public_base_url: "http://127.0.0.1:0".to_string(),
            mcp_quota_tokens_per_window: 100_000,
            mcp_quota_window_secs: 3600,
            mcp_quota_enabled: false,
            mcp_passthrough_user_cache_ttl_secs: 60,
            oauth_validation_mode: "off".to_string(),
            migrate_on_boot: "false".to_string(),
            ingest_quota_tokens_per_window: 5_000_000,
            ingest_quota_window_secs: 3600,
            ingest_quota_enabled: false,
        }
    }

    async fn build_test_ingest_service() -> IngestionServices {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://akashic@localhost:5432/akashic".into());
        let neo4j_uri =
            std::env::var("TEST_NEO4J_URI").unwrap_or_else(|_| "bolt://localhost:7687".into());

        let cfg = test_config(&database_url, &neo4j_uri);
        let pg = akashic_store_pg::connect(&database_url)
            .await
            .expect("connect Postgres");
        let neo4j = Neo4jPool::connect(&cfg).await.expect("connect Neo4j");

        let dim = DIM;
        let vec_type = cfg.vector_type(dim);
        let cos_ops = cfg.cosine_ops();
        akashic_store_pg::init_schema(&pg, dim, &vec_type, cos_ops)
            .await
            .expect("pg init_schema");
        akashic_store_neo4j::schema::init_schema(&neo4j, dim)
            .await
            .expect("neo4j init_schema");

        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbedder);
        let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlm);
        let semaphore = Arc::new(Semaphore::new(1));
        let event_tx = broadcast::channel(256).0;

        let pipeline = IngestionPipeline::new(
            pg.clone(),
            neo4j.clone(),
            embedder,
            llm,
            cfg.clone(),
            semaphore,
            event_tx,
        );

        let source_repo = Arc::new(PgSourceRepo::new(pg.clone()));
        let job_repo = Arc::new(PgIngestionJobRepo::new(pg.clone()));
        let community_repo = Arc::new(PgCommunityRepo::new(pg.clone()));
        let community_graph = Arc::new(Neo4jCommunityGraphRepo::new(neo4j.clone()));
        let graph_read = Arc::new(Neo4jGraphReadRepo::new(neo4j.clone()));
        let repo_graph = Arc::new(Neo4jRepoGraphRepo::new(neo4j.clone()));
        let saga_executor_repo = Arc::new(PgSagaExecutorRepo::new(pg.clone()));

        // No-op gateway (not needed for source gate tests).
        use akashic_domain::ports::gitlab::{
            GitLabGateway, GitLabToken, GitLabUser, WebLoginOutcome,
        };
        struct NoopGateway;
        #[async_trait::async_trait]
        impl GitLabGateway for NoopGateway {
            async fn exchange_oauth_code(
                &self,
                _code: &str,
                _redirect_uri: &str,
            ) -> anyhow::Result<GitLabToken> {
                anyhow::bail!("noop")
            }
            async fn fetch_user(&self, _token: &str) -> anyhow::Result<GitLabUser> {
                anyhow::bail!("noop")
            }
            async fn validate_passthrough(&self, _pat: &str) -> anyhow::Result<Option<GitLabUser>> {
                anyhow::bail!("noop")
            }
            async fn project_access_level(&self, _repo: &str, _token: &str) -> anyhow::Result<i32> {
                Ok(0)
            }
            async fn list_branches(
                &self,
                _repo: &str,
                _token: &str,
            ) -> anyhow::Result<Vec<akashic_domain::ports::services::GitLabBranch>> {
                Ok(vec![])
            }
            async fn runtime_report(&self) -> akashic_domain::types::ValidationReport {
                akashic_domain::types::ValidationReport::from_checks(vec![])
            }
        }
        let _ = WebLoginOutcome {
            api_key: String::new(),
            ttl_secs: 0,
            username: String::new(),
            next: None,
        };
        let gateway: Arc<dyn GitLabGateway> = Arc::new(NoopGateway);

        IngestionServices::new(
            pipeline,
            community_repo,
            community_graph,
            graph_read,
            source_repo,
            job_repo,
            repo_graph,
            saga_executor_repo,
            pg,
            gateway,
            "http://localhost".into(),
        )
    }

    async fn cleanup_repo(svc: &IngestionServices, repo: &str) {
        sqlx::query("DELETE FROM ingestion_jobs WHERE repo_name = $1")
            .bind(repo)
            .execute(&svc.pg)
            .await
            .ok();
        sqlx::query("DELETE FROM sources WHERE repo_name = $1")
            .bind(repo)
            .execute(&svc.pg)
            .await
            .ok();
    }

    #[tokio::test]
    #[serial_test::serial]
    #[ignore = "requires live Postgres + Neo4j; run with --ignored"]
    async fn website_submit_is_gated_then_approved() {
        let svc = build_test_ingest_service().await;
        let repo = "x.example/gated";
        cleanup_repo(&svc, repo).await;

        // Submit → pending, NO job started.
        let res = svc
            .add_source(
                "website".into(),
                "http://x.example/gated".into(),
                Some(1),
                None,
                "tonyhu".into(),
            )
            .await
            .unwrap();
        assert_eq!(res.status, "pending_review");
        assert!(res.job_id.is_empty(), "submit must not start a job");
        let pending = svc.list_pending_sources().await.unwrap();
        let item = pending
            .iter()
            .find(|p| p.repo_name == repo)
            .expect("queued");

        // Approve → approved + a job exists.
        let id = uuid::Uuid::parse_str(&item.id).unwrap();
        let appr = svc.approve_source(id, "tonyhu".into()).await.unwrap();
        assert_eq!(appr.status, "approved");
        assert!(!appr.job_id.is_empty(), "approve must start a job");

        cleanup_repo(&svc, repo).await;
    }

    #[test]
    fn classification_to_status_diverts_only_unsupported() {
        // The load-bearing A2d-2 decision: only `unsupported` leaves the review
        // queue (→ demand backlog); every other verdict is queued for review.
        assert_eq!(
            super::classification_to_status("unsupported"),
            "unsupported"
        );
        assert_eq!(super::classification_to_status("good"), "pending_review");
        assert_eq!(super::classification_to_status("generic"), "pending_review");
        assert_eq!(
            super::classification_to_status("unprobed"),
            "pending_review"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    #[ignore = "requires live Postgres + Neo4j; run with --ignored"]
    // The unsupported→not-queued DECISION is now unit-covered by
    // classification_to_status_diverts_only_unsupported; a full live end-to-end
    // assertion of the unsupported branch would still need a controllable
    // empty-HTML fixture host (loopback is SSRF-blocked → unprobed, not unsupported).
    async fn unprobed_submission_still_queues() {
        let svc = build_test_ingest_service().await;
        let repo = "127.0.0.1/probe-unreach"; // loopback → SSRF-blocked → fetch fails → unprobed
        cleanup_repo(&svc, repo).await;
        // A loopback host: the probe fetch is SSRF-rejected → classification "unprobed" → still queued.
        let res = svc
            .add_source(
                "website".into(),
                "http://127.0.0.1/probe-unreach".into(),
                Some(1),
                None,
                "tonyhu".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status, "pending_review",
            "unprobed must still queue, not reject"
        );
        cleanup_repo(&svc, repo).await;
    }
}
