//! PostgreSQL adapter for `CommunityRepo`.
//!
//! All SQL is moved verbatim from the original call-sites:
//! - `akashic-ingestion::community::mod` (`store_communities`, `clean_communities`)
//! - `akashic-ingestion::community::summarize` (`update_community_summary`,
//!   `resolve_community_id`)

use anyhow::Result;
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use akashic_domain::ports::CommunityRepo;
use akashic_domain::types::CommunitySummaryRow;

/// PostgreSQL adapter implementing [`CommunityRepo`].
#[derive(Clone)]
pub struct PgCommunityRepo {
    pool: PgPool,
}

impl PgCommunityRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CommunityRepo for PgCommunityRepo {
    async fn clean_communities(&self, repo_name: &str) -> Result<()> {
        sqlx::query(
            "DELETE FROM community_members \
             WHERE community_id IN (SELECT id FROM communities WHERE repo_name = $1)",
        )
        .bind(repo_name)
        .execute(&self.pool)
        .await?;

        sqlx::query("DELETE FROM communities WHERE repo_name = $1")
            .bind(repo_name)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    async fn insert_community(
        &self,
        repo_name: &str,
        level: i16,
        member_count: i32,
    ) -> Result<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO communities (repo_name, level, member_count) \
             VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(repo_name)
        .bind(level)
        .bind(member_count)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    async fn insert_community_chunk_member(
        &self,
        community_id: Uuid,
        chunk_id: Uuid,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO community_members (community_id, chunk_id) \
             VALUES ($1, $2)",
        )
        .bind(community_id)
        .bind(chunk_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn insert_community_module_member(
        &self,
        community_id: Uuid,
        module_id: Uuid,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO community_members (community_id, module_id) \
             VALUES ($1, $2)",
        )
        .bind(community_id)
        .bind(module_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_community_summary(
        &self,
        community_id: Uuid,
        name: &str,
        summary: &str,
        embedding: Option<&[f32]>,
    ) -> Result<()> {
        if let Some(emb) = embedding {
            let vec = pgvector::Vector::from(emb.to_vec());
            sqlx::query(
                "UPDATE communities SET name = $1, summary = $2, embedding = $3 WHERE id = $4",
            )
            .bind(name)
            .bind(summary)
            .bind(vec)
            .bind(community_id)
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query("UPDATE communities SET name = $1, summary = $2 WHERE id = $3")
                .bind(name)
                .bind(summary)
                .bind(community_id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    async fn resolve_community_id_by_chunk_member(
        &self,
        repo_name: &str,
        level: i16,
        member_pg_id: Uuid,
    ) -> Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT cm.community_id FROM community_members cm \
             JOIN communities c ON c.id = cm.community_id \
             WHERE c.repo_name = $1 AND c.level = $2 AND cm.chunk_id = $3 \
             LIMIT 1",
        )
        .bind(repo_name)
        .bind(level)
        .bind(member_pg_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id,)| id))
    }

    async fn resolve_community_id_by_module_member(
        &self,
        repo_name: &str,
        level: i16,
        member_pg_id: Uuid,
    ) -> Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT cm.community_id FROM community_members cm \
             JOIN communities c ON c.id = cm.community_id \
             WHERE c.repo_name = $1 AND c.level = $2 AND cm.module_id = $3 \
             LIMIT 1",
        )
        .bind(repo_name)
        .bind(level)
        .bind(member_pg_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id,)| id))
    }

    async fn max_summarized_level(&self, repo_name: &str) -> Result<Option<i16>> {
        let row: Option<(Option<i16>,)> = sqlx::query_as(
            "SELECT MAX(level) FROM communities WHERE repo_name = $1 AND summary IS NOT NULL",
        )
        .bind(repo_name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|(v,)| v))
    }

    async fn select_communities_by_vector(
        &self,
        repo_name: &str,
        level: i16,
        embedding: &[f32],
        limit: i64,
    ) -> Result<Vec<CommunitySummaryRow>> {
        let vec = pgvector::Vector::from(embedding.to_vec());
        let rows: Vec<(Uuid, String, String, i16, i32, f64)> = sqlx::query_as(
            "SELECT id, coalesce(name, 'Unnamed'), coalesce(summary, ''), level, member_count, \
                    (1.0 - (embedding <=> $1::vector))::float8 AS score \
             FROM communities \
             WHERE repo_name = $2 AND level = $3 AND embedding IS NOT NULL \
             ORDER BY embedding <=> $1::vector LIMIT $4",
        )
        .bind(vec)
        .bind(repo_name)
        .bind(level)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, name, summary, level, member_count, score)| CommunitySummaryRow {
                    id,
                    name,
                    summary,
                    level,
                    member_count,
                    score,
                },
            )
            .collect())
    }

    async fn fetch_community_examples(
        &self,
        community_id: Uuid,
        limit: i64,
    ) -> Result<Vec<(String, String, String)>> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT c.name, c.chunk_type, c.module_path \
             FROM community_members cm JOIN chunks c ON cm.chunk_id = c.id \
             WHERE cm.community_id = $1 LIMIT $2",
        )
        .bind(community_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}
