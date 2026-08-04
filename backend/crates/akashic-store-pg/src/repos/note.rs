//! PostgreSQL adapters for `NoteRepo` and `NoteHealthRepo`.
//!
//! All SQL is moved verbatim from the original call-sites in:
//!
//! - `akashic-curation::notes::dedup` (`find_similar_by_embedding`)
//! - `akashic-curation::notes::health` (`get_all_with_symbols`,
//!   `compute_embedding_divergence`, `update_staleness`, `auto_archive_stale`,
//!   count queries, `get_stale_notes_above_threshold`)
//! - `akashic-curation::notes::memory_stack` (`get_notes_by_category`,
//!   `get_top_notes_by_access`, `count_notes_for_saga`, `get_saga_notes_timeline`)

use anyhow::Result;
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::{
    NoteCrudRow, NoteHealthRepo, NoteInsertRow, NoteListCrudRow, NotePatchRow, NoteRepo,
};
use akashic_domain::types::{
    DedupCandidate, NoteBriefRow, NoteContentRow, NoteDetailRow, NoteFullRow, NoteSearchRow,
    NoteWithSymbols, SagaNoteDbRow, ScoutNoteRow, StaleNoteDbRow, TopNoteDbRow,
};

// ── PgNoteRepo ────────────────────────────────────────────────────────────────

/// PostgreSQL adapter implementing [`NoteRepo`].
#[derive(Clone)]
pub struct PgNoteRepo {
    pool: PgPool,
}

