//! C3: schema migration discipline.
//!
//! - `MigrateAction` is the operation to run (up / verify / down).
//! - `MigrateOnBoot` is the env-driven boot-time policy.
//! - `run` is the single dispatcher used by both the CLI subcommand and
//!   the boot-time path.

use anyhow::Result;

use akashic_config::Config;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrateAction {
    Up { allow_destructive: bool },
    Verify,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrateOnBoot {
    Auto,
    True,
    False,
    UpWithDestructive,
}

impl MigrateOnBoot {
    /// Parse from env string. Unrecognized values fall back to `Auto`
    /// with a warn-level tracing log.
    pub fn from_env_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => MigrateOnBoot::Auto,
            "true" | "1" | "yes" => MigrateOnBoot::True,
            "false" | "0" | "no" => MigrateOnBoot::False,
            "up_with_destructive" | "destructive" => MigrateOnBoot::UpWithDestructive,
            other => {
                tracing::warn!(
                    event = "migrate_on_boot_unrecognized",
                    value = other,
                    "MIGRATE_ON_BOOT='{other}' not recognized; defaulting to 'auto'",
                );
                MigrateOnBoot::Auto
            }
        }
    }

    /// Resolve to a concrete action given whether we're in production.
    /// `Auto` is the only variant that branches on production state.
    pub fn resolve(self, is_prod: bool) -> MigrateAction {
        match (self, is_prod) {
            (MigrateOnBoot::Auto, true) => MigrateAction::Verify,
            (MigrateOnBoot::Auto, false) => MigrateAction::Up {
                allow_destructive: false,
            },
            (MigrateOnBoot::True, _) => MigrateAction::Up {
                allow_destructive: false,
            },
            (MigrateOnBoot::False, _) => MigrateAction::Verify,
            (MigrateOnBoot::UpWithDestructive, _) => MigrateAction::Up {
                allow_destructive: true,
            },
        }
    }
}

// ── Verify checks (read-only) ─────────────────────────────────────────────

use std::collections::HashSet;

use sqlx::PgPool;

use akashic_store_neo4j::Neo4jPool;

/// Canonical list of PostgreSQL tables the binary expects.
/// Sourced from `db::pg::init_schema` + `init_auth_schema` CREATE TABLE
/// statements. If those add or remove tables, this list MUST be updated
/// in the same commit.
const PG_TABLES: &[&str] = &[
    // From init_schema:
    "chunks",
    "large_chunks",
    "notes",
    "modules",
    "ingestion_jobs",
    "sources",
    "documents",
    "sections",
    "doc_clusters",
    "communities",
    "community_members",
    "sagas",
    "saga_steps",
    // From init_auth_schema (B1, B2, B3, B4):
    "mcp_tokens",
    "audit_log",
    "llm_usage",
    "sessions",
    "device_flow_pending",
    "revoked_passthrough_tokens",
    // Task 6 (B1, docs corpus): repo-scoped publish tokens.
    "publish_tokens",
    // Task 5 (C1, docs corpus): versioned raw corpus layer.
    "corpus_versions",
    "corpus_files",
    // Task 3 (MCP OAuth, docs-kit kit enablers): dynamic client registration
    // (RFC 7591) + authorization-code storage.
    "mcp_oauth_clients",
    "mcp_oauth_codes",
];

/// Canonical list of Neo4j constraints. Sourced from
/// `db::schema::init_schema`. Update in lockstep.
const NEO4J_CONSTRAINTS: &[&str] = &[
    "repo_name_unique",
    "category_name_unique",
    "module_pgid",
    "chunk_pgid",
    "document_pg_id",
    "section_pg_id",
    "tag_name",
    "note_uuid",
    "note_pg_id",
    "flow_pg_id",
];

async fn assert_pg_tables_exist(pg: &PgPool) -> Result<()> {
    let existing: HashSet<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = 'public'",
    )
    .fetch_all(pg)
    .await?
    .into_iter()
    .collect();
    let missing: Vec<&str> = PG_TABLES
        .iter()
        .copied()
        .filter(|t| !existing.contains(*t))
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "PG schema verify failed: missing tables {missing:?}. \
             Run 'akashic-record migrate up' to apply."
        );
    }
    Ok(())
}

async fn assert_pg_vector_dim(pg: &PgPool, expected_vec_type: &str) -> Result<()> {
    let needs = akashic_store_pg::vector_precision_migration_needed(pg, expected_vec_type).await?;
    if needs {
        anyhow::bail!(
            "PG vector dim mismatch: chunks.embedding does not match expected '{expected_vec_type}'. \
             Run 'akashic-record migrate up --allow-destructive' (or set \
             MIGRATE_ON_BOOT=up_with_destructive) to apply the reshape."
        );
    }
    Ok(())
}

async fn assert_neo4j_constraints_exist(neo: &Neo4jPool) -> Result<()> {
    let rows = neo
        .query(neo4rs::query("SHOW CONSTRAINTS YIELD name"))
        .await?;
    let mut existing: HashSet<String> = HashSet::new();
    for row in rows {
        if let Ok(name) = row.get::<String>("name") {
            existing.insert(name);
        }
    }
    let missing: Vec<&str> = NEO4J_CONSTRAINTS
        .iter()
        .copied()
        .filter(|c| !existing.contains(*c))
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "Neo4j schema verify failed: missing constraints {missing:?}. \
             Run 'akashic-record migrate up' to apply."
        );
    }
    Ok(())
}

