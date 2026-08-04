//! Port traits for repo-scoped ingestion snapshot export/import (Roadmap E2).
//!
//! `SnapshotPgRepo` covers both directions because import must use explicit-id
//! `INSERT`s the existing write methods (`upsert_module`, `store_chunk_row`, …)
//! don't support (they always `gen_random_uuid()` or `ON CONFLICT` on natural
//! keys). `SnapshotGraphRepo` is export-only: Neo4j import reuses the exact
//! write ports the ingestion pipeline itself already calls
//! (`ChunkGraphRepo`, `ModuleGraphRepo`, `IngestEdgeRepo`, `FlowGraphRepo`,
//! `CommunityGraphRepo`), since those already MERGE/CREATE by explicit
//! `pg_id`/`Uuid` — no new Neo4j write surface is needed.

use async_trait::async_trait;

use crate::types::{GraphRepoSnapshot, PgRepoSnapshot};

#[async_trait]
pub trait SnapshotPgRepo: Send + Sync {
    /// Read every `modules`/`chunks`/`large_chunks`/`communities`/
    /// `community_members` row for `repo_name`. Returns an empty snapshot
    /// (all vecs empty) if the repo has never been ingested — this is not an
    /// error.
    async fn export_repo_snapshot(&self, repo_name: &str) -> anyhow::Result<PgRepoSnapshot>;

    /// Write every row in `snapshot` under `repo_name`, preserving each row's
    /// original UUID via explicit-id `INSERT`s, inside a single transaction.
    /// Callers MUST have already verified `repo_name` has no existing data in
    /// this database — this method does not check and does not clean.
    async fn import_repo_snapshot(
        &self,
        repo_name: &str,
        snapshot: &PgRepoSnapshot,
    ) -> anyhow::Result<()>;

    /// Like [`import_repo_snapshot`], but FIRST deletes every existing row for
    /// `repo_name` (in FK-safe order) and THEN inserts `snapshot`, all inside
    /// ONE transaction. On any error the transaction rolls back — so the wipe
    /// is undone too and the repo's prior data is left fully intact.
    ///
    /// This is the production commit path (Roadmap F): making the wipe and the
    /// insert atomic converts a mid-commit crash from "old graph already wiped,
    /// new graph not yet landed → both empty" into "prior graph fully intact
    /// (rollback)". Unlike [`import_repo_snapshot`] — which keeps its no-wipe
    /// contract for E2's CLI `import` (a fresh target) and the `run_sync` path —
    /// the caller of this method MUST NOT pre-wipe: this method owns the delete
    /// so it can share the transaction with the insert.
    async fn import_repo_snapshot_replacing(
        &self,
        repo_name: &str,
        snapshot: &PgRepoSnapshot,
    ) -> anyhow::Result<()>;
}

#[async_trait]
pub trait SnapshotGraphRepo: Send + Sync {
    /// Read every `:Module`/`:Chunk` node and every edge/`:Flow`/`:Community`
    /// node the ingestion pipeline wrote for `repo_name`. Returns an empty
    /// snapshot if the repo has no graph data — this is not an error.
    async fn export_repo_snapshot(&self, repo_name: &str) -> anyhow::Result<GraphRepoSnapshot>;
}
