//! PostgreSQL adapter for [`AdminGrantRepo`].
//!
//! Implements the delegated admin trust-chain: reachability from env roots via a
//! recursive CTE, plus grant/soft-revoke mutations.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use akashic_domain::ports::admin_grant::{AdminEntry, AdminGrantRepo};

/// PostgreSQL adapter implementing [`AdminGrantRepo`].
#[derive(Clone)]
pub struct PgAdminGrantRepo {
    pool: PgPool,
}

impl PgAdminGrantRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AdminGrantRepo for PgAdminGrantRepo {
    /// Recursive-CTE reachability check.
    ///
    /// ```sql
    /// WITH RECURSIVE admins(username) AS (
    ///     SELECT unnest($1::text[])
    ///   UNION
    ///     SELECT g.grantee_username
    ///     FROM admin_grants g
    ///     JOIN admins a ON a.username = g.granter_username
    ///     WHERE g.revoked_at IS NULL
    /// )
    /// SELECT EXISTS (SELECT 1 FROM admins WHERE username = $2)
    /// ```
    async fn is_admin(&self, roots: &[String], username: &str) -> Result<bool> {
        if roots.is_empty() {
            return Ok(false);
        }
        let (exists,): (bool,) = sqlx::query_as(
            "WITH RECURSIVE admins(username) AS ( \
                 SELECT unnest($1::text[]) \
               UNION \
                 SELECT g.grantee_username \
                 FROM admin_grants g \
                 JOIN admins a ON a.username = g.granter_username \
                 WHERE g.revoked_at IS NULL \
             ) \
             SELECT EXISTS (SELECT 1 FROM admins WHERE username = $2)",
        )
        .bind(roots)
        .bind(username)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    async fn list_admins(&self, roots: &[String]) -> Result<Vec<AdminEntry>> {
        // Two-part result: roots (no grant row) then reachable granted users.
        // We join the CTE back to admin_grants to get granter + granted_at for
        // non-root entries; roots appear with NULL granter/granted_at.
        if roots.is_empty() {
            return Ok(vec![]);
        }
        let rows: Vec<(String, Option<String>, Option<DateTime<Utc>>)> = sqlx::query_as(
            "WITH RECURSIVE admins(username) AS ( \
                 SELECT unnest($1::text[]) \
               UNION \
                 SELECT g.grantee_username \
                 FROM admin_grants g \
                 JOIN admins a ON a.username = g.granter_username \
                 WHERE g.revoked_at IS NULL \
             ) \
             SELECT a.username, \
                    g.granter_username, \
                    g.granted_at \
             FROM admins a \
             LEFT JOIN LATERAL ( \
                 SELECT granter_username, granted_at \
                 FROM admin_grants \
                 WHERE grantee_username = a.username \
                   AND revoked_at IS NULL \
                 ORDER BY granted_at ASC \
                 LIMIT 1 \
             ) g ON true \
             ORDER BY a.username",
        )
        .bind(roots)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(username, granter, granted_at)| AdminEntry {
                username,
                granter,
                granted_at,
            })
            .collect())
    }

    async fn grant(&self, grantee: &str, granter: &str) -> Result<uuid::Uuid> {
        let (id,): (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO admin_grants (grantee_username, granter_username) \
             VALUES ($1, $2) \
             RETURNING id",
        )
        .bind(grantee)
        .bind(granter)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    async fn revoke(
        &self,
        grantee: &str,
        revoked_by: &str,
        caller_is_root: bool,
        caller: &str,
    ) -> Result<u64> {
        let rows_affected = if caller_is_root {
            // Root can revoke any active grant for this grantee.
            sqlx::query(
                "UPDATE admin_grants \
                 SET revoked_at = now(), revoked_by = $2 \
                 WHERE grantee_username = $1 \
                   AND revoked_at IS NULL",
            )
            .bind(grantee)
            .bind(revoked_by)
            .execute(&self.pool)
            .await?
            .rows_affected()
        } else {
            // Non-root: only revoke grants that this caller made.
            sqlx::query(
                "UPDATE admin_grants \
                 SET revoked_at = now(), revoked_by = $2 \
                 WHERE grantee_username = $1 \
                   AND granter_username = $3 \
                   AND revoked_at IS NULL",
            )
            .bind(grantee)
            .bind(revoked_by)
            .bind(caller)
            .execute(&self.pool)
            .await?
            .rows_affected()
        };
        Ok(rows_affected)
    }
}

