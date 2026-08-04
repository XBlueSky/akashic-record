//! Port trait for the `admin_grants` table — delegated admin trust chain.
//!
//! `AdminGrantRepo` is the infra-free domain contract for reachability queries
//! and grant/revoke mutations.  The Pg adapter lives in `akashic-store-pg`.
//!
//! **Domain boundary rules:**
//! - No `sqlx`, no infra types.
//! - `anyhow::Result` throughout.
//! - All traits are `#[async_trait]`, `dyn`-safe, `Send + Sync`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

// ── Row types ─────────────────────────────────────────────────────────────────

/// One entry in the admin set (roots + granted).
#[derive(Debug, Clone)]
pub struct AdminEntry {
    /// GitLab username.
    pub username: String,
    /// `None` for env roots; `Some(granter)` for delegated admins.
    pub granter: Option<String>,
    /// `None` for env roots.
    pub granted_at: Option<DateTime<Utc>>,
}

// ── Port trait ────────────────────────────────────────────────────────────────

/// Repository contract for `admin_grants`.
#[async_trait]
pub trait AdminGrantRepo: Send + Sync {
    /// Return `true` iff `username` is reachable from any of `roots` through
    /// non-revoked grant edges.  `roots` themselves are always admin (the CTE
    /// seed); if `roots` is empty the result is always `false` (fail-closed).
    ///
    /// Implemented as a recursive CTE — cascade revocation is automatic.
    async fn is_admin(&self, roots: &[String], username: &str) -> anyhow::Result<bool>;

    /// Return the full admin set: env roots (granter=None) unioned with every
    /// username reachable via non-revoked grants.
    async fn list_admins(&self, roots: &[String]) -> anyhow::Result<Vec<AdminEntry>>;

    /// Record a new active grant edge `granter → grantee`.
    ///
    /// Idempotent: multiple active grants from different granters are fine;
    /// the CTE handles deduplication.  Returns the new row's UUID.
    async fn grant(&self, grantee: &str, granter: &str) -> anyhow::Result<uuid::Uuid>;

    /// Soft-revoke active grant(s) for `grantee`.
    ///
    /// - If `caller_is_root`: revoke all active grants for `grantee`.
    /// - Otherwise: revoke only grants where `granter_username == caller`.
    ///
    /// Returns the number of rows revoked (0 ⇒ nothing to revoke or caller
    /// not permitted — the distinction must be made by the service layer).
    async fn revoke(
        &self,
        grantee: &str,
        revoked_by: &str,
        caller_is_root: bool,
        caller: &str,
    ) -> anyhow::Result<u64>;
}
