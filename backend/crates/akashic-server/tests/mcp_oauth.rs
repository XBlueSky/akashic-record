//! Integration tests for MCP OAuth: discovery metadata (RFC 8414 / RFC 9728),
//! CIMD client validation (SEP-991, spec §4 — Task 5), the consent screen,
//! and the authorization-code + PKCE token exchange (RFC 6749 §4.1 / §4.1.3,
//! RFC 7636).
//!
//! Requires the live Postgres bench (`common::TestEnv::start()`); export
//! `DATABASE_URL`/`TEST_DATABASE_URL` at :5432 (the akashic_test_support
//! `test_pg_pool()` :5433 default is a dead port in this environment).
//!
//! NOTE: written for CI — not run against the persistent dev stack in this
//! session (per task instructions, to avoid burning host resources on the
//! full `common::TestEnv` bring-up). `common::TestEnv`'s `build_test_config`
//! sets `mcp_cimd_allow_loopback: true` (test-only — production hard-rejects
//! it via `Config::validate_for_production`), so every CIMD fixture below is
//! a plain `http://127.0.0.1:<port>` server spun up inside the test itself,
//! same pattern as `cimd.rs`'s own `#[cfg(test)]` fixtures (Task 4).

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
async fn authorization_server_metadata_advertises_cimd_support_not_registration() {
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
    // Task 5 (spec §4): CIMD replaces DCR — no more registration_endpoint.
    assert_eq!(body["client_id_metadata_document_supported"], true);
    assert!(body.get("registration_endpoint").is_none());
}

// ── Task 5: CIMD validation + consent screen + token (PKCE, one-time code) ──
//
// NOTE: written for CI — NOT run against the persistent dev stack in this
// session (TEST ENV POLICY: `common::TestEnv::start()` → `reset_state`
// TRUNCATEs, which would wipe dev data if pointed at the persistent :5432
// stack; do not run this file locally). PKCE math (S256 + the RFC 7636
// Appendix B vector), CIMD fetch/validate/SSRF-guard, and the `next`
// same-origin-path validator are already covered by pure/fixture-backed unit
// tests in `akashic-http/src/auth/mcp_oauth.rs`, `akashic-http/src/auth/cimd.rs`,
// and `akashic-http/src/auth/web.rs`. This file exercises the full HTTP round
// trip those can't: a real web session cookie, a real (loopback) CIMD fetch,
// the consent screen + POST, the 303 redirects, and the MCP 401 challenge.

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
/// `Location` header of a 303 directly instead of chasing it.
fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("build no-redirect client")
}

// ── CIMD fixture server (spec §4, SEP-991) ───────────────────────────────────
//
// Mirrors `akashic-http/src/auth/cimd.rs`'s own `#[cfg(test)]` fixture
// pattern (Task 4): bind a loopback listener first (so its URL is known),
// build the JSON body via `body_fn(&url)` (the document's own `client_id`
// field must equal the URL it's served from), then serve it. Loopback is
// allowed because `common::TestEnv`'s `build_test_config` sets
// `mcp_cimd_allow_loopback: true` for this exact reason.

#[derive(Clone)]
struct CimdFixtureState {
    body: std::sync::Arc<String>,
}

async fn cimd_fixture_handler(
    axum::extract::State(fx): axum::extract::State<CimdFixtureState>,
) -> impl axum::response::IntoResponse {
    (*fx.body).clone()
}

async fn spawn_cimd_fixture(body_fn: impl FnOnce(&str) -> String) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind cimd fixture listener");
    let addr = listener.local_addr().expect("local_addr");
    let doc_url = format!("http://{addr}/cimd");
    let body = body_fn(&doc_url);
    let app = axum::Router::new()
        .route("/cimd", axum::routing::get(cimd_fixture_handler))
        .with_state(CimdFixtureState {
            body: std::sync::Arc::new(body),
        });
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("cimd fixture server");
    });
    doc_url
}

fn cimd_doc_body(client_id_url: &str, redirect_uri: &str, client_name: &str) -> String {
    serde_json::json!({
        "client_id": client_id_url,
        "redirect_uris": [redirect_uri],
        "client_name": client_name,
        "logo_uri": null,
    })
    .to_string()
}

/// Spawn a well-formed CIMD fixture and return `(client_id_url, redirect_uri)`
/// for the authorize/token flow tests below.
async fn spawn_test_client(name: &str) -> (String, String) {
    const REDIRECT_URI: &str = "http://127.0.0.1:33418/callback";
    let name = name.to_string();
    let client_id = spawn_cimd_fixture(move |url| cimd_doc_body(url, REDIRECT_URI, &name)).await;
    (client_id, REDIRECT_URI.to_string())
}

