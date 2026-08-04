use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::info;

use crate::migrate::migrate_vector_dims;

/// Initialise the PostgreSQL schema: pgvector extension, tables, indexes.
///
/// `vector_dims` controls the width of all embedding columns.
/// `vec_type` is the full SQL type, e.g. `"vector(384)"` or `"halfvec(384)"`.
/// `cos_ops` is the HNSW operator class, e.g. `"vector_cosine_ops"` or `"halfvec_cosine_ops"`.
pub async fn init_schema(
    pool: &PgPool,
    vector_dims: usize,
    vec_type: &str,
    cos_ops: &str,
) -> Result<()> {
    info!(
        vector_dims,
        vec_type, cos_ops, "Initialising PostgreSQL schema …"
    );

    let d = vector_dims; // kept for migrate_vector_dims which still needs the raw number

    // ── Idempotent migration: contexts → notes ──────────────────────
    sqlx::raw_sql(
        r"
        DO $$ BEGIN
            IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'contexts')
               AND NOT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'notes')
            THEN
                ALTER TABLE contexts RENAME TO notes;
                ALTER INDEX IF EXISTS idx_contexts_repo RENAME TO idx_notes_repo;
                ALTER INDEX IF EXISTS idx_contexts_repo_branch RENAME TO idx_notes_repo_branch;
                -- Rename FROM the old contexts-era embedding index, not idx_notes_embedding→itself.
                -- The previous `idx_notes_embedding RENAME TO idx_notes_embedding` was a self-rename
                -- no-op (copy-paste typo) that left the legacy idx_contexts_embedding orphaned.
                ALTER INDEX IF EXISTS idx_contexts_embedding RENAME TO idx_notes_embedding;
            END IF;
        END $$;
        ",
    )
    .execute(pool)
    .await
    .context("Failed to migrate contexts → notes")?;

    let create_tables = format!(
        r"
        CREATE EXTENSION IF NOT EXISTS vector;

        -- ═══ The Map (auto-generated code structure) ═══

        CREATE TABLE IF NOT EXISTS chunks (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_name TEXT NOT NULL,
            module_path TEXT NOT NULL,
            chunk_type TEXT NOT NULL,
            name TEXT NOT NULL,
            signature TEXT,
            content TEXT NOT NULL,
            language TEXT,
            embedding {vec_type} NOT NULL,
            signature_embedding {vec_type},
            git_ref TEXT,
            fqn TEXT,
            parent_fqn TEXT,
            start_line INT,
            end_line INT,
            visibility TEXT,
            is_async BOOLEAN DEFAULT FALSE,
            is_static BOOLEAN DEFAULT FALSE,
            is_exported BOOLEAN DEFAULT FALSE,
            doc TEXT,
            ingested_at TIMESTAMPTZ DEFAULT now()
        );

        CREATE INDEX IF NOT EXISTS idx_chunks_repo ON chunks (repo_name);
        CREATE INDEX IF NOT EXISTS idx_chunks_repo_type ON chunks (repo_name, chunk_type);
        CREATE INDEX IF NOT EXISTS idx_chunks_name ON chunks (name);
        CREATE INDEX IF NOT EXISTS idx_chunks_repo_module ON chunks (repo_name, module_path);

        -- ═══ Large Chunks (module-level summarisation windows) ═══

        CREATE TABLE IF NOT EXISTS large_chunks (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_name TEXT NOT NULL,
            module_path TEXT NOT NULL,
            chunk_ids UUID[] NOT NULL,
            content TEXT NOT NULL,
            embedding {vec_type} NOT NULL,
            git_ref TEXT,
            ingested_at TIMESTAMPTZ DEFAULT now()
        );

        -- ═══ The Notes (human-written knowledge) ═══

        CREATE TABLE IF NOT EXISTS notes (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_name TEXT NOT NULL,
            branch TEXT,
            category TEXT NOT NULL,
            title TEXT,
            summary TEXT,
            content TEXT NOT NULL,
            facts TEXT[],
            author TEXT,
            embedding {vec_type} NOT NULL,
            related_symbols TEXT[],
            related_files TEXT[],
            tags TEXT[],
            created_at TIMESTAMPTZ DEFAULT now(),
            updated_at TIMESTAMPTZ DEFAULT now()
        );

        CREATE INDEX IF NOT EXISTS idx_notes_repo ON notes (repo_name);
        CREATE INDEX IF NOT EXISTS idx_notes_repo_branch ON notes (repo_name, branch);

        -- ═══ Modules (directory-level summaries) ═══

        CREATE TABLE IF NOT EXISTS modules (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_name TEXT NOT NULL,
            path TEXT NOT NULL,
            language TEXT,
            summary TEXT,
            exports_count INTEGER,
            file_count INTEGER,
            is_virtual BOOLEAN NOT NULL DEFAULT false,
            embedding {vec_type},
            git_ref TEXT,
            ingested_at TIMESTAMPTZ DEFAULT now(),
            UNIQUE(repo_name, path)
        );

        -- ═══ Ingestion Jobs ═══

        CREATE TABLE IF NOT EXISTS ingestion_jobs (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_name TEXT NOT NULL,
            git_ref TEXT,
            status TEXT NOT NULL DEFAULT 'pending',
            total_files INTEGER DEFAULT 0,
            processed_files INTEGER DEFAULT 0,
            total_chunks INTEGER DEFAULT 0,
            error_message TEXT,
            started_at TIMESTAMPTZ DEFAULT now(),
            completed_at TIMESTAMPTZ,
            checkpoint_data JSONB,
            resumed_from UUID
        );

        -- ═══ Sources (persisted source config for re-ingest) ═══

        CREATE TABLE IF NOT EXISTS sources (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_name TEXT NOT NULL UNIQUE,
            source_type TEXT NOT NULL,
            seed_url TEXT,
            crawl_depth SMALLINT,
            url_pattern TEXT,
            git_url TEXT,
            created_at TIMESTAMPTZ DEFAULT now(),
            updated_at TIMESTAMPTZ DEFAULT now()
        );

        -- ═══ Doc Space (crawled documentation) ═══

        CREATE TABLE IF NOT EXISTS documents (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_name TEXT NOT NULL,
            source_url TEXT,
            title TEXT NOT NULL,
            doc_type TEXT NOT NULL,
            crawled_at TIMESTAMPTZ DEFAULT NOW()
        );

        CREATE INDEX IF NOT EXISTS idx_documents_repo ON documents(repo_name);

        CREATE TABLE IF NOT EXISTS sections (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            doc_id UUID NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            parent_id UUID REFERENCES sections(id),
            heading TEXT NOT NULL,
            content TEXT NOT NULL,
            depth SMALLINT NOT NULL,
            position SMALLINT NOT NULL,
            tags TEXT[] DEFAULT '{{}}',
            embedding {vec_type}
        );

        CREATE INDEX IF NOT EXISTS idx_sections_doc ON sections(doc_id);
        CREATE INDEX IF NOT EXISTS idx_sections_parent ON sections(parent_id);

        -- ═══ Doc Clustering (topic clusters for website repos) ═══

        CREATE TABLE IF NOT EXISTS doc_clusters (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_name TEXT NOT NULL,
            name TEXT NOT NULL,
            section_count INTEGER DEFAULT 0,
            embedding {vec_type},
            created_at TIMESTAMPTZ DEFAULT now(),
            UNIQUE(repo_name, name)
        );

        CREATE INDEX IF NOT EXISTS idx_doc_clusters_repo ON doc_clusters(repo_name);
        ",
    );

    sqlx::raw_sql(&create_tables)
        .execute(pool)
        .await
        .context("Failed to create base tables")?;

    // Migrate existing vector columns if dimensions changed.
    // This drops old HNSW indexes (they reference the old type), alters the
    // column, and truncates stale embedding data that no longer matches.
    migrate_vector_dims(pool, d).await?;

    // ── Column migrations for existing databases ────────────────
    // signature_embedding on chunks (multipass embedding)
    sqlx::query(&format!(
        "ALTER TABLE chunks ADD COLUMN IF NOT EXISTS signature_embedding {vec_type}"
    ))
    .execute(pool)
    .await
    .context("Failed to add signature_embedding column")?;

    // checkpoint columns on ingestion_jobs
    sqlx::raw_sql(
        "ALTER TABLE ingestion_jobs ADD COLUMN IF NOT EXISTS checkpoint_data JSONB; \
         ALTER TABLE ingestion_jobs ADD COLUMN IF NOT EXISTS resumed_from UUID;",
    )
    .execute(pool)
    .await
    .context("Failed to add checkpoint columns")?;

    // Create HNSW vector indexes separately (IF NOT EXISTS not supported for all index types)
    // Use DO block to check existence first
    let create_indexes = format!(
        r"
        DO $$ BEGIN
            IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'idx_chunks_embedding') THEN
                CREATE INDEX idx_chunks_embedding ON chunks USING hnsw (embedding {cos_ops});
            END IF;
        END $$;

        DO $$ BEGIN
            IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'idx_chunks_sig_embedding') THEN
                CREATE INDEX idx_chunks_sig_embedding ON chunks USING hnsw (signature_embedding {cos_ops}) WHERE signature_embedding IS NOT NULL;
            END IF;
        END $$;

        DO $$ BEGIN
            IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'idx_large_chunks_embedding') THEN
                CREATE INDEX idx_large_chunks_embedding ON large_chunks USING hnsw (embedding {cos_ops});
            END IF;
        END $$;

        DO $$ BEGIN
            IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'idx_notes_embedding') THEN
                CREATE INDEX idx_notes_embedding ON notes USING hnsw (embedding {cos_ops});
            END IF;
        END $$;

        DO $$ BEGIN
            IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'idx_modules_embedding') THEN
                CREATE INDEX idx_modules_embedding ON modules USING hnsw (embedding {cos_ops}) WHERE embedding IS NOT NULL;
            END IF;
        END $$;

        DO $$ BEGIN
            IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'idx_sections_embedding') THEN
                CREATE INDEX idx_sections_embedding ON sections USING hnsw (embedding {cos_ops})
                    WITH (m = 16, ef_construction = 200)
                    WHERE embedding IS NOT NULL;
            END IF;
        END $$;
        ",
    );
    sqlx::raw_sql(&create_indexes)
        .execute(pool)
        .await
        .context("Failed to create vector indexes")?;

    // Add space column to chunks (migration for existing databases)
    sqlx::raw_sql(
        r"ALTER TABLE chunks ADD COLUMN IF NOT EXISTS space TEXT NOT NULL DEFAULT 'code';",
    )
    .execute(pool)
    .await
    .context("Failed to add space column to chunks")?;

    // Add enrichment columns to chunks (migration for existing databases)
    for stmt in &[
        "ALTER TABLE chunks ADD COLUMN IF NOT EXISTS fqn TEXT",
        "ALTER TABLE chunks ADD COLUMN IF NOT EXISTS parent_fqn TEXT",
        "ALTER TABLE chunks ADD COLUMN IF NOT EXISTS start_line INT",
        "ALTER TABLE chunks ADD COLUMN IF NOT EXISTS end_line INT",
        "ALTER TABLE chunks ADD COLUMN IF NOT EXISTS visibility TEXT",
        "ALTER TABLE chunks ADD COLUMN IF NOT EXISTS is_async BOOLEAN DEFAULT FALSE",
        "ALTER TABLE chunks ADD COLUMN IF NOT EXISTS is_static BOOLEAN DEFAULT FALSE",
        "ALTER TABLE chunks ADD COLUMN IF NOT EXISTS is_exported BOOLEAN DEFAULT FALSE",
        "ALTER TABLE chunks ADD COLUMN IF NOT EXISTS doc TEXT",
    ] {
        sqlx::query(stmt)
            .execute(pool)
            .await
            .context("Failed to add enrichment column to chunks")?;
    }

    // Add cluster_id column to sections (migration for existing databases)
    sqlx::raw_sql(
        r"ALTER TABLE sections ADD COLUMN IF NOT EXISTS cluster_id UUID REFERENCES doc_clusters(id);",
    )
    .execute(pool)
    .await
    .context("Failed to add cluster_id column to sections")?;

    sqlx::raw_sql(r"CREATE INDEX IF NOT EXISTS idx_sections_cluster ON sections(cluster_id);")
        .execute(pool)
        .await
        .context("Failed to create sections cluster index")?;

    // ═══ Saga tables (dual-write reliability) ═══
    sqlx::raw_sql(
        r"
        CREATE TABLE IF NOT EXISTS sagas (
            id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            saga_type       TEXT NOT NULL,
            idempotency_key TEXT UNIQUE,
            repo_name       TEXT NOT NULL,
            status          TEXT NOT NULL DEFAULT 'running',
            context_json    JSONB NOT NULL DEFAULT '{}',
            result_json     JSONB,
            created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
        );

        CREATE TABLE IF NOT EXISTS saga_steps (
            id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            saga_id     UUID NOT NULL REFERENCES sagas(id) ON DELETE CASCADE,
            step_index  SMALLINT NOT NULL,
            step_name   TEXT NOT NULL,
            status      TEXT NOT NULL DEFAULT 'pending',
            error_msg   TEXT,
            created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
            UNIQUE (saga_id, step_index)
        );

        CREATE INDEX IF NOT EXISTS idx_sagas_idempotency
            ON sagas(idempotency_key) WHERE idempotency_key IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_sagas_status
            ON sagas(status) WHERE status IN ('running', 'compensating');
        CREATE INDEX IF NOT EXISTS idx_saga_steps_saga
            ON saga_steps(saga_id);
        ",
    )
    .execute(pool)
    .await
    .context("Failed to create saga tables")?;

    // ═══ Knowledge saga columns + indexes ═══
    // Extend sagas table for knowledge-saga use (grouping related notes).
    for stmt in &[
        "ALTER TABLE sagas ADD COLUMN IF NOT EXISTS name TEXT",
        "ALTER TABLE sagas ADD COLUMN IF NOT EXISTS source_type TEXT",
        "ALTER TABLE sagas ADD COLUMN IF NOT EXISTS source_ref TEXT",
        "ALTER TABLE sagas ADD COLUMN IF NOT EXISTS summary TEXT",
        "ALTER TABLE sagas ADD COLUMN IF NOT EXISTS resolved_at TIMESTAMPTZ",
    ] {
        sqlx::query(stmt).execute(pool).await.ok();
    }

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_sagas_repo ON sagas (repo_name)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_sagas_repo_status ON sagas (repo_name, status)")
        .execute(pool)
        .await?;

    // Unique partial index to prevent duplicate knowledge sagas for the same source_ref
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_sagas_unique_source_ref \
         ON sagas (repo_name, source_type, source_ref) \
         WHERE source_ref IS NOT NULL AND saga_type = 'knowledge'",
    )
    .execute(pool)
    .await
    .ok(); // ok() — may fail if index already exists with different definition

    // Full-text search column and index
    sqlx::raw_sql(
        r"
        DO $$ BEGIN
            IF NOT EXISTS (
                SELECT 1 FROM information_schema.columns
                WHERE table_name = 'chunks' AND column_name = 'content_tsv'
            ) THEN
                ALTER TABLE chunks ADD COLUMN content_tsv tsvector
                    GENERATED ALWAYS AS (
                        to_tsvector('english', coalesce(name,'') || ' ' || coalesce(signature,'') || ' ' || coalesce(content,''))
                    ) STORED;
                CREATE INDEX idx_chunks_tsv ON chunks USING GIN (content_tsv);
            END IF;
        END $$;
        ",
    )
    .execute(pool)
    .await
    .context("Failed to create full-text search column")?;

    // version_coordinate columns for Doc Space (A2b-1)
    sqlx::query("ALTER TABLE documents ADD COLUMN IF NOT EXISTS version_coordinate JSONB")
        .execute(pool)
        .await
        .context("Failed to add version_coordinate column to documents")?;
    sqlx::query("ALTER TABLE sections ADD COLUMN IF NOT EXISTS version_coordinate JSONB")
        .execute(pool)
        .await
        .context("Failed to add version_coordinate column to sections")?;

    // A2d-1 onboarding status machine on sources.
    for ddl in [
        "ALTER TABLE sources ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'approved'",
        "ALTER TABLE sources ADD COLUMN IF NOT EXISTS submitter TEXT",
        "ALTER TABLE sources ADD COLUMN IF NOT EXISTS reviewed_by TEXT",
        "ALTER TABLE sources ADD COLUMN IF NOT EXISTS reviewed_at TIMESTAMPTZ",
    ] {
        sqlx::query(ddl)
            .execute(pool)
            .await
            .with_context(|| format!("Failed sources A2d-1 migration: {ddl}"))?;
    }

    // A2d-2 probe columns on sources.
    for ddl in [
        "ALTER TABLE sources ADD COLUMN IF NOT EXISTS adapter_id TEXT",
        "ALTER TABLE sources ADD COLUMN IF NOT EXISTS classification TEXT",
        "ALTER TABLE sources ADD COLUMN IF NOT EXISTS probe_note TEXT",
    ] {
        sqlx::query(ddl)
            .execute(pool)
            .await
            .with_context(|| format!("Failed sources A2d-2 migration: {ddl}"))?;
    }

    // Admin trust-chain (delegated admin with cascade revocation).
    sqlx::raw_sql(
        r"
        CREATE TABLE IF NOT EXISTS admin_grants (
            id               UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
            grantee_username TEXT        NOT NULL,
            granter_username TEXT        NOT NULL,
            granted_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
            revoked_at       TIMESTAMPTZ,
            revoked_by       TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_admin_grants_grantee
            ON admin_grants(grantee_username);

        CREATE INDEX IF NOT EXISTS idx_admin_grants_granter_active
            ON admin_grants(granter_username)
            WHERE revoked_at IS NULL;
        ",
    )
    .execute(pool)
    .await
    .context("Failed to create admin_grants table")?;

    // Full-text search for sections (Doc space BM25)
    sqlx::query(
        "ALTER TABLE sections ADD COLUMN IF NOT EXISTS content_tsv tsvector \
         GENERATED ALWAYS AS (to_tsvector('english', coalesce(heading,'') || ' ' || content)) STORED"
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_sections_tsv ON sections USING GIN (content_tsv)")
        .execute(pool)
        .await?;

    // Full-text search for notes (Human space BM25)
    // Note: array_to_string is STABLE not IMMUTABLE, so we create an IMMUTABLE wrapper
    // for use in the generated column expression.
    sqlx::query(
        "CREATE OR REPLACE FUNCTION immutable_array_to_string(arr text[], sep text) \
         RETURNS text LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$ \
           SELECT array_to_string(arr, sep); \
         $$",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "ALTER TABLE notes ADD COLUMN IF NOT EXISTS content_tsv tsvector \
         GENERATED ALWAYS AS ( \
           to_tsvector('english', \
             coalesce(title,'') || ' ' || \
             coalesce(summary,'') || ' ' || \
             content || ' ' || \
             coalesce(immutable_array_to_string(facts, ' '),'') || ' ' || \
             coalesce(immutable_array_to_string(tags, ' '),'') \
           ) \
         ) STORED",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_notes_tsv ON notes USING GIN (content_tsv)")
        .execute(pool)
        .await?;

    // Staleness tracking for self-improving notes
    sqlx::query("ALTER TABLE notes ADD COLUMN IF NOT EXISTS staleness_score FLOAT DEFAULT 0.0")
        .execute(pool)
        .await?;
    sqlx::query("ALTER TABLE notes ADD COLUMN IF NOT EXISTS staleness_reasons TEXT[] DEFAULT '{}'")
        .execute(pool)
        .await?;
    sqlx::query("ALTER TABLE notes ADD COLUMN IF NOT EXISTS last_verified_at TIMESTAMPTZ")
        .execute(pool)
        .await?;
    sqlx::query("ALTER TABLE notes ADD COLUMN IF NOT EXISTS access_count INTEGER DEFAULT 0")
        .execute(pool)
        .await?;
    sqlx::query("ALTER TABLE notes ADD COLUMN IF NOT EXISTS last_accessed_at TIMESTAMPTZ")
        .execute(pool)
        .await?;
    sqlx::query("ALTER TABLE notes ADD COLUMN IF NOT EXISTS archived BOOLEAN DEFAULT false")
        .execute(pool)
        .await?;

    // Temporal + saga columns for notes
    for stmt in &[
        "ALTER TABLE notes ADD COLUMN IF NOT EXISTS saga_id UUID REFERENCES sagas(id)",
        "ALTER TABLE notes ADD COLUMN IF NOT EXISTS valid_at TIMESTAMPTZ DEFAULT now()",
        "ALTER TABLE notes ADD COLUMN IF NOT EXISTS invalid_at TIMESTAMPTZ",
        "ALTER TABLE notes ADD COLUMN IF NOT EXISTS superseded_by UUID",
    ] {
        sqlx::query(stmt).execute(pool).await.ok();
    }

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_notes_saga ON notes (saga_id)")
        .execute(pool)
        .await
        .ok();

    // ── Community tables (Global Query / Feature B) ────────────────────
    sqlx::query(&format!(
        "CREATE TABLE IF NOT EXISTS communities ( \
             id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
             repo_name TEXT NOT NULL, \
             level SMALLINT NOT NULL, \
             name TEXT, \
             summary TEXT, \
             member_count INTEGER, \
             embedding {vec_type}, \
             parent_id UUID REFERENCES communities(id), \
             created_at TIMESTAMPTZ DEFAULT now() \
         )"
    ))
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS community_members ( \
             community_id UUID NOT NULL REFERENCES communities(id) ON DELETE CASCADE, \
             chunk_id UUID REFERENCES chunks(id), \
             module_id UUID REFERENCES modules(id) \
         )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_communities_repo_level ON communities(repo_name, level)",
    )
    .execute(pool)
    .await?;

    sqlx::query(&format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'idx_communities_embedding') THEN \
             CREATE INDEX idx_communities_embedding ON communities \
               USING hnsw (embedding {cos_ops}) WITH (m = 16, ef_construction = 200); \
           END IF; \
         END $$;"
    ))
    .execute(pool)
    .await?;

    // ═══ Docs Corpus (versioned raw layer) ═══
    init_corpus_schema(pool).await?;

    // ═══ Auth sessions (persisted across restarts) ═══
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sessions (
            api_key      TEXT PRIMARY KEY,
            username     TEXT NOT NULL,
            name         TEXT,
            avatar_url   TEXT,
            gitlab_token TEXT NOT NULL,
            created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
            expires_at   TIMESTAMPTZ
        )",
    )
    .execute(pool)
    .await
    .context("Failed to create sessions table")?;

    // ═══ B1: MCP authentication tables ═══
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mcp_tokens (
            id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
            token_hash      BYTEA       NOT NULL UNIQUE,
            user_id         BIGINT      NOT NULL,
            user_login      TEXT        NOT NULL,
            label           TEXT,
            issued_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
            last_used_at    TIMESTAMPTZ,
            expires_at      TIMESTAMPTZ NOT NULL,
            revoked_at      TIMESTAMPTZ
        )",
    )
    .execute(pool)
    .await
    .context("Failed to create mcp_tokens table")?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS mcp_tokens_user_id_idx \
         ON mcp_tokens(user_id) WHERE revoked_at IS NULL",
    )
    .execute(pool)
    .await
    .context("Failed to create mcp_tokens_user_id_idx")?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS device_flow_pending (
            device_code        TEXT        PRIMARY KEY,
            user_code          TEXT        NOT NULL UNIQUE,
            client_id          TEXT        NOT NULL,
            created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
            expires_at         TIMESTAMPTZ NOT NULL,
            interval_secs      INTEGER     NOT NULL DEFAULT 5,
            last_polled_at     TIMESTAMPTZ,
            status             TEXT        NOT NULL,
            granted_user_id    BIGINT,
            granted_user_login TEXT,
            granted_token_id   UUID        REFERENCES mcp_tokens(id)
        )",
    )
    .execute(pool)
    .await
    .context("Failed to create device_flow_pending table")?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS revoked_passthrough_tokens (
            token_id_hash   TEXT        PRIMARY KEY,
            revoked_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
            revoked_by      BIGINT
        )",
    )
    .execute(pool)
    .await
    .context("Failed to create revoked_passthrough_tokens table")?;

    info!("PostgreSQL schema initialised successfully");
    Ok(())
}