impl PgNoteRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl NoteRepo for PgNoteRepo {
    async fn find_similar_by_embedding(
        &self,
        repo_name: &str,
        embedding: &[f32],
    ) -> Result<Option<DedupCandidate>> {
        let vec = pgvector::Vector::from(embedding.to_vec());

        let row: Option<(Uuid, Option<String>, String, f64, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, title, category, \
                        1 - (embedding <=> $1::vector) AS similarity, \
                        saga_id \
                 FROM notes \
                 WHERE repo_name = $2 AND invalid_at IS NULL AND archived = false \
                 ORDER BY embedding <=> $1::vector \
                 LIMIT 1",
        )
        .bind(&vec)
        .bind(repo_name)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(
            |(note_id, title, category, similarity, saga_id)| DedupCandidate {
                note_id,
                title,
                category,
                similarity,
                saga_id,
            },
        ))
    }

    async fn get_all_with_symbols(&self, repo_name: &str) -> Result<Vec<NoteWithSymbols>> {
        let rows: Vec<(Uuid, Vec<String>)> = sqlx::query_as(
            "SELECT id, coalesce(related_symbols, '{}') \
             FROM notes WHERE repo_name = $1 AND (archived IS NULL OR archived = false)",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, symbols)| NoteWithSymbols { id, symbols })
            .collect())
    }

    async fn check_symbol_existence(&self, repo_name: &str, symbol_name: &str) -> Result<i64> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM chunks WHERE repo_name = $1 AND name = $2")
                .bind(repo_name)
                .bind(symbol_name)
                .fetch_one(&self.pool)
                .await?;
        Ok(count)
    }

    async fn compute_embedding_divergence(
        &self,
        note_id: Uuid,
        repo_name: &str,
        symbol_names: &[String],
    ) -> Result<Option<f64>> {
        let divergence: Option<(Option<f64>,)> = sqlx::query_as(
            "SELECT (1.0 - AVG(1.0 - (n.embedding <=> c.embedding)))::float8 \
             FROM notes n, chunks c \
             WHERE n.id = $1 AND c.repo_name = $2 \
               AND c.name = ANY($3) \
               AND n.embedding IS NOT NULL AND c.embedding IS NOT NULL",
        )
        .bind(note_id)
        .bind(repo_name)
        .bind(symbol_names)
        .fetch_optional(&self.pool)
        .await?;

        // The aggregate (AVG, no GROUP BY) always returns exactly one row; when
        // zero chunks match, AVG is SQL NULL, so the column is NULL. Decode it
        // as nullable and flatten: zero-match → None (there is nothing to
        // compute content-divergence against; "the code is gone" is the
        // symbol_deleted signal's job, not content_diverged's).
        Ok(divergence.and_then(|(v,)| v))
    }

    async fn search_notes_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> Result<Vec<NoteSearchRow>> {
        // SQL moved verbatim from akashic-retrieval::graphrag::mod.rs search_human_space.
        let vec = pgvector::Vector::from(query_vec.to_vec());
        let rows: Vec<(Uuid, String, f64)> = if let Some(r) = repo {
            sqlx::query_as(
                "SELECT id, title, 1 - (embedding <=> $1::vector) AS score \
                 FROM notes WHERE repo_name = $2 \
                   AND (archived IS NULL OR archived = false) \
                 ORDER BY embedding <=> $1::vector LIMIT $3",
            )
            .bind(&vec)
            .bind(r)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, title, 1 - (embedding <=> $1::vector) AS score \
                 FROM notes WHERE (archived IS NULL OR archived = false) \
                 ORDER BY embedding <=> $1::vector LIMIT $2",
            )
            .bind(&vec)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows
            .into_iter()
            .map(|(id, title, score)| NoteSearchRow {
                id,
                title,
                score: score as f32,
            })
            .collect())
    }

    async fn scout_search_notes_by_vector(
        &self,
        query_vec: &[f32],
        repo: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ScoutNoteRow>> {
        // SQL moved verbatim from akashic-server::api::routes::search::unified_search
        // (note layer): NO archived filter (intentional — Scout shows archived
        // notes), single `$2::text IS NULL OR repo_name = $2` repo filter.
        let vec = pgvector::Vector::from(query_vec.to_vec());
        let rows: Vec<(Uuid, String, Option<String>, Option<String>, String, f64)> =
            sqlx::query_as(
                "SELECT id, category, title, summary, repo_name, \
                        1 - (embedding <=> $1::vector) AS score \
                 FROM notes \
                 WHERE ($2::text IS NULL OR repo_name = $2) \
                 ORDER BY embedding <=> $1::vector \
                 LIMIT $3",
            )
            .bind(&vec)
            .bind(repo)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, category, title, summary, repo_name, score)| ScoutNoteRow {
                    id,
                    category,
                    title,
                    summary,
                    repo_name,
                    score,
                },
            )
            .collect())
    }

    async fn search_notes_by_bm25(
        &self,
        query: &str,
        repo: Option<&str>,
        limit: i64,
    ) -> Result<Vec<NoteSearchRow>> {
        // SQL moved verbatim from akashic-retrieval::graphrag::mod.rs search_note_bm25.
        let rows: Vec<(Uuid, String, f64)> = sqlx::query_as(
            "SELECT id, coalesce(title, 'Untitled'), \
                    ts_rank(content_tsv, plainto_tsquery('english', $1))::float8 AS score \
             FROM notes \
             WHERE content_tsv @@ plainto_tsquery('english', $1) \
               AND ($2::text IS NULL OR repo_name = $2) \
               AND (archived IS NULL OR archived = false) \
             ORDER BY score DESC LIMIT $3",
        )
        .bind(query)
        .bind(repo)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, title, score)| NoteSearchRow {
                id,
                title,
                score: score as f32,
            })
            .collect())
    }

    async fn fetch_note_content(&self, note_id: Uuid) -> Result<Option<NoteContentRow>> {
        // SQL moved verbatim from akashic-retrieval::graphrag::mod.rs fetch_full_node
        // Human branch.
        let row: Option<(String, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT content, author, category FROM notes WHERE id = $1")
                .bind(note_id)
                .fetch_optional(&self.pool)
                .await?;

        Ok(row.map(|(content, author, category)| NoteContentRow {
            content,
            author,
            category,
        }))
    }

    async fn fetch_notes_detail_batch(&self, note_ids: &[Uuid]) -> Result<Vec<NoteDetailRow>> {
        if note_ids.is_empty() {
            return Ok(Vec::new());
        }
        // SQL moved verbatim from graph/module.rs get_module_detail.
        let rows: Vec<(Uuid, Option<String>, String, Option<String>)> =
            sqlx::query_as("SELECT id, title, category, summary FROM notes WHERE id = ANY($1)")
                .bind(note_ids)
                .fetch_all(&self.pool)
                .await?;

        Ok(rows
            .into_iter()
            .map(|(id, title, category, summary)| NoteDetailRow {
                id,
                title,
                category,
                summary,
            })
            .collect())
    }

    async fn fetch_notes_brief_batch(&self, note_ids: &[Uuid]) -> Result<Vec<NoteDetailRow>> {
        // Same shape as detail batch for the module handler — reuse the same SQL.
        self.fetch_notes_detail_batch(note_ids).await
    }

    async fn fetch_notes_full_batch(&self, note_ids: &[Uuid]) -> Result<Vec<NoteFullRow>> {
        type NoteFullSqlRow = (
            Uuid,
            Option<String>,
            Option<String>,
            Option<Vec<String>>,
            String,
            String,
            Option<Vec<String>>,
            Option<Vec<String>>,
            Option<String>,
        );
        let rows: Vec<NoteFullSqlRow> = sqlx::query_as(
            "SELECT id, title, summary, facts, content, category, \
                    related_symbols, related_files, branch \
             FROM notes WHERE id = ANY($1)",
        )
        .bind(note_ids)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    title,
                    summary,
                    facts,
                    content,
                    category,
                    related_symbols,
                    related_files,
                    branch,
                )| {
                    NoteFullRow {
                        id,
                        title,
                        summary,
                        facts,
                        content,
                        category,
                        related_symbols,
                        related_files,
                        branch,
                    }
                },
            )
            .collect())
    }

    // ── CRUD methods (A2a) ────────────────────────────────────────────────────

    async fn insert_note(&self, row: NoteInsertRow) -> Result<Uuid> {
        // SQL moved verbatim from akashic-server::mcp::tools::save_note.
        let vec = pgvector::Vector::from(row.embedding);
        let (id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO notes (repo_name, branch, category, title, summary, content, facts, \
                               author, embedding, related_symbols, related_files, tags, saga_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'ai', $8, $9, $10, $11, $12) \
             RETURNING id",
        )
        .bind(&row.repo_name)
        .bind(&row.branch)
        .bind(&row.category)
        .bind(&row.title)
        .bind(&row.summary)
        .bind(&row.content)
        .bind(&row.facts)
        .bind(vec)
        .bind(&row.related_symbols)
        .bind(&row.related_files)
        .bind(&row.tags)
        .bind(row.saga_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    async fn list_notes_paginated(
        &self,
        repo_name: &str,
        branch: Option<&str>,
        category: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<(i64, Vec<NoteListCrudRow>)> {
        // SQL moved verbatim from akashic-server::api::routes::notes::list_notes.
        let (total,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM notes \
             WHERE repo_name = $1 \
               AND ($2::text IS NULL OR branch = $2) \
               AND ($3::text IS NULL OR category = $3)",
        )
        .bind(repo_name)
        .bind(branch)
        .bind(category)
        .fetch_one(&self.pool)
        .await?;

        type ListSqlRow = (
            Uuid,
            Option<String>,
            Option<String>,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<Vec<String>>,
            Option<Uuid>,
            Option<i32>,
        );

        let rows: Vec<ListSqlRow> = sqlx::query_as(
            "SELECT id, title, summary, content, category, \
                    to_char(created_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
                    to_char(updated_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
                    tags, saga_id, access_count \
             FROM notes \
             WHERE repo_name = $1 \
               AND ($2::text IS NULL OR branch = $2) \
               AND ($3::text IS NULL OR category = $3) \
             ORDER BY created_at DESC \
             OFFSET $4 LIMIT $5",
        )
        .bind(repo_name)
        .bind(branch)
        .bind(category)
        .bind(offset)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let items = rows
            .into_iter()
            .map(
                |(
                    id,
                    title,
                    summary,
                    _content,
                    category,
                    created_at,
                    updated_at,
                    tags,
                    saga_id,
                    access_count,
                )| {
                    NoteListCrudRow {
                        id,
                        title,
                        category,
                        summary,
                        tags,
                        saga_id,
                        created_at: created_at.unwrap_or_default(),
                        updated_at,
                        access_count,
                    }
                },
            )
            .collect();

        Ok((total, items))
    }

    async fn fetch_note_by_id(
        &self,
        note_id: Uuid,
        repo_name: &str,
    ) -> Result<Option<NoteCrudRow>> {
        // SQL moved verbatim from akashic-server::api::routes::notes::get_note.
        // repo_name omitted from SELECT (already available as the `repo_name`
        // parameter) to stay within sqlx's 16-column tuple limit.
        type DetailSqlRow = (
            Uuid,                                  // id
            Option<String>,                        // title
            Option<String>,                        // summary
            String,                                // content
            String,                                // category
            Option<String>,                        // branch
            Option<Vec<String>>,                   // tags
            Option<Vec<String>>,                   // facts
            Option<Vec<String>>,                   // related_symbols
            Option<Vec<String>>,                   // related_files
            Option<Uuid>,                          // saga_id
            Option<Uuid>,                          // superseded_by
            Option<chrono::DateTime<chrono::Utc>>, // invalid_at
            Option<i32>,                           // access_count
            Option<String>,                        // created_at (formatted)
            Option<String>,                        // updated_at (formatted)
        );
        let row: Option<DetailSqlRow> = sqlx::query_as(
            "SELECT id, title, summary, content, category, branch, \
                    tags, facts, related_symbols, related_files, \
                    saga_id, superseded_by, invalid_at, access_count, \
                    to_char(created_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
                    to_char(updated_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
             FROM notes WHERE id = $1 AND repo_name = $2",
        )
        .bind(note_id)
        .bind(repo_name)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(
            |(
                id,
                title,
                summary,
                content,
                category,
                branch,
                tags,
                facts,
                related_symbols,
                related_files,
                saga_id,
                supersedes,
                invalid_at,
                access_count,
                created_at,
                updated_at,
            )| {
                NoteCrudRow {
                    id,
                    repo_name: repo_name.to_owned(),
                    branch,
                    category,
                    title,
                    summary,
                    content,
                    facts,
                    related_symbols,
                    related_files,
                    tags,
                    saga_id,
                    supersedes,
                    is_superseded: invalid_at.is_some(),
                    access_count,
                    created_at: created_at.unwrap_or_default(),
                    updated_at,
                }
            },
        ))
    }

    async fn update_note(
        &self,
        note_id: Uuid,
        repo_name: &str,
        patch: NotePatchRow,
    ) -> Result<u64> {
        // SQL moved verbatim from akashic-server::api::routes::notes::update_note
        // (two variants: with embedding and without).
        let rows_affected = match patch.embedding {
            Some(vector) => {
                let vec = pgvector::Vector::from(vector);
                sqlx::query(
                    "UPDATE notes SET title = $1, summary = $2, content = $3, category = $4, \
                     tags = $5, embedding = $6, updated_at = now() \
                     WHERE id = $7 AND repo_name = $8",
                )
                .bind(patch.title)
                .bind(patch.summary)
                .bind(patch.content)
                .bind(patch.category)
                .bind(patch.tags)
                .bind(vec)
                .bind(note_id)
                .bind(repo_name)
                .execute(&self.pool)
                .await?
                .rows_affected()
            }
            None => sqlx::query(
                "UPDATE notes SET title = $1, summary = $2, content = $3, category = $4, \
                     tags = $5, updated_at = now() \
                     WHERE id = $6 AND repo_name = $7",
            )
            .bind(patch.title)
            .bind(patch.summary)
            .bind(patch.content)
            .bind(patch.category)
            .bind(patch.tags)
            .bind(note_id)
            .bind(repo_name)
            .execute(&self.pool)
            .await?
            .rows_affected(),
        };
        Ok(rows_affected)
    }

    async fn delete_note(&self, note_id: Uuid, repo_name: &str) -> Result<u64> {
        // SQL moved verbatim from akashic-server::api::routes::notes::delete_note.
        let rows_affected = sqlx::query("DELETE FROM notes WHERE id = $1 AND repo_name = $2")
            .bind(note_id)
            .bind(repo_name)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(rows_affected)
    }

    async fn note_exists(&self, note_id: Uuid) -> Result<bool> {
        // SQL moved verbatim from akashic-server::mcp::tools::supersede_note.
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM notes WHERE id = $1)")
            .bind(note_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(exists)
    }

    async fn mark_superseded(&self, old_id: Uuid, new_id: Uuid) -> Result<u64> {
        // SQL moved verbatim from akashic-server::mcp::tools::supersede_note.
        let rows_affected =
            sqlx::query("UPDATE notes SET invalid_at = now(), superseded_by = $1 WHERE id = $2")
                .bind(new_id)
                .bind(old_id)
                .execute(&self.pool)
                .await?
                .rows_affected();
        Ok(rows_affected)
    }

    async fn unmark_superseded(&self, old_id: Uuid) -> Result<u64> {
        // Exact inverse of mark_superseded — compensation for a failed Neo4j
        // SUPERSEDES mirror write in CurationService::supersede_note.
        let rows_affected =
            sqlx::query("UPDATE notes SET invalid_at = NULL, superseded_by = NULL WHERE id = $1")
                .bind(old_id)
                .execute(&self.pool)
                .await?
                .rows_affected();
        Ok(rows_affected)
    }
}

// ── PgNoteHealthRepo ──────────────────────────────────────────────────────────

/// PostgreSQL adapter implementing [`NoteHealthRepo`].
#[derive(Clone)]
pub struct PgNoteHealthRepo {
    pool: PgPool,
}

impl PgNoteHealthRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl NoteHealthRepo for PgNoteHealthRepo {
    // ── Write ────────────────────────────────────────────────────────────────

    async fn update_staleness(
        &self,
        note_id: Uuid,
        staleness_score: f64,
        reasons: &[String],
    ) -> Result<()> {
        sqlx::query(
            "UPDATE notes SET staleness_score = $1, staleness_reasons = $2, \
             last_verified_at = now() WHERE id = $3",
        )
        .bind(staleness_score)
        .bind(reasons)
        .bind(note_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn auto_archive_stale(&self, repo_name: &str) -> Result<i64> {
        let archived_count = sqlx::query(
            "UPDATE notes SET archived = true \
             WHERE repo_name = $1 AND (archived IS NULL OR archived = false) \
               AND staleness_score > 0.9 \
               AND (access_count IS NULL OR access_count = 0) \
               AND created_at < now() - interval '90 days'",
        )
        .bind(repo_name)
        .execute(&self.pool)
        .await?
        .rows_affected();

        Ok(archived_count as i64)
    }

    // ── Count reads ──────────────────────────────────────────────────────────

    async fn count_total_notes(&self, repo_name: &str) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM notes WHERE repo_name = $1")
            .bind(repo_name)
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    async fn count_archived_notes(&self, repo_name: &str) -> Result<i64> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM notes WHERE repo_name = $1 AND archived = true")
                .bind(repo_name)
                .fetch_one(&self.pool)
                .await?;
        Ok(count)
    }

    async fn count_healthy_notes(&self, repo_name: &str) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM notes WHERE repo_name = $1 AND (archived IS NULL OR archived = false) AND staleness_score < 0.3",
        )
        .bind(repo_name)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    async fn count_needs_review_notes(&self, repo_name: &str) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM notes WHERE repo_name = $1 AND (archived IS NULL OR archived = false) \
             AND staleness_score >= 0.3 AND staleness_score < 0.7",
        )
        .bind(repo_name)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    async fn count_stale_notes(&self, repo_name: &str) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM notes WHERE repo_name = $1 AND (archived IS NULL OR archived = false) AND staleness_score >= 0.7",
        )
        .bind(repo_name)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    // ── Fetch reads ──────────────────────────────────────────────────────────

    async fn get_stale_notes_above_threshold(
        &self,
        repo_name: &str,
        min_staleness: f64,
    ) -> Result<Vec<StaleNoteDbRow>> {
        let rows: Vec<(
            Uuid,
            Option<String>,
            f64,
            Vec<String>,
            Option<i32>,
            Option<chrono::DateTime<chrono::Utc>>,
        )> = sqlx::query_as(
            "SELECT id, title, staleness_score, coalesce(staleness_reasons, '{}'), \
                        access_count, last_accessed_at \
             FROM notes WHERE repo_name = $1 AND (archived IS NULL OR archived = false) \
               AND staleness_score >= $2 \
             ORDER BY staleness_score DESC",
        )
        .bind(repo_name)
        .bind(min_staleness)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    title,
                    staleness_score,
                    staleness_reasons,
                    access_count,
                    last_accessed_at,
                )| {
                    StaleNoteDbRow {
                        id,
                        title,
                        staleness_score,
                        staleness_reasons,
                        access_count,
                        last_accessed_at,
                    }
                },
            )
            .collect())
    }

    async fn get_notes_by_category(&self, repo_name: &str) -> Result<Vec<(String, i64)>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT category, COUNT(*) AS cnt FROM notes \
             WHERE repo_name = $1 AND invalid_at IS NULL AND archived = false \
             GROUP BY category",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn get_top_notes_by_access(
        &self,
        repo_name: &str,
        limit: i64,
    ) -> Result<Vec<TopNoteDbRow>> {
        // NULLS LAST: Postgres sorts NULLs FIRST under DESC by default, which would
        // rank a never-accessed note (NULL access_count) above the genuinely
        // most-accessed ones. Force NULLs to the bottom so the top-N is correct.
        let rows: Vec<(Option<String>, String, Option<String>, Option<i32>)> = sqlx::query_as(
            "SELECT title, category, summary, access_count FROM notes \
             WHERE repo_name = $1 AND invalid_at IS NULL AND archived = false \
               AND title IS NOT NULL \
             ORDER BY access_count DESC NULLS LAST LIMIT $2",
        )
        .bind(repo_name)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(title, category, summary, access_count)| TopNoteDbRow {
                title,
                category,
                summary,
                access_count,
            })
            .collect())
    }

    async fn count_notes_for_saga(&self, saga_id: Uuid) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notes WHERE saga_id = $1")
            .bind(saga_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    async fn get_saga_notes_timeline(&self, saga_id: Uuid) -> Result<Vec<SagaNoteDbRow>> {
        let rows: Vec<(
            Option<String>,
            String,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
            bool,
        )> = sqlx::query_as(
            "SELECT title, category, summary, \
                    COALESCE(valid_at, created_at) AS ts, \
                    (invalid_at IS NOT NULL) AS is_superseded \
             FROM notes WHERE saga_id = $1 \
             ORDER BY COALESCE(valid_at, created_at) ASC",
        )
        .bind(saga_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(title, category, summary, ts, is_superseded)| SagaNoteDbRow {
                    title,
                    category,
                    summary,
                    ts,
                    is_superseded,
                },
            )
            .collect())
    }

    async fn get_latest_notes(
        &self,
        repo_name: &str,
        branch: Option<&str>,
        limit: i64,
    ) -> Result<Vec<NoteBriefRow>> {
        // SQL moved verbatim from mcp/tools.rs get_project_summary.
        let rows: Vec<(Uuid, String, Option<String>, Option<String>, String)> =
            if let Some(b) = branch {
                sqlx::query_as(
                    "SELECT id, category, branch, \
                            to_char(created_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
                            left(content, 120) \
                     FROM notes WHERE repo_name = $1 AND branch = $2 \
                     ORDER BY created_at DESC LIMIT $3",
                )
                .bind(repo_name)
                .bind(b)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            } else {
                sqlx::query_as(
                    "SELECT id, category, branch, \
                            to_char(created_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
                            left(content, 120) \
                     FROM notes WHERE repo_name = $1 \
                     ORDER BY created_at DESC LIMIT $2",
                )
                .bind(repo_name)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            };

        Ok(rows
            .into_iter()
            .map(|(id, category, branch, created_at, snippet)| NoteBriefRow {
                id,
                category,
                branch,
                created_at,
                snippet,
            })
            .collect())
    }

    async fn increment_access_count(&self, note_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE notes SET access_count = coalesce(access_count, 0) + 1, \
             last_accessed_at = now() WHERE id = $1",
        )
        .bind(note_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_database_url() -> String {
        std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| {
            "postgres://akashic:${PG_PASSWORD}@localhost:5432/akashic".to_string()
        })
    }

    async fn connect() -> PgPool {
        let url = test_database_url();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect to test Postgres");
        crate::init_schema(&pool, 1536, "vector(1536)", "vector_cosine_ops")
            .await
            .expect("init schema");
        pool
    }

    #[tokio::test]
    #[ignore = "requires live Postgres; run with --ignored"]
    async fn compute_embedding_divergence_zero_match_is_none_not_err() {
        let pool = connect().await;
        let repo = "e2e-note-divergence-zeromatch";
        let note_repo = PgNoteRepo::new(pool.clone());

        // Clean, then seed ONE note with an embedding and NO matching chunk.
        sqlx::query("DELETE FROM notes WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();

        let note_id = Uuid::new_v4();
        let emb = pgvector::Vector::from(vec![0.1_f32; 1536]);
        sqlx::query(
            "INSERT INTO notes (id, repo_name, category, title, content, related_symbols, embedding) \
             VALUES ($1,$2,'ARCHITECTURE','t','c', ARRAY['symbol_that_does_not_exist'], $3)",
        )
        .bind(note_id)
        .bind(repo)
        .bind(emb)
        .execute(&pool)
        .await
        .unwrap();

        // Zero chunks named 'symbol_that_does_not_exist' → AVG is NULL.
        let div = note_repo
            .compute_embedding_divergence(
                note_id,
                repo,
                &["symbol_that_does_not_exist".to_string()],
            )
            .await;

        assert!(
            div.is_ok(),
            "zero-match divergence must be Ok, got Err: {div:?}"
        );
        assert_eq!(div.unwrap(), None, "zero-match divergence must be None");

        sqlx::query("DELETE FROM notes WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    #[ignore = "requires live Postgres; run with --ignored"]
    async fn compute_embedding_divergence_nonempty_match_is_some() {
        let pool = connect().await;
        let repo = "e2e-note-divergence-nonempty";
        let note_repo = PgNoteRepo::new(pool.clone());

        sqlx::query("DELETE FROM notes WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();

        let note_id = Uuid::new_v4();
        let emb = pgvector::Vector::from(vec![0.1_f32; 1536]);
        sqlx::query(
            "INSERT INTO notes (id, repo_name, category, title, content, related_symbols, embedding) \
             VALUES ($1,$2,'ARCHITECTURE','t','c', ARRAY['real_fn'], $3)",
        )
        .bind(note_id)
        .bind(repo)
        .bind(emb.clone())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO chunks (id, repo_name, module_path, chunk_type, name, content, embedding) \
             VALUES ($1,$2,'src/lib.rs','function','real_fn','fn real_fn() {}', $3)",
        )
        .bind(Uuid::new_v4())
        .bind(repo)
        .bind(emb)
        .execute(&pool)
        .await
        .unwrap();

        let div = note_repo
            .compute_embedding_divergence(note_id, repo, &["real_fn".to_string()])
            .await
            .expect("non-empty divergence must be Ok");
        assert!(div.is_some(), "non-empty match must yield Some(divergence)");

        sqlx::query("DELETE FROM notes WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .ok();
    }
}
