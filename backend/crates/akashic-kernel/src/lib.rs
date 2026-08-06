//! Kernel — lowest-level shared types that must not depend on any domain
//! module (`mcp`, `auth`, `llm`, `embedding`, …).
//!
//! Currently contains:
//!   - `actor`: `Authenticated` trait + `AuthMethod` enum (actor-identity)
//!   - `audit`: `AuditPort` trait + `NoopAudit` stub
//!   - `events`: `AppEvent` enum (broadcast events for SSE fan-out)

pub mod actor;
pub mod audit;
pub mod events;

pub use actor::{AuthMethod, Authenticated, parse_actor_id};
pub use audit::AuditPort;
pub use events::AppEvent;


fn  badly_formatted        () {}
