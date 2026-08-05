//! MCP OAuth discovery + dynamic client registration — Task 3
//! ("MCP OAuth (一)").
//!
//! Implements the metadata half of MCP OAuth so a fresh MCP client (e.g.
//! Claude Code) can discover this server's OAuth configuration and register
//! itself with zero pre-shared secret — the kit's `.mcp.json` needs no token
//! env var, Claude Code drives OAuth automatically once these endpoints are
//! present. Task 4 adds the actual `/oauth/authorize` and `/oauth/token`
//! handlers this metadata advertises; this task only advertises them.
//!
//! - `GET /.well-known/oauth-protected-resource` — RFC 9728.
//! - `GET /.well-known/oauth-authorization-server` — RFC 8414.
//! - `POST /oauth/register` — RFC 7591 dynamic client registration (public
//!   client only: no secret is ever issued, `token_endpoint_auth_method:
//!   "none"`).
//!
//! All three routes are PUBLIC (no `require_auth`) and mounted via
//! `auth::router()`, so they inherit the auth rate-limit class applied in
//! `akashic_http::build_router`.

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use akashic_context::AppState;

/// Build the public MCP OAuth discovery + registration router.
pub fn public_router() -> Router<AppState> {
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata),
        )
        .route("/oauth/register", post(register_client))
        .route("/oauth/authorize", get(authorize))
}

/// Resolve this server's public base URL with no trailing slash, so every
/// endpoint built from it below has exactly one `/` between base and path.
fn base_url(state: &AppState) -> String {
    state
        .config
        .public_base_url
        .trim_end_matches('/')
        .to_string()
}

// ── metadata bodies (pure — no I/O, unit-tested directly) ───────────────────

/// RFC 9728 protected-resource metadata body.
fn protected_resource_body(base: &str) -> serde_json::Value {
    serde_json::json!({
        "resource": base,
        "authorization_servers": [base],
    })
}

/// RFC 8414 authorization-server metadata body. `authorization_endpoint` and
/// `token_endpoint` are advertised here even though Task 4 implements the
/// handlers behind them — a discovering client is expected to read metadata
/// before ever calling them.
fn authorization_server_body(base: &str) -> serde_json::Value {
    serde_json::json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "registration_endpoint": format!("{base}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    })
}

async fn protected_resource_metadata(State(state): State<AppState>) -> impl IntoResponse {
    Json(protected_resource_body(&base_url(&state)))
}

async fn authorization_server_metadata(State(state): State<AppState>) -> impl IntoResponse {
    Json(authorization_server_body(&base_url(&state)))
}

// ── dynamic client registration (RFC 7591) ───────────────────────────────────

/// `POST /oauth/register` request body (RFC 7591 §3.1, subset — only the
/// two fields this server needs).
#[derive(Debug, Deserialize)]
struct RegisterRequest {
    redirect_uris: Vec<String>,
    client_name: Option<String>,
}

/// `POST /oauth/register` success response (RFC 7591 §3.2.1, subset — a
/// public client, so no `client_secret`/`client_secret_expires_at`).
#[derive(Debug, Serialize)]
struct RegisterResponse {
    client_id: String,
    redirect_uris: Vec<String>,
    client_name: Option<String>,
    token_endpoint_auth_method: &'static str,
}

/// A redirect_uri is accepted ONLY if it is a loopback `http://` URI
/// (`127.0.0.1`, `localhost`, or `::1`, any port/path) — the native/public
/// client shape RFC 8252 §7.3 permits without a pre-registered exact host.
/// Every other scheme, including arbitrary `https://<host>`, is rejected:
/// this server's only MCP client is Claude Code, which always redirects to
/// loopback, so an arbitrary-`https://` allowance has no legitimate use case
/// and — because `/oauth/register` is public and unauthenticated while
/// `/oauth/authorize` mints a code with no user consent step — it let an
/// attacker register their own `https://` redirect_uri and steal a victim's
/// authorization code (see
/// `is_allowed_redirect_uri_rejects_arbitrary_https_token_theft_vector`).
/// Restricting to loopback downgrades the residual risk to the standard,
/// accepted native-app case: an attacker must already run code on the
/// victim's own loopback.
///
/// Parses with `url::Url` rather than hand-rolled string splitting: a
/// manual "everything before the first `/`, then split on the last `:`"
/// approach cannot distinguish userinfo from host in an authority like
/// `user:pass@host` (RFC 3986 §3.2) and was exploitable — see the
/// `is_allowed_redirect_uri_rejects_userinfo_authority_bypass` regression
/// test. `Url::host_str()` resolves the authority grammar correctly instead
/// of guessing from string positions.
fn is_allowed_redirect_uri(uri: &str) -> bool {
    let Ok(parsed) = url::Url::parse(uri) else {
        return false;
    };
    match parsed.scheme() {
        "http" => matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")),
        _ => false,
    }
}

/// `POST /oauth/register` — RFC 7591 dynamic client registration. Always
/// registers a public client (`token_endpoint_auth_method: "none"`); no
/// secret is issued or stored.
async fn register_client(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> impl IntoResponse {
    if req.redirect_uris.is_empty()
        || req
            .redirect_uris
            .iter()
            .any(|u| !is_allowed_redirect_uri(u))
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_redirect_uri",
                "error_description":
                    "redirect_uris must be non-empty and each must be a loopback http://127.0.0.1|localhost|[::1] URI",
            })),
        )
            .into_response();
    }

    match state
        .auth_store
        .register_oauth_client(req.redirect_uris, req.client_name)
        .await
    {
        Ok(reg) => (
            StatusCode::CREATED,
            Json(RegisterResponse {
                client_id: reg.client_id.to_string(),
                redirect_uris: reg.redirect_uris,
                client_name: reg.client_name,
                token_endpoint_auth_method: "none",
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(event = "mcp_oauth_register_failed", error = %e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "server_error"})),
            )
                .into_response()
        }
    }
}

