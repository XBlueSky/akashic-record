# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.1.0] - 2026-02-12

### Added

- **Infrastructure**: Docker Compose with 3 services (Neo4j, Rust backend, Svelte frontend), multi-stage Dockerfiles, environment configuration
- **Backend Core**: Rust backend with axum, configurable embedding providers (candle local MiniLM-L6-v2, OpenAI API), Neo4j async driver with schema initialization
- **MCP Server**: rmcp SSE server with 3 tools — `save_context`, `search_context`, `get_project_summary` — for Claude Code integration
- **GitLab Webhook**: Push, TagPush, and MergeRequest event handler with MERGE-based upsert for Repository/Branch/Tag nodes
- **REST API**: 5 endpoints for the Svelte viewer — repos, branches, contexts (paginated), context detail, graph data
- **GitLab OAuth**: Device Flow authentication for headless CLI clients with 6-digit code exchange, Bearer token middleware, DashMap session store with TTL cleanup
- **Frontend Viewer**: Svelte 4 + Vite + TypeScript with timeline view (markdown-rendered context cards), graph view (SVG force-directed), dark mode, and responsive layout
- **Error Handling**: Shared `AppError` type with `IntoResponse` for clean `?`-based error propagation in API handlers
- **Vector Search**: Neo4j native vector index with dynamic dimensions based on active embedding provider