/// Run the requested migration action.
pub async fn run(action: MigrateAction, cfg: &Config) -> Result<()> {
    use std::sync::Arc;

    use akashic_embed::{self as embedding, EmbeddingProvider};

    match action {
        MigrateAction::Down => {
            anyhow::bail!(
                "'migrate down' is not supported: migrations are forward-only by design."
            );
        }
        MigrateAction::Up { allow_destructive } => {
            let pg = akashic_store_pg::connect(&cfg.database_url).await?;
            let neo = Neo4jPool::connect(cfg).await?;

            // Build embedding provider just to read its dimension. The
            // CancellationToken is a no-op here — migrate is short-lived.
            let cancel = tokio_util::sync::CancellationToken::new();
            let raw_embedder: Arc<dyn EmbeddingProvider> =
                Arc::from(embedding::build_provider(cfg, cancel).await?);
            let dim = raw_embedder.dimensions();
            let vec_type = cfg.vector_type(dim);
            let cos_ops = cfg.cosine_ops();

            akashic_store_pg::init_schema(&pg, dim, &vec_type, cos_ops).await?;
            akashic_store_pg::init_auth_schema(&pg).await?;
            akashic_store_neo4j::schema::init_schema(&neo, dim).await?;

            if allow_destructive {
                let migrated =
                    akashic_store_pg::migrate_vector_precision(&pg, &vec_type, cos_ops).await?;
                if migrated {
                    tracing::info!(event = "migrate_vector_precision_applied");
                }
            } else {
                let needed =
                    akashic_store_pg::vector_precision_migration_needed(&pg, &vec_type).await?;
                if needed {
                    tracing::warn!(
                        event = "migrate_vector_precision_pending",
                        expected_vec_type = %vec_type,
                        "Vector column reshape is pending. Re-run with --allow-destructive \
                         (or set MIGRATE_ON_BOOT=up_with_destructive) to apply.",
                    );
                }
            }
            tracing::info!(event = "migrate_up_complete", allow_destructive);
            Ok(())
        }
        MigrateAction::Verify => {
            let pg = akashic_store_pg::connect(&cfg.database_url).await?;
            let neo = Neo4jPool::connect(cfg).await?;
            let cancel = tokio_util::sync::CancellationToken::new();
            let raw_embedder: Arc<dyn EmbeddingProvider> =
                Arc::from(embedding::build_provider(cfg, cancel).await?);
            let expected_dim = raw_embedder.dimensions();
            let expected_vec_type = cfg.vector_type(expected_dim);

            assert_pg_tables_exist(&pg).await?;
            assert_pg_vector_dim(&pg, &expected_vec_type).await?;
            assert_neo4j_constraints_exist(&neo).await?;
            tracing::info!(event = "migrate_verify_ok");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_str_auto() {
        assert_eq!(MigrateOnBoot::from_env_str(""), MigrateOnBoot::Auto);
        assert_eq!(MigrateOnBoot::from_env_str("auto"), MigrateOnBoot::Auto);
        assert_eq!(MigrateOnBoot::from_env_str("AUTO"), MigrateOnBoot::Auto);
    }

    #[test]
    fn from_env_str_true() {
        assert_eq!(MigrateOnBoot::from_env_str("true"), MigrateOnBoot::True);
        assert_eq!(MigrateOnBoot::from_env_str("1"), MigrateOnBoot::True);
        assert_eq!(MigrateOnBoot::from_env_str("yes"), MigrateOnBoot::True);
    }

    #[test]
    fn from_env_str_false() {
        assert_eq!(MigrateOnBoot::from_env_str("false"), MigrateOnBoot::False);
        assert_eq!(MigrateOnBoot::from_env_str("0"), MigrateOnBoot::False);
        assert_eq!(MigrateOnBoot::from_env_str("no"), MigrateOnBoot::False);
    }

    #[test]
    fn from_env_str_up_with_destructive() {
        assert_eq!(
            MigrateOnBoot::from_env_str("up_with_destructive"),
            MigrateOnBoot::UpWithDestructive
        );
        assert_eq!(
            MigrateOnBoot::from_env_str("destructive"),
            MigrateOnBoot::UpWithDestructive
        );
    }

    #[test]
    fn from_env_str_unrecognized_defaults_to_auto() {
        assert_eq!(
            MigrateOnBoot::from_env_str("hunkydory"),
            MigrateOnBoot::Auto
        );
        assert_eq!(MigrateOnBoot::from_env_str("on"), MigrateOnBoot::Auto);
    }

    #[test]
    fn resolve_auto_in_production_returns_verify() {
        assert_eq!(MigrateOnBoot::Auto.resolve(true), MigrateAction::Verify);
    }

    #[test]
    fn resolve_auto_in_dev_returns_up_non_destructive() {
        assert_eq!(
            MigrateOnBoot::Auto.resolve(false),
            MigrateAction::Up {
                allow_destructive: false
            }
        );
    }

    #[test]
    fn resolve_true_returns_up_non_destructive_regardless() {
        assert_eq!(
            MigrateOnBoot::True.resolve(true),
            MigrateAction::Up {
                allow_destructive: false
            }
        );
        assert_eq!(
            MigrateOnBoot::True.resolve(false),
            MigrateAction::Up {
                allow_destructive: false
            }
        );
    }

    #[test]
    fn resolve_false_returns_verify_regardless() {
        assert_eq!(MigrateOnBoot::False.resolve(true), MigrateAction::Verify);
        assert_eq!(MigrateOnBoot::False.resolve(false), MigrateAction::Verify);
    }

    #[test]
    fn resolve_up_with_destructive_returns_up_destructive_regardless() {
        assert_eq!(
            MigrateOnBoot::UpWithDestructive.resolve(true),
            MigrateAction::Up {
                allow_destructive: true
            }
        );
        assert_eq!(
            MigrateOnBoot::UpWithDestructive.resolve(false),
            MigrateAction::Up {
                allow_destructive: true
            }
        );
    }
}
