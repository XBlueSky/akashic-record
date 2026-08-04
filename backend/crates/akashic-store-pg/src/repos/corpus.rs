//! PostgreSQL adapter for `CorpusStore` (docs-corpus ingestion, C2).
//!
//! `corpus_versions` / `corpus_files` schema is created by C1
//! (`akashic-store-pg::schema::init_schema`).

use anyhow::{Context, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::corpus::{CorpusStore, InsertOutcome, NewCorpusVersion};
use akashic_domain::types::corpus::{CorpusFileRow, CorpusRepoSummary, NavTree};
use akashic_domain::types::{CorpusVersionMeta, DeriveStatus};

/// Row shape shared by every `corpus_versions` SELECT in this adapter —
/// matches the [`CorpusVersionMeta`] field order. The trailing `Option<String>`
/// is `manifest->>'index'` (jsonb arrow-arrow is nullable at the SQL type
/// level even though every corpus is guaranteed to have an index — see
/// `row_to_meta`'s `unwrap_or_default`).
type VersionRow = (
    Uuid,
    String,
    String,
    String,
    bool,
    bool,
    String,
    chrono::DateTime<chrono::Utc>,
    Option<i32>,
    Option<i32>,
    Option<String>,
);

fn row_to_meta(row: VersionRow) -> Result<CorpusVersionMeta> {
    let (
        id,
        repo_name,
        version,
        sha,
        is_latest,
        is_tagged,
        derive_status,
        ingested_at,
        page_count,
        asset_count,
        index_path,
    ) = row;
    let derive_status: DeriveStatus = derive_status
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))
        .context("corpus_versions.derive_status held an unrecognized value")?;
    Ok(CorpusVersionMeta {
        id,
        repo_name,
        version,
        sha,
        is_latest,
        is_tagged,
        derive_status,
        ingested_at,
        page_count,
        asset_count,
        // `manifest->>'index'` is contract-validated at publish time (every
        // corpus has an index page), so this NULL-typed-but-never-actually-NULL
        // column collapses to an empty string rather than staying an Option.
        index_path: index_path.unwrap_or_default(),
    })
}

const VERSION_COLUMNS: &str = "id, repo_name, version, sha, is_latest, is_tagged, derive_status, ingested_at, page_count, asset_count, manifest->>'index' AS index_path";

/// PostgreSQL adapter implementing [`CorpusStore`].
#[derive(Clone)]
pub struct PgCorpusStore {
    pool: PgPool,
}

impl PgCorpusStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn fetch_meta_by_repo_sha(
        &self,
        repo_name: &str,
        sha: &str,
    ) -> Result<CorpusVersionMeta> {
        let row: VersionRow = sqlx::query_as(&format!(
            "SELECT {VERSION_COLUMNS} FROM corpus_versions WHERE repo_name = $1 AND sha = $2"
        ))
        .bind(repo_name)
        .bind(sha)
        .fetch_one(&self.pool)
        .await
        .context("Failed to fetch existing corpus_versions row after unique-violation replay")?;
        row_to_meta(row)
    }
}

#[async_trait]
impl CorpusStore for PgCorpusStore {
    async fn insert_version(&self, v: NewCorpusVersion) -> Result<InsertOutcome> {
        let repo_name = v.manifest.repo.clone();
        let sha = v.manifest.sha.clone();

        let mut tx = self
            .pool
            .begin()
            .await
            .context("Failed to begin corpus insert_version transaction")?;

        // Flip the previous latest OFF before inserting the new row as
        // latest, both inside this transaction, so the partial unique index
        // `corpus_latest_one` never sees two `true` rows for this repo.
        sqlx::query(
            "UPDATE corpus_versions SET is_latest = false WHERE repo_name = $1 AND is_latest",
        )
        .bind(&repo_name)
        .execute(&mut *tx)
        .await
        .context("Failed to flip previous corpus latest off")?;

        let page_count = v.files.iter().filter(|f| f.is_markdown).count() as i32;
        let asset_count = v.files.iter().filter(|f| !f.is_markdown).count() as i32;
        let total_bytes: i64 = v.files.iter().map(|f| f.content.len() as i64).sum();

        let insert_result: std::result::Result<
            (Uuid, chrono::DateTime<chrono::Utc>, String),
            sqlx::Error,
        > = sqlx::query_as(
            "INSERT INTO corpus_versions \
                (repo_name, version, sha, manifest, nav, source, page_count, asset_count, total_bytes, is_latest, is_tagged) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, true, $10) \
             RETURNING id, ingested_at, derive_status",
        )
        .bind(&repo_name)
        .bind(&v.manifest.version)
        .bind(&sha)
        .bind(sqlx::types::Json(&v.manifest))
        .bind(sqlx::types::Json(&v.nav))
        .bind(v.source.as_str())
        .bind(page_count)
        .bind(asset_count)
        .bind(total_bytes)
        .bind(v.is_tagged)
        .fetch_one(&mut *tx)
        .await;

        let (version_id, ingested_at, derive_status) = match insert_result {
            Ok(row) => row,
            Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
                // The only unique constraint this INSERT can hit is
                // `corpus_versions_repo_name_sha_key` (the id column is a
                // fresh gen_random_uuid() every time) — this is a replay of
                // an already-ingested (repo, sha), not an error. Roll back
                // (undoing the latest-flip above) and return the existing row.
                tx.rollback()
                    .await
                    .context("Failed to roll back after (repo, sha) unique violation")?;
                let existing = self.fetch_meta_by_repo_sha(&repo_name, &sha).await?;
                return Ok(InsertOutcome::Existing(existing));
            }
            Err(e) => return Err(e).context("Failed to insert corpus_versions row"),
        };

