pub mod repos;

pub use repos::{
    PgAccountAuditRepo, PgAdminGrantRepo, PgAuditRepo, PgChunkRepo, PgCommunityRepo,
    PgDeviceFlowRepo, PgDocClusterRepo, PgDocumentRepo, PgIngestionJobRepo, PgModuleRepo,
    PgNoteHealthRepo, PgNoteRepo, PgSagaExecutorRepo, PgSagaRepo, PgSourceRepo, PgSymbolRepo,
};

mod migrate;
mod schema;

pub use migrate::{migrate_vector_precision, vector_precision_migration_needed};
pub use schema::{init_auth_schema, init_corpus_schema, init_schema};

use anyhow::{Context, Result};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tracing::info;

/// Create a PostgreSQL connection pool from the DATABASE_URL.
pub async fn connect(database_url: &str) -> Result<PgPool> {
    let max_conn: u32 = std::env::var("PG_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let pool = PgPoolOptions::new()
        .max_connections(max_conn)
        .connect(database_url)
        .await
        .context("Failed to connect to PostgreSQL")?;
    info!("PostgreSQL connection pool established");
    Ok(pool)
}