/// Extract the `consent_id` hidden-field value from the consent page HTML
/// `authorize_with_session_get_consent_id` fetches — a small, deliberately
/// simple string search rather than pulling in an HTML parser dependency for
/// one field in a page this test suite itself controls the shape of.
fn extract_consent_id(html: &str) -> String {
    let marker = r#"name="consent_id" value=""#;
    let start = html
        .find(marker)
        .unwrap_or_else(|| panic!("consent_id hidden field not found in: {html}"))
        + marker.len();
    let rest = &html[start..];
    let end = rest.find('"').expect("closing quote after consent_id");
    rest[..end].to_string()
}

/// `GET /oauth/authorize` with the `TestEnv`'s pre-populated session cookie
/// and a CIMD-valid `client_id`/`redirect_uri` pair — asserts 200 + a consent
/// screen and returns the extracted `consent_id`.
async fn authorize_with_session_get_consent_id(
    env: &common::TestEnv,
    client_id: &str,
    redirect_uri: &str,
    state_param: &str,
    code_challenge: &str,
) -> String {
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
    assert_eq!(
        resp.status(),
        200,
        "authorize should render the consent screen for a CIMD-valid client + active session"
    );
    let html = resp.text().await.expect("body text");
    assert!(html.contains("Authorize MCP client"));
    extract_consent_id(&html)
}

/// `POST /oauth/authorize/consent` with the `TestEnv`'s pre-populated
/// session cookie.
async fn post_consent(
    env: &common::TestEnv,
    consent_id: &str,
    decision: &str,
) -> reqwest::Response {
    no_redirect_client()
        .post(url(env, "/oauth/authorize/consent"))
        .form(&[("consent_id", consent_id), ("decision", decision)])
        .header("cookie", format!("ak_session={}", env.session_token))
        .send()
        .await
        .expect("POST /oauth/authorize/consent")
}

