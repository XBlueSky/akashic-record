use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::{info, warn};

/// Check whether existing vector columns match `expected_dims`. If they
/// differ, drop HNSW indexes, alter the columns, and truncate stale rows
/// whose embeddings are now the wrong width.
pub(crate) async fn migrate_vector_dims(pool: &PgPool, expected_dims: usize) -> Result<()> {
    // Query the actual dimension of the chunks.embedding column via pg_attribute.
    // atttypmod for vector stores the dimension; -1 means column doesn't exist yet.
    let row: (i32,) = sqlx::query_as(
        r"
        SELECT COALESCE(
            (SELECT atttypmod FROM pg_attribute
             WHERE attrelid = 'chunks'::regclass
               AND attname  = 'embedding'
               AND NOT attisdropped),
            -1
        )
        ",
    )
    .fetch_one(pool)
    .await
    .context("Failed to check vector column dimensions")?;

    let current_dims = row.0;
    let target = expected_dims as i32;

    if current_dims == -1 || current_dims == target {
        // Column doesn't exist yet (fresh DB) or already correct — nothing to do.
        return Ok(());
    }

    warn!(
        current_dims,
        target, "Vector dimensions changed — migrating columns and clearing stale embeddings"
    );

    // Drop HNSW indexes that reference the old vector type
    sqlx::raw_sql(
        r"
        DROP INDEX IF EXISTS idx_chunks_embedding;
        DROP INDEX IF EXISTS idx_notes_embedding;
        DROP INDEX IF EXISTS idx_modules_embedding;
        DROP INDEX IF EXISTS idx_sections_embedding;
        ",
    )
    .execute(pool)
    .await
    .context("Failed to drop old vector indexes")?;

    // Count affected rows before destructive migration
    let chunk_count: (i64,) = sqlx::query_as("SELECT count(*) FROM chunks")
        .fetch_one(pool)
        .await
        .unwrap_or((0,));
    let note_count: (i64,) = sqlx::query_as("SELECT count(*) FROM notes")
        .fetch_one(pool)
        .await
        .unwrap_or((0,));
    warn!(
        chunks = chunk_count.0,
        notes = note_count.0,
        "Vector dimension migration will DELETE all embedding data — re-ingestion required"
    );

    // Delete stale rows first — ALTER TYPE can't cast between different vector widths.
    // Then alter columns to the new dimension.
    let alter = format!(
        r"
        DELETE FROM sections;
        DELETE FROM chunks;
        DELETE FROM notes;
        DELETE FROM modules;

        ALTER TABLE chunks   ALTER COLUMN embedding TYPE vector({target});
        ALTER TABLE notes     ALTER COLUMN embedding TYPE vector({target});
        ALTER TABLE modules   ALTER COLUMN embedding TYPE vector({target});
        ALTER TABLE sections  ALTER COLUMN embedding TYPE vector({target});
        ",
    );

    sqlx::raw_sql(&alter)
        .execute(pool)
        .await
        .context("Failed to migrate vector columns")?;

    info!(
        target,
        "Vector column migration complete — old data cleared"
    );
    Ok(())
}

/// Detect whether the embedding column UDT (vector vs halfvec) differs from
/// C3: read-only sibling of `migrate_vector_precision`. Returns `true`
/// if a reshape would be needed (i.e. the existing `chunks.embedding`
/// column type does not match `expected_vec_type`). Does not mutate.
pub async fn vector_precision_migration_needed(
    pool: &PgPool,
    expected_vec_type: &str,
) -> Result<bool> {
    let observed: Option<String> = sqlx::query_scalar(
        "SELECT format_type(a.atttypid, a.atttypmod) \
         FROM pg_attribute a \
         JOIN pg_class c ON a.attrelid = c.oid \
         WHERE c.relname = 'chunks' AND a.attname = 'embedding' AND NOT a.attisdropped",
    )
    .fetch_optional(pool)
    .await?;
    match observed {
        Some(t) => Ok(t.trim() != expected_vec_type.trim()),
        // Column missing entirely is a different problem — surface as
        // "needs migration" so the verify path bails loudly.
        None => Ok(true),
    }
}

