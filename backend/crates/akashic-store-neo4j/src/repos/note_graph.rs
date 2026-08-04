//! Neo4j adapter for `NoteGraphRepo`.
//!
//! All Cypher is moved verbatim from the original call-sites:
//! - `akashic-server::mcp::tools::save_note` (`create_note_node`)
//! - `akashic-server::api::routes::notes::update_note` (`update_note_node`)
//! - `akashic-server::api::routes::notes::delete_note` (`detach_delete_note`)

use anyhow::Result;
use async_trait::async_trait;
use neo4rs::query;
use uuid::Uuid;

use akashic_domain::ports::NoteGraphRepo;

use crate::Neo4jPool;

/// Neo4j adapter implementing [`NoteGraphRepo`].
#[derive(Clone)]
pub struct Neo4jNoteGraphRepo {
    graph: Neo4jPool,
}

impl Neo4jNoteGraphRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl NoteGraphRepo for Neo4jNoteGraphRepo {
    async fn create_note_node(
        &self,
        pg_id: Uuid,
        repo_name: &str,
        branch_name: &str,
        category: &str,
    ) -> Result<()> {
        // Cypher moved verbatim from akashic-server::mcp::tools::save_note.
        // Returns Err on Neo4j failure — the caller (CurationServices::save_note)
        // compensates by deleting the PG row.
        let cypher = "
            MERGE (r:Repository {name: $repo_name})
            MERGE (r)-[:HAS_BRANCH]->(b:Branch {name: $branch_name})
            CREATE (c:Note {pg_id: $pg_id, repo_name: $repo_name})
            CREATE (c)-[:LINKED_TO]->(b)
            CREATE (c)-[:BELONGS_TO]->(r)
            WITH c
            MATCH (cat:Category {name: $category})
            CREATE (c)-[:TAGGED_AS]->(cat)
        ";
        self.graph
            .execute(
                query(cypher)
                    .param("repo_name", repo_name)
                    .param("branch_name", branch_name)
                    .param("pg_id", pg_id.to_string().as_str())
                    .param("category", category),
            )
            .await
    }

    async fn update_note_node(&self, pg_id: Uuid, title: &str, category: &str) -> Result<()> {
        // Non-fatal absence of the node is acceptable (Neo4j and PG can drift;
        // the node may have been cleaned up independently).
        //
        // Category is stored TWO ways at create time: the `n.category` property
        // AND a (:Note)-[:TAGGED_AS]->(:Category) edge. Every reader resolves
        // category via the EDGE (trace_decision_history's Category{name:...}
        // filter, the graph view's coloring), so setting only the property left
        // the edge pointing at the old category — a re-categorized note kept its
        // old color and stayed in / out of decision histories incorrectly.
        // Re-point the edge in the same write.
        let cypher = "
            MATCH (n:Note {pg_id: $uuid})
            SET n.title = $title, n.category = $category
            WITH n
            OPTIONAL MATCH (n)-[t:TAGGED_AS]->(:Category)
            DELETE t
            WITH n
            MATCH (cat:Category {name: $category})
            MERGE (n)-[:TAGGED_AS]->(cat)
        ";
        let _ = self
            .graph
            .execute(
                query(cypher)
                    .param("uuid", pg_id.to_string().as_str())
                    .param("title", title)
                    .param("category", category),
            )
            .await;
        Ok(())
    }

    async fn detach_delete_note(&self, pg_id: Uuid) -> Result<()> {
        // Cypher moved verbatim from akashic-server::api::routes::notes::delete_note.
        // Non-fatal if the node is absent.
        let _ = self
            .graph
            .execute(
                query("MATCH (n:Note {pg_id: $uuid}) DETACH DELETE n")
                    .param("uuid", pg_id.to_string().as_str()),
            )
            .await;
        Ok(())
    }

    async fn create_supersedes_edge(&self, old_id: Uuid, new_id: Uuid) -> Result<()> {
        // Idempotent MERGE; the NEWER note points at the OLDER one, mirroring
        // Postgres `old.superseded_by = new.id`. Returns Err on Neo4j failure —
        // CurationService::supersede_note compensates via unmark_superseded.
        //
        // Both MATCHes are required before the MERGE runs: if either :Note node
        // is absent, the query silently matches zero rows and MERGE never fires
        // — but a bare `execute` would still return Ok(()), letting
        // supersede_note believe the mirror write succeeded when no edge was
        // ever created (the exact split the strict-compensation guarantee is
        // meant to preclude). RETURN a match count and error when it's zero so
        // a missing node is treated the same as any other write failure.
        let cypher = "
            MATCH (old:Note {pg_id: $old_id})
            MATCH (new:Note {pg_id: $new_id})
            MERGE (new)-[:SUPERSEDES]->(old)
            RETURN count(*) AS c
        ";
        let rows = self
            .graph
            .query(
                query(cypher)
                    .param("old_id", old_id.to_string().as_str())
                    .param("new_id", new_id.to_string().as_str()),
            )
            .await?;
        let matched = rows
            .first()
            .and_then(|r| r.get::<i64>("c").ok())
            .unwrap_or(0);
        if matched == 0 {
            return Err(anyhow::anyhow!(
                "create_supersedes_edge: no :Note node found for old_id={old_id} and/or new_id={new_id} \
                 (MATCH...MATCH...MERGE matched zero rows) — edge NOT created"
            ));
        }
        Ok(())
    }
}
