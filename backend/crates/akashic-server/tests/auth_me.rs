//! D2 §5.2 (reframed 2026-05-11) — session-cookie auth roundtrip.
//!
//! Pre-populated logged-in actor → GET /api/v1/auth/me → 200 + actor identity.
//! Missing cookie → 401.

mod common;

#[tokio::test]
#[serial_test::serial]
async fn test_auth_me_returns_test_actor() {
    let env = common::TestEnv::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://{}/api/v1/auth/me", env.app_addr))
        .header("Cookie", format!("ak_session={}", env.session_token))
        .send()
        .await
        .expect("GET /api/v1/auth/me");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "expected 200 OK with valid session cookie"
    );
    let body: serde_json::Value = resp.json().await.expect("auth/me JSON body");
    assert_eq!(
        body.get("username").and_then(|v| v.as_str()),
        Some("test-user"),
        "username should match the bench-issued session's user_info"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn test_auth_me_without_cookie_returns_401() {
    let env = common::TestEnv::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://{}/api/v1/auth/me", env.app_addr))
        .send()
        .await
        .expect("GET /api/v1/auth/me unauthenticated");

    assert_eq!(
        resp.status().as_u16(),
        401,
        "anonymous access to a protected route must return 401"
    );
}
