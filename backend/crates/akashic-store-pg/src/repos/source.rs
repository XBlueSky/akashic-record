//! PostgreSQL adapter for [`SourceRepo`].
//!
//! All SQL is moved verbatim from the original call-sites in:
//! - `akashic-server::api::routes::repos` (lookup_source_config, delete)
//! - `akashic-server::api::routes::ingestion` (upsert gitlab/website, list)

use anyhow::Result;
use async_trait::async_trait;
use sqlx::PgPool;

use akashic_domain::ports::source::{
    DemandRow, SourceConfig, SourceOverviewRow, SourceRepo, SourceRow, SourceTypeRow,
    WebsiteSourceUpsert,
};

/// PostgreSQL adapter implementing [`SourceRepo`].
#[derive(Clone)]
pub struct PgSourceRepo {
    pool: PgPool,
}

impl PgSourceRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SourceRepo for PgSourceRepo {
    async fn list_source_types(&self) -> Result<Vec<SourceTypeRow>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT repo_name, source_type FROM sources")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(repo_name, source_type)| SourceTypeRow {
                repo_name,
                source_type,
            })
            .collect())
    }

    async fn lookup_source_config(&self, repo: &str) -> Result<Option<SourceConfig>> {
        let row: Option<(
            String,
            Option<String>,
            Option<i16>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT source_type, seed_url, crawl_depth, url_pattern, git_url \
                 FROM sources WHERE repo_name = $1",
        )
        .bind(repo)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(
            |(source_type, seed_url, crawl_depth, url_pattern, git_url)| SourceConfig {
                source_type,
                seed_url,
                crawl_depth,
                url_pattern,
                git_url,
            },
        ))
    }

    async fn upsert_source_gitlab(&self, repo: &str, git_url: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO sources (repo_name, source_type, git_url) \
             VALUES ($1, 'gitlab', $2) \
             ON CONFLICT (repo_name) DO UPDATE SET \
               source_type = 'gitlab', \
               git_url = EXCLUDED.git_url, \
               updated_at = now()",
        )
        .bind(repo)
        .bind(git_url)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete_source(&self, repo: &str) -> Result<()> {
        sqlx::query("DELETE FROM sources WHERE repo_name = $1")
            .bind(repo)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn upsert_website_source(&self, u: WebsiteSourceUpsert<'_>) -> Result<uuid::Uuid> {
        let row: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO sources \
               (repo_name, source_type, seed_url, crawl_depth, url_pattern, status, submitter, adapter_id, classification, probe_note) \
             VALUES ($1, 'website', $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (repo_name) DO UPDATE SET \
               source_type='website', seed_url=EXCLUDED.seed_url, crawl_depth=EXCLUDED.crawl_depth, \
               url_pattern=EXCLUDED.url_pattern, status=EXCLUDED.status, submitter=EXCLUDED.submitter, \
               adapter_id=EXCLUDED.adapter_id, classification=EXCLUDED.classification, probe_note=EXCLUDED.probe_note, \
               reviewed_by=NULL, reviewed_at=NULL, updated_at=now() \
             RETURNING id",
        )
        .bind(u.repo).bind(u.seed_url).bind(u.crawl_depth).bind(u.url_pattern)
        .bind(u.status).bind(u.submitter).bind(u.adapter_id)
        .bind(u.classification).bind(u.probe_note)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    async fn list_demand(&self) -> Result<Vec<DemandRow>> {
        let rows = sqlx::query_as::<_, (String, i64, Option<String>)>(
            "SELECT split_part(repo_name, '/', 1) AS host, count(*) AS n, max(probe_note) AS note \
             FROM sources WHERE status = 'unsupported' GROUP BY 1 ORDER BY n DESC, host",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(host, count, sample_note)| DemandRow {
                host,
                count,
                sample_note,
            })
            .collect())
    }

    async fn get_source(&self, id: uuid::Uuid) -> Result<Option<SourceRow>> {
        let row = sqlx::query_as::<_, (uuid::Uuid, String, String, Option<String>, Option<i16>, Option<String>, String, Option<String>, Option<String>, Option<String>, Option<String>)>(
            "SELECT id, repo_name, source_type, seed_url, crawl_depth, url_pattern, status, submitter, adapter_id, classification, probe_note \
             FROM sources WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| SourceRow {
            id: r.0,
            repo_name: r.1,
            source_type: r.2,
            seed_url: r.3,
            crawl_depth: r.4,
            url_pattern: r.5,
            status: r.6,
            submitter: r.7,
            adapter_id: r.8,
            classification: r.9,
            probe_note: r.10,
        }))
    }

    async fn set_source_status(
        &self,
        id: uuid::Uuid,
        status: &str,
        reviewed_by: &str,
    ) -> Result<bool> {
        let res = sqlx::query(
            "UPDATE sources SET status = $2, reviewed_by = $3, reviewed_at = now(), updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(status)
        .bind(reviewed_by)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn list_pending_sources(&self) -> Result<Vec<SourceRow>> {
        let rows = sqlx::query_as::<_, (uuid::Uuid, String, String, Option<String>, Option<i16>, Option<String>, String, Option<String>, Option<String>, Option<String>, Option<String>)>(
            "SELECT id, repo_name, source_type, seed_url, crawl_depth, url_pattern, status, submitter, adapter_id, classification, probe_note \
             FROM sources WHERE status = 'pending_review' ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| SourceRow {
                id: r.0,
                repo_name: r.1,
                source_type: r.2,
                seed_url: r.3,
                crawl_depth: r.4,
                url_pattern: r.5,
                status: r.6,
                submitter: r.7,
                adapter_id: r.8,
                classification: r.9,
                probe_note: r.10,
            })
            .collect())
    }

    async fn sources_overview(&self) -> Result<Vec<SourceOverviewRow>> {
        // SQL moved verbatim from
        // akashic-server::api::routes::ingestion::sources_overview (LATERAL join).
        type RawRow = (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<i32>,
            Option<i32>,
            Option<String>,
            Option<String>,
            bool,
            i64,
            i64,
            i64,
        );
        let rows: Vec<RawRow> = sqlx::query_as(
            "SELECT
                s.repo_name,
                s.source_type,
                s.status,
                j.status,
                j.git_ref,
                j.processed_files,
                j.total_files,
                j.error_message,
                to_char(j.completed_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),
                COALESCE(j.checkpoint_data IS NOT NULL AND j.status = 'failed', false),
                (SELECT count(*) FROM chunks WHERE repo_name = s.repo_name),
                (SELECT count(*) FROM modules WHERE repo_name = s.repo_name),
                (SELECT count(*) FROM notes WHERE repo_name = s.repo_name)
            FROM sources s
            LEFT JOIN LATERAL (
                SELECT * FROM ingestion_jobs
                WHERE repo_name = s.repo_name
                ORDER BY started_at DESC LIMIT 1
            ) j ON true
            ORDER BY s.repo_name",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(
                    repo_name,
                    source_type,
                    source_status,
                    job_status,
                    git_ref,
                    processed_files,
                    total_files,
                    error_message,
                    completed_at,
                    can_resume,
                    chunk_count,
                    module_count,
                    note_count,
                )| SourceOverviewRow {
                    repo_name,
                    source_type,
                    source_status,
                    job_status,
                    git_ref,
                    processed_files,
                    total_files,
                    error_message,
                    completed_at,
                    can_resume,
                    chunk_count,
                    module_count,
                    note_count,
                },
            )
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use akashic_domain::ports::source::WebsiteSourceUpsert;

    #[tokio::test]
    #[ignore = "requires live Postgres; run with --ignored"]
    async fn pending_source_lifecycle() {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://akashic@localhost:5432/akashic".into());
        let pool = crate::connect(&url).await.unwrap();
        let vt = "vector(1536)".to_string();
        crate::init_schema(&pool, 1536, &vt, "vector_cosine_ops")
            .await
            .unwrap();
        let repo = "a2d1-test/lifecycle";
        sqlx::query("DELETE FROM sources WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();

        let r = PgSourceRepo::new(pool.clone());
        let id = r
            .upsert_website_source(WebsiteSourceUpsert {
                repo,
                seed_url: "http://x.example/docs",
                crawl_depth: Some(2),
                url_pattern: None,
                submitter: "tonyhu",
                status: "pending_review",
                adapter_id: None,
                classification: "unknown",
                probe_note: "",
            })
            .await
            .unwrap();

        let got = r.get_source(id).await.unwrap().expect("row exists");
        assert_eq!(got.status, "pending_review");
        assert_eq!(got.submitter.as_deref(), Some("tonyhu"));

        let pending = r.list_pending_sources().await.unwrap();
        assert!(pending.iter().any(|s| s.id == id));

        assert!(r.set_source_status(id, "approved", "tonyhu").await.unwrap());
        assert_eq!(r.get_source(id).await.unwrap().unwrap().status, "approved");
        assert!(
            r.list_pending_sources()
                .await
                .unwrap()
                .iter()
                .all(|s| s.id != id)
        );

        sqlx::query("DELETE FROM sources WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires live Postgres; run with --ignored"]
    async fn website_upsert_with_probe_and_demand() {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://akashic@localhost:5432/akashic".into());
        let pool = crate::connect(&url).await.unwrap();
        let vt = "vector(1536)".to_string();
        crate::init_schema(&pool, 1536, &vt, "vector_cosine_ops")
            .await
            .unwrap();
        let r = PgSourceRepo::new(pool.clone());
        for repo in ["a2d2-test.example/x", "a2d2-test.example/y"] {
            sqlx::query("DELETE FROM sources WHERE repo_name=$1")
                .bind(repo)
                .execute(&pool)
                .await
                .unwrap();
        }

        let pid = r
            .upsert_website_source(WebsiteSourceUpsert {
                repo: "a2d2-test.example/x",
                seed_url: "http://a2d2-test.example/x",
                crawl_depth: Some(1),
                url_pattern: None,
                submitter: "tonyhu",
                status: "pending_review",
                adapter_id: Some("vitepress-llms"),
                classification: "good",
                probe_note: "matched",
            })
            .await
            .unwrap();
        let got = r.get_source(pid).await.unwrap().unwrap();
        assert_eq!(got.status, "pending_review");
        assert_eq!(got.classification.as_deref(), Some("good"));
        assert_eq!(got.adapter_id.as_deref(), Some("vitepress-llms"));

        r.upsert_website_source(WebsiteSourceUpsert {
            repo: "a2d2-test.example/y",
            seed_url: "http://a2d2-test.example/y",
            crawl_depth: None,
            url_pattern: None,
            submitter: "alice",
            status: "unsupported",
            adapter_id: None,
            classification: "unsupported",
            probe_note: "JS shell",
        })
        .await
        .unwrap();
        let demand = r.list_demand().await.unwrap();
        let entry = demand
            .iter()
            .find(|d| d.host == "a2d2-test.example")
            .expect("demand entry");
        assert!(entry.count >= 1);

        for repo in ["a2d2-test.example/x", "a2d2-test.example/y"] {
            sqlx::query("DELETE FROM sources WHERE repo_name=$1")
                .bind(repo)
                .execute(&pool)
                .await
                .unwrap();
        }
    }
}