        for f in &v.files {
            let content_hash = format!("{:x}", Sha256::digest(&f.content));
            let size_bytes = f.content.len() as i64;
            sqlx::query(
                "INSERT INTO corpus_files (version_id, path, content, content_hash, size_bytes, is_markdown) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(version_id)
            .bind(&f.path)
            .bind(&f.content)
            .bind(&content_hash)
            .bind(size_bytes)
            .bind(f.is_markdown)
            .execute(&mut *tx)
            .await
            .context("Failed to insert corpus_files row")?;
        }

        tx.commit()
            .await
            .context("Failed to commit corpus insert_version transaction")?;

        let derive_status: DeriveStatus = derive_status
            .parse()
            .map_err(|e: String| anyhow::anyhow!(e))
            .context("corpus_versions.derive_status held an unrecognized value")?;

        Ok(InsertOutcome::Inserted(CorpusVersionMeta {
            id: version_id,
            repo_name,
            version: v.manifest.version,
            index_path: v.manifest.index,
            sha,
            is_latest: true,
            is_tagged: v.is_tagged,
            derive_status,
            ingested_at,
            page_count: Some(page_count),
            asset_count: Some(asset_count),
        }))
    }

    async fn resolve_version(
        &self,
        repo: &str,
        selector: &str,
    ) -> Result<Option<CorpusVersionMeta>> {
        if selector == "latest" {
            let row: Option<VersionRow> = sqlx::query_as(&format!(
                "SELECT {VERSION_COLUMNS} FROM corpus_versions WHERE repo_name = $1 AND is_latest LIMIT 1"
            ))
            .bind(repo)
            .fetch_optional(&self.pool)
            .await
            .context("Failed to resolve \"latest\" corpus version")?;
            return row.map(row_to_meta).transpose();
        }

        let exact: Option<VersionRow> = sqlx::query_as(&format!(
            "SELECT {VERSION_COLUMNS} FROM corpus_versions \
             WHERE repo_name = $1 AND version = $2 \
             ORDER BY ingested_at DESC LIMIT 1"
        ))
        .bind(repo)
        .bind(selector)
        .fetch_optional(&self.pool)
        .await
        .context("Failed to resolve corpus version by exact version string")?;
        if let Some(row) = exact {
            return Ok(Some(row_to_meta(row)?));
        }

        if selector.len() >= 7 {
            // Prefix match via `left(sha, len)` rather than `LIKE` so the
            // selector can't be (mis)interpreted as a LIKE pattern. Multiple
            // matches resolve deterministically to the earliest-ingested row
            // (see CorpusStore::resolve_version doc comment).
            let by_sha: Option<VersionRow> = sqlx::query_as(&format!(
                "SELECT {VERSION_COLUMNS} FROM corpus_versions \
                 WHERE repo_name = $1 AND left(sha, char_length($2)) = $2 \
                 ORDER BY ingested_at ASC LIMIT 1"
            ))
            .bind(repo)
            .bind(selector)
            .fetch_optional(&self.pool)
            .await
            .context("Failed to resolve corpus version by sha prefix")?;
            if let Some(row) = by_sha {
                return Ok(Some(row_to_meta(row)?));
            }
        }

        Ok(None)
    }