/// Map a `(table, embedding-column)` pair to the HNSW index name that
/// `init_schema` actually created. Pure logic — extracted so the
/// `chunks.signature_embedding` special-case can be unit-tested without a DB.
///
/// `init_schema` names the chunks signature index `idx_chunks_sig_embedding`
/// (not the templated `idx_chunks_signature_embedding`); every other column
/// follows the `idx_{table}_{col}` convention.
fn embedding_index_name(table: &str, col: &str) -> String {
    if table == "chunks" && col == "signature_embedding" {
        "idx_chunks_sig_embedding".to_string()
    } else {
        format!("idx_{table}_{col}")
    }
}

/// what the current configuration expects. If so, drop HNSW indexes, alter
/// columns to the new type, and recreate the indexes with the correct
/// operator class.
///
/// Returns `true` if a migration was performed.
pub async fn migrate_vector_precision(
    pool: &PgPool,
    vec_type: &str,
    cos_ops: &str,
) -> Result<bool> {
    // 1. Check current column UDT name from information_schema
    let current_udt: Option<String> = sqlx::query_scalar(
        "SELECT udt_name::text FROM information_schema.columns \
         WHERE table_name = 'chunks' AND column_name = 'embedding'",
    )
    .fetch_optional(pool)
    .await?;

    let expected_udt = if vec_type.starts_with("halfvec") {
        "halfvec"
    } else {
        "vector"
    };

    let needs_migration = match &current_udt {
        Some(t) => t.as_str() != expected_udt,
        None => false, // table/column doesn't exist yet — init_schema will handle it
    };

    if !needs_migration {
        return Ok(false);
    }

    warn!(from = ?current_udt, to = expected_udt, "Migrating embedding precision");

    let tables_columns = [
        ("chunks", "embedding"),
        ("chunks", "signature_embedding"),
        ("notes", "embedding"),
        ("modules", "embedding"),
        ("sections", "embedding"),
        ("communities", "embedding"),
        ("doc_clusters", "embedding"),
        ("large_chunks", "embedding"),
    ];

    for (table, col) in &tables_columns {
        // init_schema names the chunks signature index `idx_chunks_sig_embedding`
        // (not `idx_chunks_signature_embedding`), so the templated name was a
        // silent no-op DROP that left the stale vector-opclass index on the
        // column being altered to halfvec. Map it to the real index name.
        let index_name = embedding_index_name(table, col);
        // Drop HNSW index
        sqlx::query(&format!("DROP INDEX IF EXISTS {index_name}"))
            .execute(pool)
            .await?;

        // ALTER column type
        sqlx::query(&format!(
            "ALTER TABLE {table} ALTER COLUMN {col} TYPE {vec_type} USING {col}::{vec_type}"
        ))
        .execute(pool)
        .await?;

        // Recreate HNSW index with correct operator class
        let where_clause = if (*table == "modules"
            || *table == "sections"
            || *table == "communities"
            || *table == "doc_clusters")
            || (*table == "chunks" && *col == "signature_embedding")
        {
            format!(" WHERE {col} IS NOT NULL")
        } else {
            String::new()
        };

        sqlx::query(&format!(
            "CREATE INDEX idx_{table}_{col} ON {table} USING hnsw ({col} {cos_ops}){where_clause}"
        ))
        .execute(pool)
        .await?;
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_signature_embedding_maps_to_sig_index() {
        // init_schema creates this index as `idx_chunks_sig_embedding`, not the
        // templated `idx_chunks_signature_embedding`; the special-case must
        // return the real name so the DROP in migrate_vector_precision hits it.
        assert_eq!(
            embedding_index_name("chunks", "signature_embedding"),
            "idx_chunks_sig_embedding"
        );
    }

    #[test]
    fn other_embedding_columns_use_templated_name() {
        // Every non-special column follows the idx_{table}_{col} convention.
        assert_eq!(
            embedding_index_name("chunks", "embedding"),
            "idx_chunks_embedding"
        );
        assert_eq!(
            embedding_index_name("notes", "embedding"),
            "idx_notes_embedding"
        );
        assert_eq!(
            embedding_index_name("modules", "embedding"),
            "idx_modules_embedding"
        );
        assert_eq!(
            embedding_index_name("sections", "embedding"),
            "idx_sections_embedding"
        );
        // signature_embedding on a non-chunks table is NOT special-cased.
        assert_eq!(
            embedding_index_name("foo", "signature_embedding"),
            "idx_foo_signature_embedding"
        );
    }
}
