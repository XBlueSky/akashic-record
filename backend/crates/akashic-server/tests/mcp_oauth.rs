//! Integration tests for Task 3 (MCP OAuth metadata + dynamic client
//! registration — RFC 8414 / RFC 9728 / RFC 7591).
//!
//! Requires the live Postgres bench (`common::TestEnv::start()`); export
//! `DATABASE_URL`/`TEST_DATABASE_URL` at :5432 (the akashic_test_support
//! `test_pg_pool()` :5433 default is a dead port in this environment).
//!
//! NOTE: written for CI — not run against the persistent dev stack in this
//! session (per task instructions, to avoid burning host resources on the
//! full `common::TestEnv` bring-up). The metadata content and redirect_uri
//! validation logic are already covered by pure unit tests in
//! `akashic-http/src/auth/mcp_oauth.rs`, and the repo round-trip by
//! `akashic-store-pg/tests/oauth_client.rs` (both run and green this
//! session — see task-3-report.md).

mod common;

use serde_json::Value;

fn url(env: &common::TestEnv, path: &str) -> String {
    format!("http://{}{path}", env.app_addr)
}

#[tokio::test]
#[serial_test::serial]
async fn protected_resource_metadata_returns_expected_shape() {
    let env = common::TestEnv::start().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(&env, "/.well-known/oauth-protected-resource"))
        .send()
        .await
        .expect("GET .well-known/oauth-protected-resource");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json body");
    let resource = body["resource"].as_str().expect("resource string");
    assert!(!resource.is_empty());
    assert_eq!(body["authorization_servers"][0], resource);
}

#[tokio::test]
#[serial_test::serial]
async fn authorization_server_metadata_advertises_register_authorize_token() {
    let env = common::TestEnv::start().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(&env, "/.well-known/oauth-authorization-server"))
        .send()
        .await
        .expect("GET .well-known/oauth-authorization-server");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json body");
    let issuer = body["issuer"].as_str().expect("issuer string").to_string();
    assert_eq!(
        body["authorization_endpoint"],
        format!("{issuer}/oauth/authorize")
    );
    assert_eq!(body["token_endpoint"], format!("{issuer}/oauth/token"));
    assert_eq!(
        body["registration_endpoint"],
        format!("{issuer}/oauth/register")
    );
    assert_eq!(
        body["response_types_supported"],
        serde_json::json!(["code"])
    );
    assert_eq!(
        body["grant_types_supported"],
        serde_json::json!(["authorization_code"])
    );
    assert_eq!(
        body["code_challenge_methods_supported"],
        serde_json::json!(["S256"])
    );
    assert_eq!(
        body["token_endpoint_auth_methods_supported"],
        serde_json::json!(["none"])
    );
}

#[tokio::test]
#[serial_test::serial]
async fn register_happy_path_returns_201_public_client() {
    let env = common::TestEnv::start().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(&env, "/oauth/register"))
        .json(&serde_json::json!({
            "redirect_uris": ["http://127.0.0.1:33418/callback"],
            "client_name": "claude-code-test",
        }))
        .send()
        .await
        .expect("POST /oauth/register");
    assert_eq!(resp.status(), 201);
    let body: Value = resp.json().await.expect("json body");
    assert!(body["client_id"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(
        body["redirect_uris"],
        serde_json::json!(["http://127.0.0.1:33418/callback"])
    );
    assert_eq!(body["client_name"], "claude-code-test");
    assert_eq!(body["token_endpoint_auth_method"], "none");

    sqlx::query("DELETE FROM mcp_oauth_clients WHERE client_name = 'claude-code-test'")
        .execute(env.pg_pool())
        .await
        .ok();
}

#[tokio::test]
#[serial_test::serial]
async fn register_rejects_non_loopback_non_https_redirect_uri() {
    let env = common::TestEnv::start().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(&env, "/oauth/register"))
        .json(&serde_json::json!({
            "redirect_uris": ["http://evil.com/callback"],
        }))
        .send()
        .await
        .expect("POST /oauth/register");
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_redirect_uri");
}

// ── Task 4: authorize + token (PKCE, one-time code) ──────────────────────────
//
// NOTE: written for CI — NOT run against the persistent dev stack in this
// session (TEST ENV POLICY: `common::TestEnv::start()` → `reset_state`
// TRUNCATEs, which would wipe dev data if pointed at the persistent :5432
// stack; do not run this file locally). PKCE math (S256 + the RFC 7636
// Appendix B vector) and the `next` same-origin-path validator are already
// covered by pure unit tests in `akashic-http/src/auth/mcp_oauth.rs` and
// `akashic-http/src/auth/web.rs` (both run and green this session — see
// task-4-report.md). This file exercises the full HTTP round trip those unit
// tests can't: real client registration, a real web session cookie, the
// 302 redirects, and the MCP 401 challenge.

