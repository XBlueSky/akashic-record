pub mod repos;
pub mod schema;

pub use self::pool::Neo4jPool;
pub use repos::{
    Neo4jChunkGraphRepo, Neo4jCommunityGraphRepo, Neo4jDocClusterGraphRepo, Neo4jDocumentGraphRepo,
    Neo4jEdgeRepo, Neo4jFlowGraphRepo, Neo4jGraphReadRepo, Neo4jGraphTraversalRepo,
    Neo4jGraphWriteRepo, Neo4jIngestEdgeRepo, Neo4jModuleGraphRepo, Neo4jNoteGraphRepo,
    Neo4jRepoGraphRepo, Neo4jSnapshotRepo,
};

mod pool {
    use anyhow::{Context, Result};
    use neo4rs::{Graph, Query};
    use secrecy::ExposeSecret;

    use akashic_config::Config;

    /// Wrapper around the neo4rs `Graph` connection providing convenience methods.
    #[derive(Clone)]
    pub struct Neo4jPool {
        graph: Graph,
    }

    impl Neo4jPool {
        /// Create a new pool from application config.
        pub async fn connect(cfg: &Config) -> Result<Self> {
            let graph = Graph::new(
                &cfg.neo4j_uri,
                &cfg.neo4j_user,
                cfg.neo4j_password.expose_secret(),
            )
            .await
            .context("Failed to connect to Neo4j")?;
            Ok(Self { graph })
        }

        /// Execute a Cypher query that returns no rows (DDL, writes).
        pub async fn execute(&self, q: Query) -> Result<()> {
            self.graph.run(q).await.context("Neo4j execute failed")?;
            Ok(())
        }

        /// Execute a Cypher query and collect all result rows.
        pub async fn query(&self, q: Query) -> Result<Vec<neo4rs::Row>> {
            let mut result = self.graph.execute(q).await.context("Neo4j query failed")?;
            let mut rows = Vec::new();
            while let Some(row) = result.next().await? {
                rows.push(row);
            }
            Ok(rows)
        }

        /// Get a reference to the underlying Graph for advanced operations.
        pub fn inner(&self) -> &Graph {
            &self.graph
        }
    }
}