    async fn list_repos(&self) -> Result<Vec<CorpusRepoSummary>> {
        // Flat tuple (not `(VersionRow, i64)`): sqlx's tuple `FromRow` decodes
        // each element against one column by position, it does not support a
        // nested tuple standing in for several columns.
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            Uuid,
            String,
            String,
            String,
            bool,
            bool,
            String,
            chrono::DateTime<chrono::Utc>,
            Option<i32>,
            Option<i32>,
            Option<String>,
            i64,
        )> = sqlx::query_as(
            "SELECT cv.id, cv.repo_name, cv.version, cv.sha, cv.is_latest, cv.is_tagged, \
                    cv.derive_status, cv.ingested_at, cv.page_count, cv.asset_count, \
                    cv.manifest->>'index' AS index_path, \
                    (SELECT COUNT(*) FROM corpus_versions v2 WHERE v2.repo_name = cv.repo_name) AS version_count \
             FROM corpus_versions cv \
             WHERE cv.is_latest \
             ORDER BY cv.repo_name",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to list corpus repos")?;

        rows.into_iter()
            .map(
                |(
                    id,
                    repo_name,
                    version,
                    sha,
                    is_latest,
                    is_tagged,
                    derive_status,
                    ingested_at,
                    page_count,
                    asset_count,
                    index_path,
                    version_count,
                )| {
                    let latest = row_to_meta((
                        id,
                        repo_name.clone(),
                        version,
                        sha,
                        is_latest,
                        is_tagged,
                        derive_status,
                        ingested_at,
                        page_count,
                        asset_count,
                        index_path,
                    ))?;
                    Ok(CorpusRepoSummary {
                        repo_name,
                        version_count,
                        latest,
                    })
                },
            )
            .collect()
    }

    async fn list_versions(&self, repo: &str) -> Result<Vec<CorpusVersionMeta>> {
        let rows: Vec<VersionRow> = sqlx::query_as(&format!(
            "SELECT {VERSION_COLUMNS} FROM corpus_versions WHERE repo_name = $1 ORDER BY ingested_at DESC"
        ))
        .bind(repo)
        .fetch_all(&self.pool)
        .await
        .context("Failed to list corpus versions")?;

        rows.into_iter().map(row_to_meta).collect()
    }

    async fn get_version_by_id(&self, version_id: Uuid) -> Result<Option<CorpusVersionMeta>> {
        let row: Option<VersionRow> = sqlx::query_as(&format!(
            "SELECT {VERSION_COLUMNS} FROM corpus_versions WHERE id = $1"
        ))
        .bind(version_id)
        .fetch_optional(&self.pool)
        .await
        .context("Failed to fetch corpus version by id")?;
        row.map(row_to_meta).transpose()
    }

    async fn get_file(&self, version_id: Uuid, path: &str) -> Result<Option<CorpusFileRow>> {
        let row: Option<(String, Vec<u8>, String, i64, bool)> = sqlx::query_as(
            "SELECT path, content, content_hash, size_bytes, is_markdown \
             FROM corpus_files WHERE version_id = $1 AND path = $2",
        )
        .bind(version_id)
        .bind(path)
        .fetch_optional(&self.pool)
        .await
        .context("Failed to fetch corpus file")?;

        Ok(row.map(
            |(path, content, content_hash, size_bytes, is_markdown)| CorpusFileRow {
                path,
                content,
                content_hash,
                size_bytes,
                is_markdown,
            },
        ))
    }

    async fn get_nav(&self, version_id: Uuid) -> Result<Option<NavTree>> {
        let row: Option<(sqlx::types::Json<NavTree>,)> =
            sqlx::query_as("SELECT nav FROM corpus_versions WHERE id = $1")
                .bind(version_id)
                .fetch_optional(&self.pool)
                .await
                .context("Failed to fetch corpus version nav")?;

        Ok(row.map(|(nav,)| nav.0))
    }

    async fn list_md_paths(&self, version_id: Uuid) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT path FROM corpus_files WHERE version_id = $1 AND is_markdown ORDER BY path",
        )
        .bind(version_id)
        .fetch_all(&self.pool)
        .await
        .context("Failed to list corpus markdown paths")?;

        Ok(rows.into_iter().map(|(path,)| path).collect())
    }

    async fn set_derive_status(
        &self,
        version_id: Uuid,
        status: DeriveStatus,
        error: Option<&str>,
        job_id: Option<Uuid>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE corpus_versions SET derive_status = $2, derive_error = $3, derive_job_id = $4 \
             WHERE id = $1",
        )
        .bind(version_id)
        .bind(status.as_str())
        .bind(error)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .context("Failed to set corpus derive status")?;
        Ok(())
    }

    async fn claim_derive_running(&self, version_id: Uuid, job_id: Option<Uuid>) -> Result<bool> {
        // Single conditional UPDATE — the WHERE clause is evaluated and the
        // row locked atomically by Postgres, so two concurrent callers can
        // never both see rows_affected() == 1: whichever's UPDATE commits
        // first flips the status to 'running', and the other's WHERE clause
        // then fails to match (row-level lock serializes them, it does not
        // let a second transaction proceed on a stale read).
        let result = sqlx::query(
            "UPDATE corpus_versions \
             SET derive_status = 'running', derive_error = NULL, derive_job_id = $2 \
             WHERE id = $1 AND derive_status IS DISTINCT FROM 'running'",
        )
        .bind(version_id)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .context("Failed to claim corpus derive_status=running")?;
        Ok(result.rows_affected() > 0)
    }
}