use sha2::{Digest, Sha256};

/// Base64url-no-pad encode (RFC 4648 §5). Duplicated here rather than
/// depending on `akashic_http`'s private `base64url_nopad` (that function is
/// `pub(crate)` to the `akashic-http` crate and this is a different crate) —
/// small, fixed, self-contained, and pinned against the same RFC 7636
/// Appendix B vector the unit tests in `akashic-http` already verify.
fn base64url_nopad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
        if let Some(b1) = b1 {
            out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char);
        }
        if let Some(b2) = b2 {
            out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
        }
    }
    out
}

fn pkce_challenge(verifier: &str) -> String {
    base64url_nopad(&Sha256::digest(verifier.as_bytes()))
}

/// A client that does NOT follow redirects, so the test can assert on the
/// `Location` header of a 302 directly instead of chasing it.
fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("build no-redirect client")
}

/// Register a throwaway client for the authorize/token flow, returning
/// `(client_id, redirect_uri)`.
async fn register_test_client(env: &common::TestEnv, name: &str) -> (String, String) {
    let redirect_uri = "http://127.0.0.1:33418/callback".to_string();
    let client = reqwest::Client::new();
    let resp = client
        .post(url(env, "/oauth/register"))
        .json(&serde_json::json!({
            "redirect_uris": [redirect_uri],
            "client_name": name,
        }))
        .send()
        .await
        .expect("POST /oauth/register");
    assert_eq!(resp.status(), 201, "register_test_client setup failed");
    let body: Value = resp.json().await.expect("json body");
    let client_id = body["client_id"].as_str().expect("client_id").to_string();
    (client_id, redirect_uri)
}

/// `GET /oauth/authorize` with the `TestEnv`'s pre-populated session cookie,
/// returning the parsed `Location` header (the redirect target) so callers
/// can assert on its `code`/`state`/`error` query params.
async fn authorize_with_session(
    env: &common::TestEnv,
    client_id: &str,
    redirect_uri: &str,
    state_param: &str,
    code_challenge: &str,
) -> url::Url {
    let client = no_redirect_client();
    let resp = client
        .get(url(env, "/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("state", state_param),
            ("code_challenge", code_challenge),
            ("code_challenge_method", "S256"),
        ])
        .header("cookie", format!("ak_session={}", env.session_token))
        .send()
        .await
        .expect("GET /oauth/authorize");
    // axum's `Redirect::to()` deliberately emits 303 See Other (never 302);
    // for these GET-initiated OAuth redirects 303 is spec-compliant and this
    // preserves current production behavior — the tests were written against
    // an assumed 302 that never shipped.
    assert_eq!(resp.status(), 303, "authorize should redirect");
    let location = resp
        .headers()
        .get("location")
        .expect("Location header")
        .to_str()
        .expect("Location is valid ascii")
        .to_string();
    url::Url::parse(&location).expect("Location parses as a URL")
}

#[tokio::test]
#[serial_test::serial]
async fn authorize_then_token_round_trips_ak_token() {
    let env = common::TestEnv::start().await;
    let (client_id, redirect_uri) = register_test_client(&env, "t4-happy-path").await;
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"; // RFC 7636 App. B
    let challenge = pkce_challenge(verifier);

    let location =
        authorize_with_session(&env, &client_id, &redirect_uri, "state-abc", &challenge).await;
    assert!(
        location.as_str().starts_with(&redirect_uri),
        "must redirect back to the registered redirect_uri, got {location}"
    );
    let params: std::collections::HashMap<_, _> = location.query_pairs().into_owned().collect();
    assert_eq!(params.get("state").map(String::as_str), Some("state-abc"));
    let code = params.get("code").expect("code present").clone();
    assert!(!code.is_empty());

    let http = reqwest::Client::new();
    let resp = http
        .post(url(&env, "/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", client_id.as_str()),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .expect("POST /oauth/token");
    assert_eq!(resp.status(), 200, "token exchange should succeed");
    let body: Value = resp.json().await.expect("json body");
    let token = body["access_token"].as_str().expect("access_token");
    assert!(token.starts_with("ak_"), "got {token}");
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["expires_in"], 7_776_000);

    // The minted token must actually validate via the same MCP-token path
    // the device-code grant uses.
    let validated = env.state.auth_store.validate_mcp_token(token).await;
    assert!(validated.is_some(), "minted ak_ token must validate");

    // ── replay: the same code must now be invalid_grant (one-time-use) ──
    let replay = http
        .post(url(&env, "/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", client_id.as_str()),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .expect("POST /oauth/token (replay)");
    assert_eq!(replay.status(), 400);
    let replay_body: Value = replay.json().await.expect("json body");
    assert_eq!(replay_body["error"], "invalid_grant");
}

#[tokio::test]
#[serial_test::serial]
async fn token_rejects_wrong_pkce_verifier() {
    let env = common::TestEnv::start().await;
    let (client_id, redirect_uri) = register_test_client(&env, "t4-wrong-verifier").await;
    let challenge = pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");

    let location =
        authorize_with_session(&env, &client_id, &redirect_uri, "state-xyz", &challenge).await;
    let params: std::collections::HashMap<_, _> = location.query_pairs().into_owned().collect();
    let code = params.get("code").expect("code present").clone();

    let http = reqwest::Client::new();
    let resp = http
        .post(url(&env, "/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", client_id.as_str()),
            ("code_verifier", "not-the-right-verifier"),
        ])
        .send()
        .await
        .expect("POST /oauth/token");
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_grant");
}

