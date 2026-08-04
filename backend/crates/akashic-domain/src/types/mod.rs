//! Domain value + row types.
//!
//! ## serde policy (Slice C)
//!
//! `serde` on a type here is a **boundary marker**, never incidental. A type
//! carries it ONLY if it provably crosses a boundary:
//!   (a) a use-case **result DTO** returned by a service trait and serialized
//!       by an HTTP/MCP adapter (e.g. `ScoredNode`, `GraphRagResponse`,
//!       `SagaWithCount`, `NoteHealthSummary`),
//!   (b) a **persistence** row (re)serialized to a DB JSONB column, or
//!   (c) a service **input** deserialized by an adapter (`GraphRagQuery`;
//!       `Space`/`PreferSpace` are dual-use enums embedded in the above).
//! Pure internal domain/row values carry NO `serde`. HTTP/MCP adapters define
//! finer wire DTOs where the wire shape diverges from the result DTO. The set
//! is compiler-enforced: stripping a derive that is actually needed fails the
//! build.

// ── Chunk / Module domain row types ──────────────────────────────────────────
//
// These are plain Rust structs (no sqlx/neo4rs derives) so akashic-domain
// stays infra-free. Adapter crates define a companion `FromRow` helper and
// convert via `impl From<AdapterRow> for DomainRow`.

// OAuth-config health-check value types (returned by the GitLabGateway port).
pub mod oauth_health;
pub use oauth_health::{CheckResult, CheckStatus, ValidationReport};

mod analysis;
pub mod corpus;
mod identity;
mod ingest_rows;
mod note_saga;
mod retrieval_rows;
mod service_dto;
pub mod snapshot;

pub use analysis::*;
pub use corpus::*;
pub use identity::*;
pub use ingest_rows::*;
pub use note_saga::*;
pub use retrieval_rows::*;
pub use service_dto::*;
pub use snapshot::*;
