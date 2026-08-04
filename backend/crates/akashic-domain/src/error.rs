//! Typed application-service error.
//!
//! `DomainError` is the error type returned by the service-port traits
//! (`akashic_domain::ports::services`). It lets adapters (HTTP/MCP) derive a
//! response *status* from the error **variant** instead of substring-matching
//! the message text (the prior `if e.to_string().contains("not found")` smell).
//!
//! Service IMPLs map their internal `anyhow` errors to `DomainError`:
//! `Internal(#[from] anyhow::Error)` makes `?` on any `anyhow`-yielding call
//! auto-convert, so most impl bodies only need the signature change; the
//! specific variants are produced explicitly where the impl recognises a case
//! (e.g. a missing row → `NotFound`). Store/adapter internals keep `anyhow`.

/// Typed error at the service-port boundary.
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    /// Requested entity does not exist → HTTP 404. The payload names the entity.
    #[error("{0} not found")]
    NotFound(String),
    /// Request conflicts with current state → HTTP 409.
    #[error("{0}")]
    Conflict(String),
    /// Authenticated but not permitted → HTTP 403.
    #[error("{0}")]
    Forbidden(String),
    /// Malformed/invalid request → HTTP 400.
    #[error("{0}")]
    BadRequest(String),
    /// Missing/invalid credentials → HTTP 401.
    #[error("unauthorized")]
    Unauthorized,
    /// Anything else (infra failure, unexpected) → HTTP 500. Quota-exceeded
    /// currently flows through here as an `anyhow` chain so the existing 429
    /// downcast keeps working (see the boundary design doc).
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

/// JSON (de)serialization failures inside a service are internal errors → 500.
/// A dedicated `From` (rather than `#[from]`) routes them into `Internal` so
/// `?` on a `serde_json` call works in a `DomainResult` fn without a distinct
/// variant. (`serde_json` is already an akashic-domain dependency.)
impl From<serde_json::Error> for DomainError {
    fn from(e: serde_json::Error) -> Self {
        DomainError::Internal(anyhow::Error::new(e))
    }
}

/// Convenience alias for service-port return types.
pub type DomainResult<T> = Result<T, DomainError>;