#[tokio::test]
#[serial_test::serial]
async fn authorize_without_session_redirects_to_login_with_next() {
    let env = common::TestEnv::start().await;
    let (client_id, redirect_uri) = register_test_client(&env, "t4-no-session").await;
    let challenge = pkce_challenge("some-verifier-value-not-used-here");

    let client = no_redirect_client();
    let resp = client
        .get(url(&env, "/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("state", "state-noauth"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ])
        // Deliberately no `cookie` header — no web session.
        .send()
        .await
        .expect("GET /oauth/authorize");
    assert_eq!(resp.status(), 303);
    let location = resp
        .headers()
        .get("location")
        .expect("Location header")
        .to_str()
        .expect("ascii")
        .to_string();
    assert!(
        location.contains("/auth/web/login?next="),
        "expected a login redirect carrying next=, got {location}"
    );
    assert!(
        location.contains("oauth%2Fauthorize"),
        "next should be the percent-encoded original /oauth/authorize request, got {location}"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn authorize_rejects_unknown_client_id_without_redirecting() {
    let env = common::TestEnv::start().await;
    let client = no_redirect_client();
    let resp = client
        .get(url(&env, "/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", "00000000-0000-0000-0000-000000000000"),
            ("redirect_uri", "http://127.0.0.1:33418/callback"),
            ("state", "s"),
            ("code_challenge", "challenge"),
            ("code_challenge_method", "S256"),
        ])
        .header("cookie", format!("ak_session={}", env.session_token))
        .send()
        .await
        .expect("GET /oauth/authorize");
    // An unrecognized client_id must 400 directly — never redirect to an
    // unvalidated URI (RFC 6749 §4.1.2.1).
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_client");
}

/// Task 4 introduced this as a narrow carve-out: only an anonymous
/// `tools/call` naming a write tool got the RFC 9728 challenge, via a
/// body-peek in `mcp_auth`. Task 3 replaced that with full-auth,
/// fail-closed: `mcp_auth` now 401s EVERY anonymous `/mcp` request the
/// same way, peek machinery removed entirely (see
/// `akashic-mcp/src/mcp_middleware.rs`'s module doc comment). The
/// assertions below still hold unchanged under the new policy — a write
/// tool call is one anonymous shape among many that now get this
/// response — so this test is kept as an extra guard on the OAuth
/// integration surface; `mcp_contract.rs`'s
/// `mcp_anonymous_request_gets_401_challenge` and
/// `mcp_anonymous_write_gets_401_challenge` cover the same behavior (plus
/// non-write shapes) from the contract-test side.
#[tokio::test]
#[serial_test::serial]
async fn mcp_anonymous_write_tool_call_gets_401_challenge() {
    let env = common::TestEnv::start().await;
    let mcp_url = format!("{}/mcp", env.mcp_addr.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .post(&mcp_url)
        .header("content-type", "application/json")
        .body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "save_note", "arguments": {}}
            })
            .to_string(),
        )
        .send()
        .await
        .expect("POST /mcp");
    assert_eq!(resp.status(), 401);
    let challenge = resp
        .headers()
        .get("www-authenticate")
        .expect("WWW-Authenticate header present")
        .to_str()
        .expect("ascii");
    assert!(challenge.contains("resource_metadata="));
    assert!(challenge.contains(".well-known/oauth-protected-resource"));
}
