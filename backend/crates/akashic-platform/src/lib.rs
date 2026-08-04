//! Akashic Record platform/infrastructure leaf crate.
//!
//! A2b (2026-06-04): carved out of `akashic-server` as a pure structural
//! move. Holds the HTTP-cross-cutting middleware (request id, metrics,
//! rate limiting), the observability install hooks, the readiness poller +
//! probes, the cooperative shutdown primitive, the schema-migration
//! dispatcher, and the D6 alerting subsystem.
//!
//! These modules sit at the infrastructure layer below the API/auth/MCP
//! handlers and depend only on `akashic-context` (for `AppState` and the
//! shared `ReadinessState`/`ProbeOutcome` types) plus leaf infra crates —
//! never on the interface layer that lives in `akashic-server`.

pub mod alerts;
pub mod middleware;
pub mod migrate;
pub mod observability;
pub mod readiness;
pub mod shutdown;
