//! Application-service port traits: `AuthService`, `SearchService`,
//! `NavigationService`, `GraphService`, `CurationService`, `RepoService`,
//! `IngestService`.
//!
//! These traits represent the *use-case surface* shared between HTTP handlers
//! and MCP tools. They sit above the repo ports (infrastructure) and below the
//! Axum/MCP adapters. `AppState` carries `Arc<dyn SearchService>` etc. so
//! handlers stay thin instead of constructing concrete structs.
//!
//! **Boundary rules:**
//! - Only domain types as method inputs/outputs (no sqlx, neo4rs, axum).
//! - `DomainResult` return type throughout: services map their failure modes
//!   to typed [`crate::DomainError`] variants (NotFound / Conflict / Forbidden
//!   / BadRequest / Unauthorized / Internal) so adapters select HTTP/MCP status
//!   by matching the variant, never by string-matching an error message.
//!   The one exception is `AuthService::complete_web_login`, which keeps its
//!   dedicated `WebLoginError` for the browser login flow.
//! - All traits are `#[async_trait]`, `dyn`-safe, and `Send + Sync`.
//!
//! **serde (Slice C):** the `Serialize`-deriving structs below are use-case
//! **result DTOs** — service outputs serialized by the HTTP/MCP adapters, i.e.
//! boundary markers by design, not pure domain values. See the serde policy in
//! [`crate::types`].

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::{
    ChangeImpactReport, GraphRagQuery, GraphRagResponse, ImpactReport, KnowledgeSearchItem,
    KnowledgeSearchRequest, PreferSpace, ReferenceRow, RelinkResult, SymbolCandidate,
};

mod auth;
mod curation;
mod graph;
mod ingest;
mod navigation;
mod repo;
mod search;

pub use auth::*;
pub use curation::*;
pub use graph::*;
pub use ingest::*;
pub use navigation::*;
pub use repo::*;
pub use search::*;
