use akashic_test_support::test_pg_pool;

#[tokio::test]
async fn corpus_tables_exist_and_latest_is_unique_per_repo() {
    let pool = test_pg_pool().await;
    akashic_store_pg::init_corpus_schema(&pool)
        .await
        .expect("init_corpus_schema");
    sqlx::query("DELETE FROM corpus_versions WHERE repo_name = 't1'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO corpus_versions (repo_name, version, sha, manifest, nav, source, is_latest)
         VALUES ('t1','1.0.0','aaa1111','{}','{}','push', true)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // 第二個 is_latest=true 同 repo 必須撞 partial unique index
    let dup = sqlx::query(
        "INSERT INTO corpus_versions (repo_name, version, sha, manifest, nav, source, is_latest)
         VALUES ('t1','1.0.1','bbb2222','{}','{}','push', true)",
    )
    .execute(&pool)
    .await;
    assert!(
        dup.is_err(),
        "second is_latest per repo must violate corpus_latest_one"
    );
}