#[tokio::test]
#[serial_test::serial]
async fn authorize_then_consent_then_token_round_trips_ak_token() {
    let env = common::TestEnv::start().await;
    let (client_id, redirect_uri) = spawn_test_client("t5-happy-path").await;
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"; // RFC 7636 App. B
    let challenge = pkce_challenge(verifier);

    let consent_id = authorize_with_session_get_consent_id(
        &env,
        &client_id,
        &redirect_uri,
        "state-abc",
        &challenge,
    )
    .await;

    let consent_resp = post_consent(&env, &consent_id, "approve").await;
    assert_eq!(
        consent_resp.status(),
        303,
        "consent approve should redirect"
    );
    let location = consent_resp
        .headers()
        .get("location")
        .expect("Location header")
        .to_str()
        .expect("ascii")
        .to_string();
    let location_url = url::Url::parse(&location).expect("Location parses as a URL");
    assert!(
        location_url.as_str().starts_with(&redirect_uri),
        "must redirect back to the CIMD-registered redirect_uri, got {location_url}"
    );
    let params: std::collections::HashMap<_, _> = location_url.query_pairs().into_owned().collect();
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
async fn consent_deny_redirects_with_access_denied() {
    let env = common::TestEnv::start().await;
    let (client_id, redirect_uri) = spawn_test_client("t5-deny").await;
    let challenge = pkce_challenge("verifier-not-used-because-denied");

    let consent_id = authorize_with_session_get_consent_id(
        &env,
        &client_id,
        &redirect_uri,
        "state-deny",
        &challenge,
    )
    .await;

    let resp = post_consent(&env, &consent_id, "deny").await;
    assert_eq!(resp.status(), 303, "consent deny should still redirect");
    let location = resp
        .headers()
        .get("location")
        .expect("Location header")
        .to_str()
        .expect("ascii")
        .to_string();
    let location_url = url::Url::parse(&location).expect("Location parses as a URL");
    assert!(location_url.as_str().starts_with(&redirect_uri));
    let params: std::collections::HashMap<_, _> = location_url.query_pairs().into_owned().collect();
    assert_eq!(
        params.get("error").map(String::as_str),
        Some("access_denied")
    );
    assert_eq!(params.get("state").map(String::as_str), Some("state-deny"));
    assert!(
        !params.contains_key("code"),
        "denied consent must not carry a code"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn consent_rejects_unknown_consent_id_with_400_not_redirect() {
    let env = common::TestEnv::start().await;
    let resp = post_consent(&env, &uuid::Uuid::new_v4().to_string(), "approve").await;
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_request");
}

#[tokio::test]
#[serial_test::serial]
async fn consent_without_session_is_401() {
    let env = common::TestEnv::start().await;
    // Deliberately no `cookie` header — no web session.
    let resp = reqwest::Client::new()
        .post(url(&env, "/oauth/authorize/consent"))
        .form(&[
            ("consent_id", uuid::Uuid::new_v4().to_string().as_str()),
            ("decision", "approve"),
        ])
        .send()
        .await
        .expect("POST /oauth/authorize/consent");
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
#[serial_test::serial]
async fn token_rejects_wrong_pkce_verifier() {
    let env = common::TestEnv::start().await;
    let (client_id, redirect_uri) = spawn_test_client("t5-wrong-verifier").await;
    let challenge = pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");

    let consent_id = authorize_with_session_get_consent_id(
        &env,
        &client_id,
        &redirect_uri,
        "state-xyz",
        &challenge,
    )
    .await;
    let consent_resp = post_consent(&env, &consent_id, "approve").await;
    assert_eq!(consent_resp.status(), 303);
    let location = consent_resp
        .headers()
        .get("location")
        .expect("Location header")
        .to_str()
        .expect("ascii")
        .to_string();
    let location_url = url::Url::parse(&location).expect("Location parses as a URL");
    let params: std::collections::HashMap<_, _> = location_url.query_pairs().into_owned().collect();
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
    let (client_id, redirect_uri) = spawn_test_client("t5-no-session").await;
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
        // Deliberately no `cookie` header — no web session. CIMD validation
        // still succeeds first (the fixture is reachable and well-formed),
        // so this proves the session check happens AFTER CIMD, not instead
        // of it.
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
async fn authorize_rejects_unreachable_cimd_client_without_redirecting() {
    let env = common::TestEnv::start().await;
    let client = no_redirect_client();
    let resp = client
        .get(url(&env, "/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            // Loopback port nothing is listening on — CIMD fetch fails.
            ("client_id", "http://127.0.0.1:1/cimd"),
            ("redirect_uri", "http://127.0.0.1:33418/callback"),
            ("state", "s"),
            ("code_challenge", "challenge"),
            ("code_challenge_method", "S256"),
        ])
        .header("cookie", format!("ak_session={}", env.session_token))
        .send()
        .await
        .expect("GET /oauth/authorize");
    // An unreachable/invalid CIMD client must 400 directly — never redirect
    // to an unvalidated URI (spec §4 failure-response principle).
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_client");
}

#[tokio::test]
#[serial_test::serial]
async fn authorize_rejects_cimd_document_with_mismatched_client_id_without_redirecting() {
    let env = common::TestEnv::start().await;
    // The document's own `client_id` field deliberately does NOT match the
    // URL it's served from — CimdError::ClientIdMismatch.
    let client_id = spawn_cimd_fixture(|_url| {
        serde_json::json!({
            "client_id": "https://mismatched.example.com/other.json",
            "redirect_uris": ["http://127.0.0.1:33418/callback"],
            "client_name": "Mismatched",
            "logo_uri": null,
        })
        .to_string()
    })
    .await;

    let client = no_redirect_client();
    let resp = client
        .get(url(&env, "/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "http://127.0.0.1:33418/callback"),
            ("state", "s"),
            ("code_challenge", "challenge"),
            ("code_challenge_method", "S256"),
        ])
        .header("cookie", format!("ak_session={}", env.session_token))
        .send()
        .await
        .expect("GET /oauth/authorize");
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_client");
}

#[tokio::test]
#[serial_test::serial]
async fn authorize_rejects_redirect_uri_not_in_cimd_document_without_redirecting() {
    let env = common::TestEnv::start().await;
    let (client_id, _registered_redirect_uri) = spawn_test_client("t5-redirect-mismatch").await;

    let client = no_redirect_client();
    let resp = client
        .get(url(&env, "/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            // Not the redirect_uri the CIMD document declared.
            ("redirect_uri", "http://127.0.0.1:33418/not-registered"),
            ("state", "s"),
            ("code_challenge", "challenge"),
            ("code_challenge_method", "S256"),
        ])
        .header("cookie", format!("ak_session={}", env.session_token))
        .send()
        .await
        .expect("GET /oauth/authorize");
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_client");
}

#[tokio::test]
#[serial_test::serial]
async fn authorize_rejects_bad_structural_params_before_cimd_fetch() {
    let env = common::TestEnv::start().await;
    let client = no_redirect_client();
    // client_id points nowhere reachable — if this ever got to the CIMD
    // fetch it would fail differently (invalid_client). It must not: the
    // structural response_type check runs first and 400s without ever
    // dialing out.
    let resp = client
        .get(url(&env, "/oauth/authorize"))
        .query(&[
            ("response_type", "token"),
            ("client_id", "http://127.0.0.1:1/cimd"),
            ("redirect_uri", "http://127.0.0.1:33418/callback"),
            ("state", "s"),
            ("code_challenge", "challenge"),
            ("code_challenge_method", "S256"),
        ])
        .header("cookie", format!("ak_session={}", env.session_token))
        .send()
        .await
        .expect("GET /oauth/authorize");
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["error"], "unsupported_response_type");
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