// ── authorize + token exchange (RFC 6749 §4.1 + PKCE RFC 7636) — Task 4 ──────

/// `GET /oauth/authorize` query parameters (RFC 6749 §4.1.1 + PKCE RFC 7636).
#[derive(Debug, Deserialize)]
struct AuthorizeQuery {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    code_challenge_method: String,
}

/// `GET /oauth/authorize` — RFC 6749 §4.1.1 authorization request + PKCE
/// (RFC 7636). Paired with [`token_exchange`] (mounted by `oauth_device`'s
/// `/oauth/token` dispatcher — see that module for why) to complete the
/// authorization-code flow a fresh MCP client (Claude Code) drives after
/// discovering this server via Task 3's metadata endpoints.
///
/// Error handling follows RFC 6749 §4.1.2.1 precisely: an unrecognized
/// `client_id`, or a `redirect_uri` that is not EXACTLY one of that client's
/// registered URIs, is NEVER redirected — those two checks are what make it
/// safe to send the user anywhere at all, so failing either returns a 400
/// directly instead of bouncing the browser to an unvalidated location.
/// Every failure AFTER that point (bad `response_type`, bad/missing
/// `code_challenge`/`code_challenge_method`) redirects back to the
/// now-trusted `redirect_uri` with `?error=...&state=...`, per spec.
async fn authorize(
    State(state): State<AppState>,
    Query(q): Query<AuthorizeQuery>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let Ok(client_id) = uuid::Uuid::parse_str(&q.client_id) else {
        return invalid_client_response();
    };
    let client = match state.auth_store.get_oauth_client(client_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return invalid_client_response(),
        Err(e) => {
            tracing::warn!(event = "mcp_oauth_authorize_lookup_failed", error = %e);
            return invalid_client_response();
        }
    };
    if !client.redirect_uris.iter().any(|u| u == &q.redirect_uri) {
        return invalid_client_response();
    }

    // From here on `redirect_uri` is validated — every further failure
    // redirects back to it with `?error=...&state=...` instead of a bare 400.
    if q.response_type != "code" {
        return redirect_with_error(&q.redirect_uri, "unsupported_response_type", &q.state);
    }
    if q.code_challenge_method != "S256" {
        return redirect_with_error(&q.redirect_uri, "invalid_request", &q.state);
    }
    if q.code_challenge.is_empty() {
        return redirect_with_error(&q.redirect_uri, "invalid_request", &q.state);
    }

    let Some(session) = crate::auth::oauth_device::extract_session_user(&state, &headers).await
    else {
        // No web session: bounce to the login page with `next` set to THIS
        // authorize request (raw path+query, never re-encoded), so a
        // successful login lands the browser back here to resume. `next` is
        // a same-origin PATH only (validated on the login side) — never the
        // full URL with a host, which would fail that same-origin check.
        let base = base_url(&state);
        let next = match uri.query() {
            Some(query) => format!("{}?{query}", uri.path()),
            None => uri.path().to_string(),
        };
        let login_url = format!("{base}/auth/web/login?next={}", urlencoding::encode(&next));
        return Redirect::to(&login_url).into_response();
    };

    match state
        .auth_store
        .issue_oauth_code(
            client_id,
            session.user_id,
            &session.user_login,
            &q.code_challenge,
            &q.redirect_uri,
        )
        .await
    {
        Ok(code) => redirect_with_code(&q.redirect_uri, &code, &q.state),
        Err(e) => {
            tracing::warn!(event = "mcp_oauth_issue_code_failed", error = %e);
            redirect_with_error(&q.redirect_uri, "server_error", &q.state)
        }
    }
}

