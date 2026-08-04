//! Critical audit fixes for `delete_repo` (api/routes/repos.rs):
//!  1. FK-safe deletion order — a repo that ran community detection must still
//!     be deletable. `community_members.chunk_id`/`.module_id` FK-reference
//!     chunks/modules with NO ON DELETE CASCADE, so the chunk/module DELETE is
//!     blocked unless communities are cleaned first.
//!  2. Authorization — destroying a repo requires Maintainer+ GitLab access;
//!     an authenticated caller must not be able to delete a repo they cannot
//!     access (IDOR).

mod common;

use pgvector::Vector;
use uuid::Uuid;

async fn seed_chunk(pg: &sqlx::PgPool, repo: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO chunks (id, repo_name, module_path, chunk_type, name, content, embedding) \
         VALUES ($1, $2, 'm', 'function', 'f', 'body', $3)",
    )
    .bind(id)
    .bind(repo)
    .bind(Vector::from(vec![0.0_f32; 1536]))
    .execute(pg)
    .await
    .expect("seed chunk");
    id
}

async fn chunk_count(pg: &sqlx::PgPool, repo: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks WHERE repo_name = $1")
        .bind(repo)
        .fetch_one(pg)
        .await
        .expect("count chunks");
    n
}

#[tokio::test]
#[serial_test::serial]
async fn delete_repo_data_purges_repo_that_ran_community_detection() {
    let env = common::TestEnv::start().await;
    let repo = "audit-del-community";
    let chunk_id = seed_chunk(env.pg_pool(), repo).await;

    // A community whose member references the chunk — `community_members.chunk_id`
    // FK-references `chunks(id)` with NO ON DELETE CASCADE.
    let community_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO communities (id, repo_name, level, member_count) VALUES ($1, $2, 0, 1)",
    )
    .bind(community_id)
    .bind(repo)
    .execute(env.pg_pool())
    .await
    .expect("seed community");
    sqlx::query("INSERT INTO community_members (community_id, chunk_id) VALUES ($1, $2)")
        .bind(community_id)
        .bind(chunk_id)
        .execute(env.pg_pool())
        .await
        .expect("seed community_member");

    let result = akashic_record::api::routes::repos::delete_repo_data(&env.state, repo).await;
    assert!(
        result.is_ok(),
        "delete_repo_data must succeed even when community_members reference chunks",
    );

    assert_eq!(
        chunk_count(env.pg_pool(), repo).await,
        0,
        "chunks must be deleted"
    );
    let (communities,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM communities WHERE repo_name = $1")
            .bind(repo)
            .fetch_one(env.pg_pool())
            .await
            .expect("count communities");
    assert_eq!(communities, 0, "communities must be deleted");
}

#[tokio::test]
#[serial_test::serial]
async fn delete_repo_rejects_caller_without_maintainer_access() {
    let env = common::TestEnv::start().await;
    let repo = "audit-del-authz";
    seed_chunk(env.pg_pool(), repo).await;

    let client = reqwest::Client::new();
    let resp = client
        .delete(format!("http://{}/api/v1/repos/{}", env.app_addr, repo))
        .header("Cookie", format!("ak_session={}", env.session_token))
        .send()
        .await
        .expect("DELETE repo");

    // In the test env GitLab is unreachable, so the Maintainer+ access gate
    // fails closed (non-2xx). The security invariant: a caller that does not
    // pass the access check must NOT destroy the repo's data.
    assert!(
        !resp.status().is_success(),
        "delete must not succeed without authorization (got {})",
        resp.status()
    );
    assert_eq!(
        chunk_count(env.pg_pool(), repo).await,
        1,
        "repo data must survive an unauthorized delete attempt"
    );
}