/// Initialise only the docs-corpus tables (`corpus_versions`, `corpus_files`).
///
/// A lighter alternative to `init_schema` for test bootstrapping, mirroring
/// `init_auth_schema`: it touches no vector columns and runs no dimension
/// migration, so it is safe on a database whose embedding width differs.  All
/// statements are `IF NOT EXISTS`, so calling it repeatedly is idempotent.
///
/// `init_schema` calls this, so production migration is unaffected — but the
/// corpus integration tests connect to a bare database and must create these
/// tables themselves, which is why the DDL lives in its own entry point.
pub async fn init_corpus_schema(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(
        r"
        CREATE TABLE IF NOT EXISTS corpus_versions (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_name TEXT NOT NULL,
            version TEXT NOT NULL,
            sha TEXT NOT NULL,
            manifest JSONB NOT NULL,
            nav JSONB NOT NULL,
            source TEXT NOT NULL,
            page_count INT,
            asset_count INT,
            total_bytes BIGINT,
            is_latest BOOLEAN NOT NULL DEFAULT FALSE,
            is_tagged BOOLEAN NOT NULL DEFAULT FALSE,
            derive_status TEXT NOT NULL DEFAULT 'pending',
            derive_error TEXT,
            derive_job_id UUID,
            ingested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            UNIQUE (repo_name, sha)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS corpus_latest_one
            ON corpus_versions (repo_name) WHERE is_latest;
        CREATE INDEX IF NOT EXISTS idx_corpus_versions_repo ON corpus_versions (repo_name);

        CREATE TABLE IF NOT EXISTS corpus_files (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            version_id UUID NOT NULL REFERENCES corpus_versions(id) ON DELETE CASCADE,
            path TEXT NOT NULL,
            content BYTEA NOT NULL,
            content_hash TEXT NOT NULL,
            size_bytes BIGINT NOT NULL,
            is_markdown BOOLEAN NOT NULL,
            UNIQUE (version_id, path)
        );
        CREATE INDEX IF NOT EXISTS idx_corpus_files_version ON corpus_files (version_id);
        ",
    )
    .execute(pool)
    .await
    .context("Failed to create docs corpus tables")?;

    Ok(())
}

/// Initialise only the auth-specific tables (sessions, mcp_tokens,
/// device_flow_pending, revoked_passthrough_tokens).
///
/// This is a lighter alternative to `init_schema` for test bootstrapping: it
/// is safe to call on an existing database that already has a different vector
/// embedding width, because it never touches vector columns or runs dimension
/// migrations.  All statements are `IF NOT EXISTS`, so calling it repeatedly
/// is idempotent.
pub async fn init_auth_schema(pool: &PgPool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sessions (
            api_key      TEXT PRIMARY KEY,
            username     TEXT NOT NULL,
            name         TEXT,
            avatar_url   TEXT,
            gitlab_token TEXT NOT NULL,
            created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
            expires_at   TIMESTAMPTZ
        )",
    )
    .execute(pool)
    .await
    .context("Failed to create sessions table")?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mcp_tokens (
            id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
            token_hash      BYTEA       NOT NULL UNIQUE,
            user_id         BIGINT      NOT NULL,
            user_login      TEXT        NOT NULL,
            label           TEXT,
            issued_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
            last_used_at    TIMESTAMPTZ,
            expires_at      TIMESTAMPTZ NOT NULL,
            revoked_at      TIMESTAMPTZ
        )",
    )
    .execute(pool)
    .await
    .context("Failed to create mcp_tokens table")?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS mcp_tokens_user_id_idx \
         ON mcp_tokens(user_id) WHERE revoked_at IS NULL",
    )
    .execute(pool)
    .await
    .context("Failed to create mcp_tokens_user_id_idx")?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS device_flow_pending (
            device_code        TEXT        PRIMARY KEY,
            user_code          TEXT        NOT NULL UNIQUE,
            client_id          TEXT        NOT NULL,
            created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
            expires_at         TIMESTAMPTZ NOT NULL,
            interval_secs      INTEGER     NOT NULL DEFAULT 5,
            last_polled_at     TIMESTAMPTZ,
            status             TEXT        NOT NULL,
            granted_user_id    BIGINT,
            granted_user_login TEXT,
            granted_token_id   UUID        REFERENCES mcp_tokens(id)
        )",
    )
    .execute(pool)
    .await
    .context("Failed to create device_flow_pending table")?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS revoked_passthrough_tokens (
            token_id_hash   TEXT        PRIMARY KEY,
            revoked_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
            revoked_by      BIGINT
        )",
    )
    .execute(pool)
    .await
    .context("Failed to create revoked_passthrough_tokens table")?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS audit_log (
            id               BIGSERIAL    PRIMARY KEY,
            ts               TIMESTAMPTZ  NOT NULL DEFAULT now(),
            actor_user_id    BIGINT       NOT NULL,
            actor_token_id   TEXT         NOT NULL,
            auth_method      TEXT         NOT NULL,
            action           TEXT         NOT NULL,
            target_id        TEXT,
            before_hash      BYTEA,
            after_hash       BYTEA        NOT NULL,
            ip               INET,
            success          BOOLEAN      NOT NULL DEFAULT TRUE,
            response_summary TEXT
        )",
    )
    .execute(pool)
    .await
    .context("Failed to create audit_log table")?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS audit_log_actor_user_id_ts_idx \
         ON audit_log(actor_user_id, ts DESC)",
    )
    .execute(pool)
    .await
    .context("Failed to create audit_log_actor_user_id_ts_idx")?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS audit_log_action_ts_idx \
         ON audit_log(action, ts DESC)",
    )
    .execute(pool)
    .await
    .context("Failed to create audit_log_action_ts_idx")?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS llm_usage (
            id              BIGSERIAL    PRIMARY KEY,
            ts              TIMESTAMPTZ  NOT NULL DEFAULT now(),
            actor_user_id   BIGINT       NOT NULL,
            actor_token_id  TEXT         NOT NULL,
            kind            TEXT         NOT NULL,
            tokens_used     INT          NOT NULL,
            model           TEXT         NOT NULL
        )",
    )
    .execute(pool)
    .await
    .context("Failed to create llm_usage table")?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS llm_usage_actor_user_id_ts_idx \
         ON llm_usage(actor_user_id, ts DESC)",
    )
    .execute(pool)
    .await
    .context("Failed to create llm_usage_actor_user_id_ts_idx")?;

    // B4: per-user attribution upgrade — populate gitlab_user_id during
    // OAuth callback so quota and "my tokens" queries can key on a stable
    // numeric id rather than the mutable username.
    sqlx::query("ALTER TABLE sessions ADD COLUMN IF NOT EXISTS gitlab_user_id BIGINT")
        .execute(pool)
        .await
        .context("Failed to alter sessions.gitlab_user_id")?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS sessions_gitlab_user_id_idx \
         ON sessions(gitlab_user_id) WHERE gitlab_user_id IS NOT NULL",
    )
    .execute(pool)
    .await
    .context("Failed to create sessions_gitlab_user_id_idx")?;

    // B4: also add a non-partial index on mcp_tokens(user_id) so the
    // "list my tokens" query (no revoked_at filter — we want revoked
    // rows visible too) is a simple index lookup. B1's existing partial
    // index (WHERE revoked_at IS NULL) doesn't cover this query.
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS mcp_tokens_user_id_full_idx \
         ON mcp_tokens(user_id)",
    )
    .execute(pool)
    .await
    .context("Failed to create mcp_tokens_user_id_full_idx")?;

    // Admin trust-chain (additive; safe to call on existing DB).
    sqlx::raw_sql(
        r"
        CREATE TABLE IF NOT EXISTS admin_grants (
            id               UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
            grantee_username TEXT        NOT NULL,
            granter_username TEXT        NOT NULL,
            granted_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
            revoked_at       TIMESTAMPTZ,
            revoked_by       TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_admin_grants_grantee
            ON admin_grants(grantee_username);

        CREATE INDEX IF NOT EXISTS idx_admin_grants_granter_active
            ON admin_grants(granter_username)
            WHERE revoked_at IS NULL;
        ",
    )
    .execute(pool)
    .await
    .context("Failed to create admin_grants table")?;

    // Task 6 (B1, docs corpus): repo-scoped publish tokens (`akp_<32hex>`)
    // that authorize the docs-publish endpoint for exactly one repo. Mirrors
    // mcp_tokens but keyed by repo_name instead of user_id. `expires_at` is
    // nullable at the column level, but `PgPublishTokenRepo` always issues
    // it with `now() + 90 days` and slides it forward on each successful
    // validation — the same 90-day sliding TTL as MCP tokens (see
    // `repos/publish_token.rs`).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS publish_tokens (
            id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
            token_hash      BYTEA       UNIQUE NOT NULL,
            repo_name       TEXT        NOT NULL,
            created_by      TEXT        NOT NULL,
            created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
            expires_at      TIMESTAMPTZ,
            revoked_at      TIMESTAMPTZ,
            last_used_at    TIMESTAMPTZ
        )",
    )
    .execute(pool)
    .await
    .context("Failed to create publish_tokens table")?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS publish_tokens_created_by_idx \
         ON publish_tokens(created_by)",
    )
    .execute(pool)
    .await
    .context("Failed to create publish_tokens_created_by_idx")?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS publish_tokens_repo_name_idx \
         ON publish_tokens(repo_name) WHERE revoked_at IS NULL",
    )
    .execute(pool)
    .await
    .context("Failed to create publish_tokens_repo_name_idx")?;

    // Task 3 (MCP OAuth, docs-kit kit enablers): dynamic client registration
    // (RFC 7591) + authorization-code storage (RFC 6749 §4.1). `mcp_oauth_codes`
    // is created here but only populated starting Task 4's `/oauth/authorize`
    // + `/oauth/token` handlers.
    sqlx::raw_sql(
        r"
        CREATE TABLE IF NOT EXISTS mcp_oauth_clients (
            client_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            client_name TEXT,
            redirect_uris JSONB NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE TABLE IF NOT EXISTS mcp_oauth_codes (
            code_hash BYTEA PRIMARY KEY,
            client_id UUID NOT NULL REFERENCES mcp_oauth_clients(client_id),
            user_id BIGINT NOT NULL,
            user_login TEXT NOT NULL,
            code_challenge TEXT NOT NULL,
            redirect_uri TEXT NOT NULL,
            expires_at TIMESTAMPTZ NOT NULL,
            used_at TIMESTAMPTZ
        );
        ",
    )
    .execute(pool)
    .await
    .context("Failed to create mcp_oauth tables")?;

    info!("Auth schema initialised successfully");
    Ok(())
}