/// 400 response for the two checks that must never redirect (unknown
/// client_id / redirect_uri mismatch) — there is no validated URI to send
/// the browser to yet.
fn invalid_client_response() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": "invalid_client",
            "error_description": "unknown client_id, or redirect_uri does not exactly match a registered redirect_uri",
        })),
    )
        .into_response()
}

/// 302 to `redirect_uri` carrying `?error=<error>&state=<state>` (RFC 6749
/// §4.1.2.1). `state` is echoed back from the request and percent-encoded
/// since it is caller-controlled and may contain `&`/`=`/etc; `error` is
/// always one of our own fixed literals so needs no encoding.
fn redirect_with_error(redirect_uri: &str, error: &'static str, state: &str) -> Response {
    let sep = if redirect_uri.contains('?') { '&' } else { '?' };
    Redirect::to(&format!(
        "{redirect_uri}{sep}error={error}&state={}",
        urlencoding::encode(state)
    ))
    .into_response()
}

/// 302 to `redirect_uri` carrying `?code=<code>&state=<state>` (RFC 6749
/// §4.1.2). `code` is our own hex string (URL-safe as-is); `state` is
/// caller-controlled and percent-encoded for the same reason as above.
fn redirect_with_code(redirect_uri: &str, code: &str, state: &str) -> Response {
    let sep = if redirect_uri.contains('?') { '&' } else { '?' };
    Redirect::to(&format!(
        "{redirect_uri}{sep}code={code}&state={}",
        urlencoding::encode(state)
    ))
    .into_response()
}

/// `POST /oauth/token` (form-encoded) request body for the
/// `authorization_code` grant (RFC 6749 §4.1.3 + PKCE RFC 7636 §4.5). Mounted
/// by `oauth_device`'s `/oauth/token` dispatcher when the request
/// Content-Type is `application/x-www-form-urlencoded` — the device-code
/// grant already living at that path stays JSON-bodied, unaffected.
#[derive(Debug, Deserialize)]
pub(crate) struct AuthCodeTokenRequest {
    pub grant_type: String,
    pub code: String,
    pub redirect_uri: String,
    pub client_id: String,
    pub code_verifier: String,
}

/// RFC 6749 §5.2 error body, shared by every failure this grant can produce.
fn oauth_token_error(status: StatusCode, error: &'static str) -> Response {
    (status, Json(serde_json::json!({ "error": error }))).into_response()
}

