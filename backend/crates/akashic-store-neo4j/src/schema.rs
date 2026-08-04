use anyhow::Result;
use neo4rs::query;
use tracing::{info, warn};

use crate::Neo4jPool;

/// Initialise the Neo4j schema: uniqueness constraints, category seed data,
/// and the vector index for semantic search.
pub async fn init_schema(pool: &Neo4jPool, vector_dims: usize) -> Result<()> {
    info!("Initialising Neo4j schema …");

    // ── Uniqueness constraints ───────────────────────────────────────
    let constraints = [
        // Existing
        "CREATE CONSTRAINT repo_name_unique IF NOT EXISTS FOR (r:Repository) REQUIRE r.name IS UNIQUE",
        "CREATE CONSTRAINT category_name_unique IF NOT EXISTS FOR (c:Category) REQUIRE c.name IS UNIQUE",
        "CREATE CONSTRAINT module_pgid IF NOT EXISTS FOR (m:Module) REQUIRE m.pg_id IS UNIQUE",
        "CREATE CONSTRAINT chunk_pgid IF NOT EXISTS FOR (c:Chunk) REQUIRE c.pg_id IS UNIQUE",
        // Doc Space
        "CREATE CONSTRAINT document_pg_id IF NOT EXISTS FOR (d:Document) REQUIRE d.pg_id IS UNIQUE",
        "CREATE CONSTRAINT section_pg_id IF NOT EXISTS FOR (s:Section) REQUIRE s.pg_id IS UNIQUE",
        "CREATE CONSTRAINT tag_name IF NOT EXISTS FOR (t:Tag) REQUIRE t.name IS UNIQUE",
        // Note (replaces Context)
        "CREATE CONSTRAINT note_uuid IF NOT EXISTS FOR (n:Note) REQUIRE n.uuid IS UNIQUE",
        "CREATE CONSTRAINT note_pg_id IF NOT EXISTS FOR (n:Note) REQUIRE n.pg_id IS UNIQUE",
        // Execution Flows
        "CREATE CONSTRAINT flow_pg_id IF NOT EXISTS FOR (f:Flow) REQUIRE f.pg_id IS UNIQUE",
        // Communities (Global Query)
        "CREATE CONSTRAINT community_pg_id IF NOT EXISTS FOR (c:Community) REQUIRE c.pg_id IS UNIQUE",
        // Sagas
        "CREATE CONSTRAINT saga_pg_id IF NOT EXISTS FOR (s:Saga) REQUIRE s.pg_id IS UNIQUE",
    ];

    for cypher in constraints {
        pool.execute(query(cypher)).await?;
    }

    // ── Indexes ─────────────────────────────────────────────────────
    let indexes = ["CREATE INDEX saga_repo_idx IF NOT EXISTS FOR (s:Saga) ON (s.repo_name)"];

    for cypher in indexes {
        pool.execute(query(cypher)).await?;
    }

    // ── Migrate Context → Note labels ────────────────────────────────
    let migrations = [
        "MATCH (c:Context) WHERE NOT c:Note SET c:Note REMOVE c:Context",
        "MATCH (a)-[r:REFERS_TO]->(b) CREATE (a)-[:ATTACHED_TO]->(b) DELETE r",
    ];

    for cypher in migrations {
        if let Err(e) = pool.execute(query(cypher)).await {
            warn!("Migration query skipped (may already be done): {e}");
        }
    }

    // ── Drop old Context constraints (ignore errors) ─────────────────
    let old_constraints = [
        "DROP CONSTRAINT context_uuid_unique IF EXISTS",
        "DROP CONSTRAINT context_pgid IF EXISTS",
    ];

    for cypher in old_constraints {
        if let Err(e) = pool.execute(query(cypher)).await {
            warn!("Dropping old constraint skipped: {e}");
        }
    }

    // ── Seed the five canonical categories ───────────────────────────
    // CATEGORIES is now canonical in akashic_domain::types; we re-use it here.
    for name in akashic_domain::types::CATEGORIES {
        pool.execute(query("MERGE (:Category {name: $name})").param("name", *name))
            .await?;
    }

    // ── Vector index — dimension set by the active embedding provider ─
    // Drop old context_embedding_index if it exists
    if let Err(e) = pool
        .execute(query("DROP INDEX context_embedding_index IF EXISTS"))
        .await
    {
        warn!("Dropping old vector index skipped: {e}");
    }

    pool.execute(query(&format!(
        "CREATE VECTOR INDEX note_embedding_index IF NOT EXISTS \
             FOR (n:Note) ON (n.embedding) \
             OPTIONS {{indexConfig: {{ \
               `vector.dimensions`: {vector_dims}, \
               `vector.similarity_function`: 'cosine' \
             }}}}"
    )))
    .await?;

    info!("Neo4j schema initialised successfully");
    Ok(())
}
