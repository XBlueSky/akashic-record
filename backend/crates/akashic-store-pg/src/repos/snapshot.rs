//! PostgreSQL adapter for `SnapshotPgRepo` (Roadmap E2).
//!
//! Export reads full-fidelity rows (embeddings included) the analysis-shaped
//! ports (`ModuleRow`, `ChunkFullRow`) don't carry. Import uses explicit-id
//! `INSERT`s — NOT `upsert_module`/`store_chunk_row`, which always
//! `gen_random_uuid()` or `ON CONFLICT` — so the original UUIDs survive the
//! round trip and stay joined to the matching Neo4j `pg_id` properties.

use anyhow::{Context, Result};
use async_trait::async_trait;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use akashic_domain::ports::SnapshotPgRepo;
use akashic_domain::types::{
    ChunkSnapshotRow, CommunityMemberSnapshotRow, CommunitySnapshotRow, LargeChunkSnapshotRow,
    ModuleSnapshotRow, PgRepoSnapshot,
};

#[derive(Clone)]
pub struct PgSnapshotRepo {
    pool: PgPool,
}

impl PgSnapshotRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Shared insert body for BOTH import paths (`import_repo_snapshot` and
    /// `import_repo_snapshot_replacing`). Runs every explicit-id `INSERT` on
    /// the supplied connection/transaction handle in the fixed order the
    /// schema's FKs require; it does NOT begin or commit — the caller owns the
    /// transaction boundary. Extracting it guarantees the two import paths stay
    /// byte-identical on the insert side (a plain import and a wipe+import must
    /// write the same rows the same way).
    async fn insert_all(
        conn: &mut PgConnection,
        repo_name: &str,
        snapshot: &PgRepoSnapshot,
    ) -> Result<()> {
        for m in &snapshot.modules {
            let emb = m
                .embedding
                .as_ref()
                .map(|e| pgvector::Vector::from(e.clone()));
            sqlx::query(
                "INSERT INTO modules \
                 (id, repo_name, path, language, summary, exports_count, file_count, \
                  is_virtual, embedding, git_ref, ingested_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            )
            .bind(m.id)
            .bind(repo_name)
            .bind(&m.path)
            .bind(&m.language)
            .bind(&m.summary)
            .bind(m.exports_count)
            .bind(m.file_count)
            .bind(m.is_virtual)
            .bind(emb)
            .bind(&m.git_ref)
            .bind(m.ingested_at)
            .execute(&mut *conn)
            .await
            .context("Failed to import a module row")?;
        }

        for c in &snapshot.chunks {
            let emb = pgvector::Vector::from(c.embedding.clone());
            let sig_emb = c.signature_embedding.clone().map(pgvector::Vector::from);
            sqlx::query(
                "INSERT INTO chunks \
                 (id, repo_name, module_path, chunk_type, name, signature, content, language, \
                  embedding, signature_embedding, git_ref, fqn, parent_fqn, start_line, end_line, \
                  visibility, is_async, is_static, is_exported, doc, ingested_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21)",
            )
            .bind(c.id)
            .bind(repo_name)
            .bind(&c.module_path)
            .bind(&c.chunk_type)
            .bind(&c.name)
            .bind(&c.signature)
            .bind(&c.content)
            .bind(&c.language)
            .bind(emb)
            .bind(sig_emb)
            .bind(&c.git_ref)
            .bind(&c.fqn)
            .bind(&c.parent_fqn)
            .bind(c.start_line)
            .bind(c.end_line)
            .bind(&c.visibility)
            .bind(c.is_async)
            .bind(c.is_static)
            .bind(c.is_exported)
            .bind(&c.doc)
            .bind(c.ingested_at)
            .execute(&mut *conn)
            .await
            .context("Failed to import a chunk row")?;
        }

        for lc in &snapshot.large_chunks {
            let emb = pgvector::Vector::from(lc.embedding.clone());
            sqlx::query(
                "INSERT INTO large_chunks (id, repo_name, module_path, chunk_ids, content, embedding, git_ref, ingested_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
            )
            .bind(lc.id)
            .bind(repo_name)
            .bind(&lc.module_path)
            .bind(&lc.chunk_ids)
            .bind(&lc.content)
            .bind(emb)
            .bind(&lc.git_ref)
            .bind(lc.ingested_at)
            .execute(&mut *conn)
            .await
            .context("Failed to import a large_chunk row")?;
        }

        // Two-pass: insert every community with parent_id NULL first (avoids
        // assuming any parent/child insertion order), then backfill parent_id.
        for comm in &snapshot.communities {
            let emb = comm
                .embedding
                .as_ref()
                .map(|e| pgvector::Vector::from(e.clone()));
            sqlx::query(
                "INSERT INTO communities (id, repo_name, level, name, summary, member_count, embedding, created_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
            )
            .bind(comm.id)
            .bind(repo_name)
            .bind(comm.level)
            .bind(&comm.name)
            .bind(&comm.summary)
            .bind(comm.member_count)
            .bind(emb)
            .bind(comm.created_at)
            .execute(&mut *conn)
            .await
            .context("Failed to import a community row")?;
        }
        for comm in &snapshot.communities {
            if let Some(parent_id) = comm.parent_id {
                sqlx::query("UPDATE communities SET parent_id = $1 WHERE id = $2")
                    .bind(parent_id)
                    .bind(comm.id)
                    .execute(&mut *conn)
                    .await
                    .context("Failed to backfill a community's parent_id")?;
            }
        }

        for cm in &snapshot.community_members {
            sqlx::query(
                "INSERT INTO community_members (community_id, chunk_id, module_id) VALUES ($1,$2,$3)",
            )
            .bind(cm.community_id)
            .bind(cm.chunk_id)
            .bind(cm.module_id)
            .execute(&mut *conn)
            .await
            .context("Failed to import a community_members row")?;
        }

        Ok(())
    }
}

#[async_trait]
impl SnapshotPgRepo for PgSnapshotRepo {
    async fn export_repo_snapshot(&self, repo_name: &str) -> Result<PgRepoSnapshot> {
        let module_rows: Vec<(
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Option<i32>,
            Option<i32>,
            bool,
            Option<pgvector::Vector>,
            Option<String>,
            Option<chrono::DateTime<chrono::Utc>>,
        )> = sqlx::query_as(
            "SELECT id, path, language, summary, exports_count, file_count, is_virtual, \
                    embedding, git_ref, ingested_at \
             FROM modules WHERE repo_name = $1",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await
        .context("Failed to export modules")?;

        let modules = module_rows
            .into_iter()
            .map(
                |(
                    id,
                    path,
                    language,
                    summary,
                    exports_count,
                    file_count,
                    is_virtual,
                    embedding,
                    git_ref,
                    ingested_at,
                )| {
                    ModuleSnapshotRow {
                        id,
                        path,
                        language,
                        summary,
                        exports_count,
                        file_count,
                        is_virtual,
                        embedding: embedding.map(|e| e.to_vec()),
                        git_ref,
                        ingested_at,
                    }
                },
            )
            .collect();

        // A plain tuple can't carry all 20 `chunks` columns: sqlx's `FromRow`
        // is only implemented for tuples up to 16 elements
        // (sqlx-core's `impl_from_row_for_tuple!` macro invocations top out
        // at `T1..T16`). A `#[derive(sqlx::FromRow)]` struct has no such
        // arity limit, so it replaces the tuple for this one query.
        #[derive(sqlx::FromRow)]
        struct ChunkExportRow {
            id: Uuid,
            module_path: String,
            chunk_type: String,
            name: String,
            signature: Option<String>,
            content: String,
            language: Option<String>,
            embedding: pgvector::Vector,
            signature_embedding: Option<pgvector::Vector>,
            git_ref: Option<String>,
            fqn: Option<String>,
            parent_fqn: Option<String>,
            start_line: Option<i32>,
            end_line: Option<i32>,
            visibility: Option<String>,
            is_async: bool,
            is_static: bool,
            is_exported: bool,
            doc: Option<String>,
            ingested_at: Option<chrono::DateTime<chrono::Utc>>,
        }

        let chunk_rows: Vec<ChunkExportRow> = sqlx::query_as(
            "SELECT id, module_path, chunk_type, name, signature, content, language, \
                    embedding, signature_embedding, git_ref, fqn, parent_fqn, \
                    start_line, end_line, visibility, is_async, is_static, is_exported, doc, \
                    ingested_at \
             FROM chunks WHERE repo_name = $1",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await
        .context("Failed to export chunks")?;

        let chunks = chunk_rows
            .into_iter()
            .map(|r| ChunkSnapshotRow {
                id: r.id,
                module_path: r.module_path,
                chunk_type: r.chunk_type,
                name: r.name,
                signature: r.signature,
                content: r.content,
                language: r.language,
                embedding: r.embedding.to_vec(),
                signature_embedding: r.signature_embedding.map(|e| e.to_vec()),
                git_ref: r.git_ref,
                fqn: r.fqn,
                parent_fqn: r.parent_fqn,
                start_line: r.start_line,
                end_line: r.end_line,
                visibility: r.visibility,
                is_async: r.is_async,
                is_static: r.is_static,
                is_exported: r.is_exported,
                doc: r.doc,
                ingested_at: r.ingested_at,
            })
            .collect();

        let large_chunk_rows: Vec<(
            Uuid,
            String,
            Vec<Uuid>,
            String,
            pgvector::Vector,
            Option<String>,
            Option<chrono::DateTime<chrono::Utc>>,
        )> = sqlx::query_as(
            "SELECT id, module_path, chunk_ids, content, embedding, git_ref, ingested_at \
             FROM large_chunks WHERE repo_name = $1",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await
        .context("Failed to export large_chunks")?;

        let large_chunks = large_chunk_rows
            .into_iter()
            .map(
                |(id, module_path, chunk_ids, content, embedding, git_ref, ingested_at)| {
                    LargeChunkSnapshotRow {
                        id,
                        module_path,
                        chunk_ids,
                        content,
                        embedding: embedding.to_vec(),
                        git_ref,
                        ingested_at,
                    }
                },
            )
            .collect();

        let community_rows: Vec<(
            Uuid,
            i16,
            Option<String>,
            Option<String>,
            Option<i32>,
            Option<pgvector::Vector>,
            Option<Uuid>,
            Option<chrono::DateTime<chrono::Utc>>,
        )> = sqlx::query_as(
            "SELECT id, level, name, summary, member_count, embedding, parent_id, created_at \
             FROM communities WHERE repo_name = $1",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await
        .context("Failed to export communities")?;

        let communities = community_rows
            .into_iter()
            .map(
                |(id, level, name, summary, member_count, embedding, parent_id, created_at)| {
                    CommunitySnapshotRow {
                        id,
                        level,
                        name,
                        summary,
                        member_count,
                        embedding: embedding.map(|e| e.to_vec()),
                        parent_id,
                        created_at,
                    }
                },
            )
            .collect();

        let member_rows: Vec<(Uuid, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
            "SELECT cm.community_id, cm.chunk_id, cm.module_id \
             FROM community_members cm \
             JOIN communities c ON cm.community_id = c.id \
             WHERE c.repo_name = $1",
        )
        .bind(repo_name)
        .fetch_all(&self.pool)
        .await
        .context("Failed to export community_members")?;

        let community_members = member_rows
            .into_iter()
            .map(
                |(community_id, chunk_id, module_id)| CommunityMemberSnapshotRow {
                    community_id,
                    chunk_id,
                    module_id,
                },
            )
            .collect();

        Ok(PgRepoSnapshot {
            modules,
            chunks,
            large_chunks,
            communities,
            community_members,
        })
    }

    async fn import_repo_snapshot(&self, repo_name: &str, snapshot: &PgRepoSnapshot) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .context("Failed to start import transaction")?;

        Self::insert_all(&mut tx, repo_name, snapshot).await?;

        tx.commit()
            .await
            .context("Failed to commit import transaction")?;
        Ok(())
    }

    async fn import_repo_snapshot_replacing(
        &self,
        repo_name: &str,
        snapshot: &PgRepoSnapshot,
    ) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .context("Failed to start replacing-import transaction")?;

        // FK-safe wipe of every existing row for `repo_name`, run on the SAME
        // transaction as the inserts below, so a mid-insert failure rolls the
        // wipe back too. ORDER is deliberate: `community_members`/communities
        // are deleted before chunks/large_chunks/modules, because
        // `community_members.chunk_id`/`module_id` FK-reference
        // `chunks(id)`/`modules(id)` with NO `ON DELETE CASCADE` — deleting
        // chunks/modules first would violate that FK while community rows
        // still reference them. The `community_members` delete shape is
        // copied verbatim from `clean_communities` (delete members whose
        // community belongs to the repo; communities are repo-scoped, so this
        // covers every member row referencing this repo's chunks/modules).
        sqlx::query(
            "DELETE FROM community_members \
             WHERE community_id IN (SELECT id FROM communities WHERE repo_name = $1)",
        )
        .bind(repo_name)
        .execute(&mut *tx)
        .await
        .context("Failed to delete old community_members before replacing import")?;

        sqlx::query("DELETE FROM communities WHERE repo_name = $1")
            .bind(repo_name)
            .execute(&mut *tx)
            .await
            .context("Failed to delete old communities before replacing import")?;

        sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
            .bind(repo_name)
            .execute(&mut *tx)
            .await
            .context("Failed to delete old chunks before replacing import")?;

        sqlx::query("DELETE FROM large_chunks WHERE repo_name = $1")
            .bind(repo_name)
            .execute(&mut *tx)
            .await
            .context("Failed to delete old large_chunks before replacing import")?;

        sqlx::query("DELETE FROM modules WHERE repo_name = $1")
            .bind(repo_name)
            .execute(&mut *tx)
            .await
            .context("Failed to delete old modules before replacing import")?;

        Self::insert_all(&mut tx, repo_name, snapshot).await?;

        tx.commit()
            .await
            .context("Failed to commit replacing-import transaction")?;
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
    async fn export_then_import_round_trips_module_and_chunk() {
        let pool = connect().await;
        let repo = "e2e-snapshot-pg-task2";

        sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM modules WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();

        let module_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO modules (id, repo_name, path, language, summary, exports_count, file_count, is_virtual) \
             VALUES ($1,$2,'src/lib.rs','rust','a module',1,1,false)",
        )
        .bind(module_id)
        .bind(repo)
        .execute(&pool)
        .await
        .unwrap();

        let chunk_id = Uuid::new_v4();
        let emb = pgvector::Vector::from(vec![0.1_f32; 1536]);
        sqlx::query(
            "INSERT INTO chunks (id, repo_name, module_path, chunk_type, name, content, embedding) \
             VALUES ($1,$2,'src/lib.rs','function','do_thing','fn do_thing() {}',$3)",
        )
        .bind(chunk_id)
        .bind(repo)
        .bind(emb)
        .execute(&pool)
        .await
        .unwrap();

        let repo_snap = PgSnapshotRepo::new(pool.clone());
        let snapshot = repo_snap.export_repo_snapshot(repo).await.unwrap();
        assert_eq!(snapshot.modules.len(), 1);
        assert_eq!(snapshot.chunks.len(), 1);
        assert_eq!(snapshot.chunks[0].id, chunk_id);
        assert_eq!(snapshot.chunks[0].embedding.len(), 1536);

        // Clean, then re-import — proves import writes back identical rows
        // under the SAME original UUIDs.
        sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM modules WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();

        repo_snap
            .import_repo_snapshot(repo, &snapshot)
            .await
            .unwrap();

        let (restored_module_id,): (Uuid,) =
            sqlx::query_as("SELECT id FROM modules WHERE repo_name = $1")
                .bind(repo)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(restored_module_id, module_id);

        let (restored_chunk_id,): (Uuid,) =
            sqlx::query_as("SELECT id FROM chunks WHERE repo_name = $1")
                .bind(repo)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(restored_chunk_id, chunk_id);

        // Cleanup.
        sqlx::query("DELETE FROM chunks WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM modules WHERE repo_name = $1")
            .bind(repo)
            .execute(&pool)
            .await
            .unwrap();
    }
}