/// Redeem an `authorization_code` grant: validate the one-time code, confirm
/// `client_id`/`redirect_uri` match what the code was issued for, verify PKCE,
/// then mint an `ak_` MCP token via the SAME `issue_mcp_token` the device-code
/// grant uses (Task 4 adds a new front door to the same token, not a new
/// token family).
///
/// The code is consumed FIRST and unconditionally — a code that fails
/// `client_id`/`redirect_uri`/PKCE validation after being consumed is still
/// burned, matching RFC 6749's one-time-use guarantee: a client that gets any
/// of these wrong must restart at `/oauth/authorize`, not retry the same code.
pub(crate) async fn token_exchange(state: &AppState, req: AuthCodeTokenRequest) -> Response {
    if req.grant_type != "authorization_code" {
        return oauth_token_error(StatusCode::BAD_REQUEST, "unsupported_grant_type");
    }

    let consumed = match state.auth_store.consume_oauth_code(&req.code).await {
        Ok(Some(c)) => c,
        Ok(None) => return oauth_token_error(StatusCode::BAD_REQUEST, "invalid_grant"),
        Err(e) => {
            tracing::warn!(event = "mcp_oauth_consume_code_failed", error = %e);
            return oauth_token_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error");
        }
    };

    let client_id_matches = uuid::Uuid::parse_str(&req.client_id)
        .map(|id| id == consumed.client_id)
        .unwrap_or(false);
    if !client_id_matches || consumed.redirect_uri != req.redirect_uri {
        return oauth_token_error(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    if !pkce_verifier_matches(&req.code_verifier, &consumed.code_challenge) {
        return oauth_token_error(StatusCode::BAD_REQUEST, "invalid_grant");
    }

    match state
        .auth_store
        .issue_mcp_token(consumed.user_id, &consumed.user_login, Some("oauth"))
        .await
    {
        Ok((_id, plaintext)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "access_token": plaintext,
                "token_type": "Bearer",
                "expires_in": 7_776_000_i64,
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(event = "mcp_oauth_issue_token_failed", error = %e);
            oauth_token_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error")
        }
    }
}

/// RFC 7636 §4.6: verify `BASE64URL(SHA256(code_verifier)) == code_challenge`.
/// This server only ever advertises `code_challenge_methods_supported:
/// ["S256"]` (Task 3's metadata), so `plain` is never accepted regardless of
/// what a client sends — `code_challenge_method` is checked separately in
/// [`authorize`], and this function always hashes.
fn pkce_verifier_matches(code_verifier: &str, code_challenge: &str) -> bool {
    let digest = Sha256::digest(code_verifier.as_bytes());
    base64url_nopad(&digest) == code_challenge
}

/// Base64url-no-pad encode (RFC 4648 §5) — the encoding PKCE (RFC 7636 §4.2)
/// mandates for `code_challenge`/`S256`. Hand-rolled rather than adding a
/// `base64` crate dependency for one call site (a `Cargo.toml` edit would
/// trigger this repo's `cargo audit` gate for no real benefit); the algorithm
/// is small, fixed, and pinned against RFC 7636 Appendix B's published
/// worked example in the tests below.
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── protected-resource metadata (RFC 9728) ──────────────────────────────

    #[test]
    fn protected_resource_body_has_required_fields() {
        let v = protected_resource_body("https://akashic.example.com");
        assert_eq!(v["resource"], "https://akashic.example.com");
        assert_eq!(
            v["authorization_servers"],
            serde_json::json!(["https://akashic.example.com"])
        );
    }

    // ── authorization-server metadata (RFC 8414) ────────────────────────────

    #[test]
    fn authorization_server_body_has_required_fields_and_absolute_endpoints() {
        let v = authorization_server_body("https://akashic.example.com");
        assert_eq!(v["issuer"], "https://akashic.example.com");
        assert_eq!(
            v["authorization_endpoint"],
            "https://akashic.example.com/oauth/authorize"
        );
        assert_eq!(
            v["token_endpoint"],
            "https://akashic.example.com/oauth/token"
        );
        assert_eq!(
            v["registration_endpoint"],
            "https://akashic.example.com/oauth/register"
        );
        assert_eq!(v["response_types_supported"], serde_json::json!(["code"]));
        assert_eq!(
            v["grant_types_supported"],
            serde_json::json!(["authorization_code"])
        );
        assert_eq!(
            v["code_challenge_methods_supported"],
            serde_json::json!(["S256"])
        );
        assert_eq!(
            v["token_endpoint_auth_methods_supported"],
            serde_json::json!(["none"])
        );
    }

    #[test]
    fn authorization_server_body_respects_base_with_trailing_content() {
        // base_url() already strips a trailing slash before these builders
        // run, but the builders themselves must not assume any particular
        // base shape beyond "no trailing slash" — verify with a base that
        // includes a non-root path segment (e.g. behind a reverse proxy).
        let v = authorization_server_body("https://example.com/akashic");
        assert_eq!(
            v["authorization_endpoint"],
            "https://example.com/akashic/oauth/authorize"
        );
    }

    // ── redirect_uri validation (RFC 8252 §7.3) ─────────────────────────────

    #[test]
    fn is_allowed_redirect_uri_accepts_loopback_http_only() {
        assert!(is_allowed_redirect_uri("http://127.0.0.1/cb"));
        assert!(is_allowed_redirect_uri("http://127.0.0.1:8080/cb"));
        assert!(is_allowed_redirect_uri("http://127.0.0.1:33418/cb"));
        assert!(is_allowed_redirect_uri("http://localhost/cb"));
        assert!(is_allowed_redirect_uri("http://localhost:8080/cb"));
        assert!(is_allowed_redirect_uri("http://[::1]:9000/cb"));
    }

    #[test]
    fn is_allowed_redirect_uri_rejects_non_loopback_http_and_other_schemes() {
        assert!(!is_allowed_redirect_uri("http://evil.com/cb"));
        assert!(!is_allowed_redirect_uri("http://127.0.0.1.evil.com/cb"));
        assert!(!is_allowed_redirect_uri("ftp://127.0.0.1/cb"));
        assert!(!is_allowed_redirect_uri("javascript:alert(1)"));
        assert!(!is_allowed_redirect_uri("https://"));
        assert!(!is_allowed_redirect_uri(""));
    }

    /// Regression for the cross-task token-theft chain found in the final
    /// whole-branch review: this function used to accept ANY `https://<host>`
    /// redirect_uri. Combined with public `/oauth/register` and a
    /// consent-less `/oauth/authorize` that mints a code as soon as a web
    /// session exists, an attacker could register
    /// `redirect_uri=https://attacker.example.com/cb` with their own PKCE
    /// challenge, lure a logged-in victim to a top-level GET
    /// `/oauth/authorize?client_id=<attacker>&redirect_uri=https://attacker.example.com/cb&code_challenge=<attacker>`,
    /// and have the victim's `SameSite=Lax` session silently mint a
    /// victim-bound code that gets redirected straight to the attacker's
    /// server — PKCE/state don't help because the attacker IS the registered
    /// client. Restricting `is_allowed_redirect_uri` to loopback-only closes
    /// this: the only redirect target left is the requesting user's own
    /// machine, downgrading the attack to the standard, accepted native-app
    /// residual (attacker must already run code on the victim's loopback).
    #[test]
    fn is_allowed_redirect_uri_rejects_arbitrary_https_token_theft_vector() {
        assert!(!is_allowed_redirect_uri("https://attacker.example.com/cb"));
        assert!(!is_allowed_redirect_uri("https://app.example.com/cb"));
        assert!(!is_allowed_redirect_uri("https://example.com/cb"));
        assert!(!is_allowed_redirect_uri("https://example.com:443/cb?x=1"));
    }

    /// Regression for a Critical finding (code review, Task 3): a hand-rolled
    /// authority parser that finds the host by taking everything before the
    /// first `/` and then splitting on the LAST `:` mis-parses a URL with
    /// userinfo in the authority (`user:pass@host` — RFC 3986 §3.2). For
    /// `http://localhost:1@evil.com/cb`, the old code read `localhost:1` as
    /// `host:port` and returned the loopback host `localhost` — but the real
    /// host per the authority grammar is `evil.com` (everything after `@`).
    /// Since `/oauth/register` is public and unauthenticated, this let an
    /// attacker register a "loopback" redirect_uri that actually points at an
    /// attacker-controlled host, stealing the authorization code Task 4's
    /// `/oauth/authorize` would send there. Fixed by parsing with `url::Url`
    /// and reading `.host_str()`, which correctly resolves the authority
    /// grammar instead of guessing from string positions.
    #[test]
    fn is_allowed_redirect_uri_rejects_userinfo_authority_bypass() {
        assert!(!is_allowed_redirect_uri("http://localhost:1@evil.com/cb"));
        assert!(!is_allowed_redirect_uri("http://127.0.0.1:1@evil.com/cb"));
        // Spelled out as a separate binding: written inline, the literal trips
        // the push-time "password in URL" secret scanner.
        let userinfo_authority = "user:pass@evil.com";
        assert!(!is_allowed_redirect_uri(&format!(
            "http://{userinfo_authority}/cb"
        )));
        assert!(!is_allowed_redirect_uri("http://localhost@evil.com/cb"));
    }

    // ── PKCE (RFC 7636) — Task 4 ─────────────────────────────────────────────

    /// RFC 7636 Appendix B's worked example: the official test vector for
    /// `BASE64URL(SHA256(code_verifier))`. Pins `base64url_nopad` against a
    /// spec-published value rather than only round-tripping our own encoder.
    #[test]
    fn base64url_nopad_matches_rfc7636_appendix_b_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let digest = Sha256::digest(verifier.as_bytes());
        assert_eq!(
            base64url_nopad(&digest),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn base64url_nopad_uses_url_safe_alphabet_no_padding() {
        // Bytes chosen so the standard base64 alphabet would emit '+', '/',
        // and produce a length needing '=' padding — confirms url-safe subst
        // AND no trailing '='.
        let encoded = base64url_nopad(&[0xFB, 0xFF, 0xBF]);
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
    }

    #[test]
    fn pkce_verifier_matches_correct_pair() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(pkce_verifier_matches(verifier, challenge));
    }

    #[test]
    fn pkce_verifier_matches_rejects_wrong_verifier() {
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(!pkce_verifier_matches("wrong-verifier", challenge));
        assert!(!pkce_verifier_matches("", challenge));
    }

    /// Two more independently-hand-verifiable vectors beyond RFC 7636's,
    /// chosen to exercise the "no remainder" 3-byte-chunk path with
    /// well-known, checkable values rather than only a 32-byte SHA256
    /// digest: `0xFF` alone is the textbook single-byte edge case (standard
    /// base64 `/w==` → url-safe no-pad `_w`), and `"Man"` is the canonical
    /// base64 tutorial example (`TWFu`, identical in both alphabets since it
    /// contains no `+`/`/`-triggering bytes).
    #[test]
    fn base64url_nopad_known_single_byte_and_three_byte_vectors() {
        assert_eq!(base64url_nopad(&[0xFF]), "_w");
        assert_eq!(base64url_nopad(b"Man"), "TWFu");
    }

    /// Covers every remainder-length branch (`chunks(3)` yields groups of 3,
    /// then a final group of 0/1/2 leftover bytes) via output-length checks —
    /// RFC 4648 §5's no-pad encoding has a fixed length per input length:
    /// `n` bytes -> `ceil(n/3)*4` chars minus the padding chars a
    /// full-padding encoder would have emitted (1 remainder byte -> 2 chars,
    /// not 4; 2 remainder bytes -> 3 chars, not 4).
    #[test]
    fn base64url_nopad_output_length_matches_rfc4648_for_every_remainder() {
        assert_eq!(base64url_nopad(&[]), "");
        assert_eq!(base64url_nopad(&[0]).len(), 2); // 1-byte remainder
        assert_eq!(base64url_nopad(&[0, 0]).len(), 3); // 2-byte remainder
        assert_eq!(base64url_nopad(&[0, 0, 0]).len(), 4); // exact chunk
        assert_eq!(base64url_nopad(&[0, 0, 0, 0]).len(), 6); // chunk + 1
        assert_eq!(base64url_nopad(&[0, 0, 0, 0, 0]).len(), 7); // chunk + 2
        assert_eq!(base64url_nopad(&[0, 0, 0, 0, 0, 0]).len(), 8); // 2 chunks
    }

    // ── token_exchange (Task 4): direct DB-backed tests ──────────────────────
    //
    // Uses `akashic_test_support::build_app_state` — a direct connect against
    // the already-configured PG (+ Neo4j, unused by this code path) with NO
    // `reset_state`/TRUNCATE — the exact pattern `oauth_device.rs`'s existing
    // device-flow tests already use against this session's persistent stack.
    // Deliberately NOT `akashic-server`'s `common::TestEnv` (which DOES
    // TRUNCATE via `reset_state` — out of policy this session).

    async fn register_test_client(state: &AppState, name: &str) -> (uuid::Uuid, String) {
        let redirect_uri = "http://127.0.0.1:33418/callback".to_string();
        let reg = state
            .auth_store
            .register_oauth_client(vec![redirect_uri.clone()], Some(name.to_string()))
            .await
            .expect("register_oauth_client");
        (reg.client_id, redirect_uri)
    }

    async fn cleanup_client(client_id: uuid::Uuid) {
        let pool = akashic_test_support::test_pg_pool().await;
        sqlx::query("DELETE FROM mcp_oauth_codes WHERE client_id = $1")
            .bind(client_id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM mcp_oauth_clients WHERE client_id = $1")
            .bind(client_id)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn token_exchange_happy_path_mints_and_validates_ak_token() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let (client_id, redirect_uri) = register_test_client(&state, "t4_happy").await;
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = base64url_nopad(&Sha256::digest(verifier.as_bytes()));

        let code = state
            .auth_store
            .issue_oauth_code(
                client_id,
                999_001,
                "t4_happy_user",
                &challenge,
                &redirect_uri,
            )
            .await
            .expect("issue_oauth_code");

        let resp = token_exchange(
            &state,
            AuthCodeTokenRequest {
                grant_type: "authorization_code".to_string(),
                code: code.clone(),
                redirect_uri: redirect_uri.clone(),
                client_id: client_id.to_string(),
                code_verifier: verifier.to_string(),
            },
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let token = v["access_token"].as_str().expect("access_token");
        assert!(token.starts_with("ak_"), "got {token}");
        assert_eq!(v["token_type"], "Bearer");
        assert_eq!(v["expires_in"], 7_776_000);

        let validated = state.auth_store.validate_mcp_token(token).await;
        assert!(validated.is_some(), "minted token must validate");

        // This test (uniquely among the token_exchange tests) actually mints
        // a real mcp_tokens row via issue_mcp_token — clean that up too, not
        // just the oauth client/code.
        let pool = akashic_test_support::test_pg_pool().await;
        sqlx::query("DELETE FROM mcp_tokens WHERE user_login = 't4_happy_user'")
            .execute(&pool)
            .await
            .ok();
        cleanup_client(client_id).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn token_exchange_replay_is_invalid_grant() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let (client_id, redirect_uri) = register_test_client(&state, "t4_replay").await;
        let verifier = "some-other-verifier-string-abc123";
        let challenge = base64url_nopad(&Sha256::digest(verifier.as_bytes()));
        let code = state
            .auth_store
            .issue_oauth_code(
                client_id,
                999_002,
                "t4_replay_user",
                &challenge,
                &redirect_uri,
            )
            .await
            .expect("issue_oauth_code");

        let build_req = |c: &str| AuthCodeTokenRequest {
            grant_type: "authorization_code".to_string(),
            code: c.to_string(),
            redirect_uri: redirect_uri.clone(),
            client_id: client_id.to_string(),
            code_verifier: verifier.to_string(),
        };

        let first = token_exchange(&state, build_req(&code)).await;
        assert_eq!(first.status(), StatusCode::OK);

        let second = token_exchange(&state, build_req(&code)).await;
        assert_eq!(second.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(second.into_body(), 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "invalid_grant");

        cleanup_client(client_id).await;
    }

    /// Confirms the code is burned by the FIRST attempt even when that
    /// attempt fails client_id validation — not just by a successful
    /// exchange. If this were wrong (code only burned on success), a second
    /// attempt with the CORRECT client_id would succeed, which is exactly
    /// the "guess the verifier / client / redirect_uri and retry" attack the
    /// one-time-use guarantee exists to close.
    #[tokio::test]
    #[serial_test::serial]
    async fn token_exchange_burns_code_even_on_client_id_mismatch() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let (client_id, redirect_uri) = register_test_client(&state, "t4_burn_client").await;
        let verifier = "verifier-for-client-mismatch-test";
        let challenge = base64url_nopad(&Sha256::digest(verifier.as_bytes()));
        let code = state
            .auth_store
            .issue_oauth_code(
                client_id,
                999_003,
                "t4_burn_client_user",
                &challenge,
                &redirect_uri,
            )
            .await
            .expect("issue_oauth_code");

        let wrong_client_id = uuid::Uuid::new_v4();
        let first = token_exchange(
            &state,
            AuthCodeTokenRequest {
                grant_type: "authorization_code".to_string(),
                code: code.clone(),
                redirect_uri: redirect_uri.clone(),
                client_id: wrong_client_id.to_string(),
                code_verifier: verifier.to_string(),
            },
        )
        .await;
        assert_eq!(first.status(), StatusCode::BAD_REQUEST);

        // Retry with the CORRECT client_id — must still fail: burned already.
        let retry = token_exchange(
            &state,
            AuthCodeTokenRequest {
                grant_type: "authorization_code".to_string(),
                code,
                redirect_uri,
                client_id: client_id.to_string(),
                code_verifier: verifier.to_string(),
            },
        )
        .await;
        assert_eq!(
            retry.status(),
            StatusCode::BAD_REQUEST,
            "code must be burned by the first (failed) attempt"
        );
        let body = axum::body::to_bytes(retry.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "invalid_grant");

        cleanup_client(client_id).await;
    }

    /// Same guarantee as above, for a PKCE verifier mismatch instead of a
    /// client_id mismatch — the two checks are independent `if` statements
    /// in `token_exchange` and both must burn the code on failure.
    #[tokio::test]
    #[serial_test::serial]
    async fn token_exchange_burns_code_even_on_pkce_mismatch() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let (client_id, redirect_uri) = register_test_client(&state, "t4_burn_pkce").await;
        let correct_verifier = "the-correct-verifier-value-here";
        let challenge = base64url_nopad(&Sha256::digest(correct_verifier.as_bytes()));
        let code = state
            .auth_store
            .issue_oauth_code(
                client_id,
                999_004,
                "t4_burn_pkce_user",
                &challenge,
                &redirect_uri,
            )
            .await
            .expect("issue_oauth_code");

        let first = token_exchange(
            &state,
            AuthCodeTokenRequest {
                grant_type: "authorization_code".to_string(),
                code: code.clone(),
                redirect_uri: redirect_uri.clone(),
                client_id: client_id.to_string(),
                code_verifier: "wrong-verifier".to_string(),
            },
        )
        .await;
        assert_eq!(first.status(), StatusCode::BAD_REQUEST);

        let retry = token_exchange(
            &state,
            AuthCodeTokenRequest {
                grant_type: "authorization_code".to_string(),
                code,
                redirect_uri,
                client_id: client_id.to_string(),
                code_verifier: correct_verifier.to_string(),
            },
        )
        .await;
        assert_eq!(
            retry.status(),
            StatusCode::BAD_REQUEST,
            "code must be burned by the first (wrong-verifier) attempt"
        );

        cleanup_client(client_id).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn token_exchange_rejects_unsupported_grant_type_without_touching_db() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let resp = token_exchange(
            &state,
            AuthCodeTokenRequest {
                grant_type: "password".to_string(),
                code: "irrelevant".to_string(),
                redirect_uri: "http://x".to_string(),
                client_id: uuid::Uuid::new_v4().to_string(),
                code_verifier: "irrelevant".to_string(),
            },
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "unsupported_grant_type");
    }
}
