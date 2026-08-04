//! Neo4j adapter for `ChunkGraphRepo`.
//!
//! Cypher moved verbatim from `akashic-ingestion::ingestion::store.rs`.

use anyhow::Result;
use async_trait::async_trait;
use neo4rs::query;

use akashic_domain::ports::{ChunkGraphNode, ChunkGraphRepo};

use crate::Neo4jPool;

/// Neo4j adapter implementing [`ChunkGraphRepo`].
#[derive(Clone)]
pub struct Neo4jChunkGraphRepo {
    graph: Neo4jPool,
}

impl Neo4jChunkGraphRepo {
    pub fn new(graph: Neo4jPool) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl ChunkGraphRepo for Neo4jChunkGraphRepo {
    async fn create_chunk_nodes_batch(
        &self,
        repo_name: &str,
        chunks: &[ChunkGraphNode],
    ) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        // Decompose into parallel scalar arrays (mirrors original store.rs batching).
        let pg_ids: Vec<String> = chunks.iter().map(|c| c.pg_id.clone()).collect();
        let names: Vec<String> = chunks.iter().map(|c| c.name.clone()).collect();
        let chunk_types: Vec<String> = chunks.iter().map(|c| c.chunk_type.clone()).collect();
        let module_paths: Vec<String> = chunks.iter().map(|c| c.module_path.clone()).collect();
        let fqns: Vec<Option<String>> = chunks.iter().map(|c| c.fqn.clone()).collect();
        let parent_fqns: Vec<Option<String>> =
            chunks.iter().map(|c| c.parent_fqn.clone()).collect();
        let start_lines: Vec<i64> = chunks.iter().map(|c| c.start_line).collect();
        let end_lines: Vec<i64> = chunks.iter().map(|c| c.end_line).collect();
        let visibilities: Vec<String> = chunks.iter().map(|c| c.visibility.clone()).collect();
        let is_asyncs: Vec<bool> = chunks.iter().map(|c| c.is_async).collect();
        let is_statics: Vec<bool> = chunks.iter().map(|c| c.is_static).collect();
        let is_exporteds: Vec<bool> = chunks.iter().map(|c| c.is_exported).collect();
        let http_methods: Vec<Option<String>> =
            chunks.iter().map(|c| c.http_method.clone()).collect();
        let http_paths: Vec<Option<String>> = chunks.iter().map(|c| c.http_path.clone()).collect();

        self.graph
            .execute(
                query(
                    "UNWIND range(0, size($pg_ids)-1) AS i \
                     MERGE (c:Chunk {pg_id: $pg_ids[i]}) \
                     SET c.repo_name = $repo_name, c.name = $names[i], \
                         c.chunk_type = $chunk_types[i], \
                         c.module_path = $module_paths[i], \
                         c.fqn = $fqns[i], c.parent_fqn = $parent_fqns[i], \
                         c.start_line = $start_lines[i], c.end_line = $end_lines[i], \
                         c.visibility = $visibilities[i], \
                         c.is_async = $is_asyncs[i], c.is_static = $is_statics[i], \
                         c.is_exported = $is_exporteds[i], \
                         c.http_method = $http_methods[i], \
                         c.http_path = $http_paths[i]",
                )
                .param("pg_ids", pg_ids)
                .param("repo_name", repo_name)
                .param("names", names)
                .param("chunk_types", chunk_types)
                .param("module_paths", module_paths)
                .param("fqns", fqns)
                .param("parent_fqns", parent_fqns)
                .param("start_lines", start_lines)
                .param("end_lines", end_lines)
                .param("visibilities", visibilities)
                .param("is_asyncs", is_asyncs)
                .param("is_statics", is_statics)
                .param("is_exporteds", is_exporteds)
                .param("http_methods", http_methods)
                .param("http_paths", http_paths),
            )
            .await?;

        Ok(())
    }

    async fn create_tagged_with_edges(
        &self,
        tag_pg_ids: Vec<String>,
        tag_names: Vec<String>,
    ) -> Result<()> {
        if tag_pg_ids.is_empty() {
            return Ok(());
        }

        self.graph
            .execute(
                query(
                    "UNWIND range(0, size($tag_pg_ids)-1) AS i \
                     MERGE (t:Tag {name: $tag_names[i]}) \
                     WITH t, i \
                     MATCH (c:Chunk {pg_id: $tag_pg_ids[i]}) \
                     MERGE (c)-[:TAGGED_WITH]->(t)",
                )
                .param("tag_pg_ids", tag_pg_ids)
                .param("tag_names", tag_names),
            )
            .await?;

        Ok(())
    }

    async fn delete_chunks_by_repo(&self, repo_name: &str) -> Result<()> {
        self.graph
            .execute(
                query("MATCH (c:Chunk {repo_name: $repo_name}) DETACH DELETE c")
                    .param("repo_name", repo_name),
            )
            .await?;
        Ok(())
    }

    async fn delete_chunks_by_module(&self, repo_name: &str, module_path: &str) -> Result<()> {
        self.graph
            .execute(
                neo4rs::query(
                    "MATCH (m:Module {repo_name: $repo, path: $path})-[:HAS_CHUNK]->(c:Chunk) \
                     DETACH DELETE c",
                )
                .param("repo", repo_name)
                .param("path", module_path),
            )
            .await?;
        Ok(())
    }
}