// ── Live-DB tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db_url() -> String {
        std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://akashic@localhost:5432/akashic".into())
    }

    async fn setup_pool() -> PgPool {
        let pool = crate::connect(&test_db_url()).await.unwrap();
        // Ensure admin_grants table exists (init_auth_schema is lighter weight
        // than init_schema and safe to call on a live dev DB).
        crate::schema::init_auth_schema(&pool).await.unwrap();
        pool
    }

    /// Delete all test grant rows for the given usernames to leave the DB clean.
    async fn cleanup(pool: &PgPool, users: &[&str]) {
        sqlx::query(
            "DELETE FROM admin_grants \
             WHERE grantee_username = ANY($1::text[]) \
                OR granter_username = ANY($1::text[])",
        )
        .bind(users)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires live Postgres; run with TEST_DATABASE_URL=... cargo test -- --ignored"]
    async fn cascade_revocation() {
        let pool = setup_pool().await;
        let r = PgAdminGrantRepo::new(pool.clone());

        let root = "trust_root_test";
        let a = "trust_a_test";
        let b = "trust_b_test";
        let c = "trust_c_test";
        let roots = vec![root.to_string()];

        // Cleanup before test (in case of prior failure).
        cleanup(&pool, &[root, a, b, c]).await;

        // root → A → B → C
        r.grant(a, root).await.unwrap();
        r.grant(b, a).await.unwrap();
        r.grant(c, b).await.unwrap();

        // All three should be reachable.
        assert!(r.is_admin(&roots, a).await.unwrap(), "A should be admin");
        assert!(r.is_admin(&roots, b).await.unwrap(), "B should be admin");
        assert!(r.is_admin(&roots, c).await.unwrap(), "C should be admin");
        assert!(
            !r.is_admin(&roots, "nobody").await.unwrap(),
            "nobody should not be admin"
        );

        // Revoke root→A: A, B, C should all lose admin (cascade).
        let revoked = r.revoke(a, root, true, root).await.unwrap();
        assert_eq!(revoked, 1, "one edge revoked");

        assert!(!r.is_admin(&roots, a).await.unwrap(), "A lost admin");
        assert!(
            !r.is_admin(&roots, b).await.unwrap(),
            "B lost admin (cascade)"
        );
        assert!(
            !r.is_admin(&roots, c).await.unwrap(),
            "C lost admin (cascade)"
        );

        // Re-grant root→A: all three regain admin.
        r.grant(a, root).await.unwrap();
        assert!(r.is_admin(&roots, a).await.unwrap(), "A regained admin");
        assert!(r.is_admin(&roots, b).await.unwrap(), "B regained (cascade)");
        assert!(r.is_admin(&roots, c).await.unwrap(), "C regained (cascade)");

        // Cleanup.
        cleanup(&pool, &[root, a, b, c]).await;
    }

    #[tokio::test]
    #[ignore = "requires live Postgres; run with TEST_DATABASE_URL=... cargo test -- --ignored"]
    async fn multipath_survives_single_revoke() {
        let pool = setup_pool().await;
        let r = PgAdminGrantRepo::new(pool.clone());

        let root = "mp_root_test";
        let a = "mp_a_test";
        let b = "mp_b_test";
        let c = "mp_c_test";
        let roots = vec![root.to_string()];

        cleanup(&pool, &[root, a, b, c]).await;

        // root→A, root→B, B→C, A→C  (C reachable via two paths)
        r.grant(a, root).await.unwrap();
        r.grant(b, root).await.unwrap();
        r.grant(c, b).await.unwrap();
        r.grant(c, a).await.unwrap();

        assert!(
            r.is_admin(&roots, c).await.unwrap(),
            "C admin via two paths"
        );

        // Revoke B→C: C is still admin via A→C.
        let revoked = r.revoke(c, root, true, root).await.unwrap();
        assert_eq!(revoked, 2, "both B→C and A→C revoked by root");

        // After root revokes ALL, C loses admin.
        assert!(
            !r.is_admin(&roots, c).await.unwrap(),
            "C lost admin after all grants revoked"
        );

        cleanup(&pool, &[root, a, b, c]).await;
    }

    #[tokio::test]
    #[ignore = "requires live Postgres; run with TEST_DATABASE_URL=... cargo test -- --ignored"]
    async fn multipath_partial_revoke_keeps_admin() {
        let pool = setup_pool().await;
        let r = PgAdminGrantRepo::new(pool.clone());

        let root = "mpp_root_test";
        let a = "mpp_a_test";
        let b = "mpp_b_test";
        let c = "mpp_c_test";
        let roots = vec![root.to_string()];

        cleanup(&pool, &[root, a, b, c]).await;

        // root→A, root→B, B→C, A→C
        r.grant(a, root).await.unwrap();
        r.grant(b, root).await.unwrap();
        r.grant(c, b).await.unwrap();
        r.grant(c, a).await.unwrap();

        assert!(
            r.is_admin(&roots, c).await.unwrap(),
            "C admin via two paths"
        );

        // Revoke only B→C (non-root B revokes its own grant to C):
        // B is a non-root admin revoking only its own edge.
        let revoked = r.revoke(c, b, false, b).await.unwrap();
        assert_eq!(revoked, 1, "B revoked its own grant to C");

        // C still admin via A→C path.
        assert!(
            r.is_admin(&roots, c).await.unwrap(),
            "C still admin via A→C"
        );

        cleanup(&pool, &[root, a, b, c]).await;
    }

    #[tokio::test]
    #[ignore = "requires live Postgres; run with TEST_DATABASE_URL=... cargo test -- --ignored"]
    async fn non_root_cannot_revoke_others_grant() {
        let pool = setup_pool().await;
        let r = PgAdminGrantRepo::new(pool.clone());

        let root = "nrr_root_test";
        let a = "nrr_a_test";
        let b = "nrr_b_test";
        let roots = vec![root.to_string()];

        cleanup(&pool, &[root, a, b]).await;

        // root→A, root→B
        r.grant(a, root).await.unwrap();
        r.grant(b, root).await.unwrap();

        // A tries to revoke B's grant (which was made by root, not A).
        let revoked = r.revoke(b, a, false, a).await.unwrap();
        assert_eq!(revoked, 0, "non-root A cannot revoke root's grant to B");

        assert!(
            r.is_admin(&roots, b).await.unwrap(),
            "B still admin after failed revoke attempt"
        );

        // Root can revoke B.
        let revoked = r.revoke(b, root, true, root).await.unwrap();
        assert_eq!(revoked, 1);
        assert!(!r.is_admin(&roots, b).await.unwrap(), "B lost admin");

        cleanup(&pool, &[root, a, b]).await;
    }
}
