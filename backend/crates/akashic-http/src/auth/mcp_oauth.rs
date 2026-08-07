//! MCP OAuth discovery + authorize/consent + token exchange.
//!
//! Task 3 shipped the metadata + dynamic client registration (RFC 7591)
//! half; Task 4 added `/oauth/authorize` (no consent step) + `/oauth/token`.
//! Task 5 (spec §4, MCP refactor 2026-08-07) replaces dynamic client
//! registration with **Client ID Metadata Documents (CIMD, SEP-991)**: the
//! `client_id` a client presents IS an `https://` URL, fetched and validated
//! by `crate::auth::cimd::CimdFetcher` (Task 4's module — wired in here for
//! the first time). `/oauth/authorize` no longer mints a code directly; it
//! renders a consent screen (`GET`) that `POST /oauth/authorize/consent`
//! redeems on approval, atomically and bound to the requesting user's
//! session (CSRF defense — see `OauthConsentRepo`'s domain doc comment).
//!
//! - `GET /.well-known/oauth-protected-resource` — RFC 9728.
//! - `GET /.well-known/oauth-authorization-server` — RFC 8414 (now
//!   advertises `client_id_metadata_document_supported: true` instead of a
//!   `registration_endpoint` — there is no more dynamic-client-registration
//!   endpoint).
//! - `GET /oauth/authorize` — RFC 6749 §4.1.1 + CIMD validation + PKCE
//!   (RFC 7636); renders the consent screen.
//! - `POST /oauth/authorize/consent` — redeems the consent, mints the
//!   authorization code on approval.
//! - `POST /oauth/token` — the paired `authorization_code` grant (mounted by
//!   `oauth_device`'s dispatcher — see that module for why).
//!
//! All routes are PUBLIC (no `require_auth`) and mounted via `auth::router()`,
//! so they inherit the auth rate-limit class applied in
//! `akashic_http::build_router`. `/oauth/authorize` and
//! `/oauth/authorize/consent` additionally require a WEB SESSION (cookie) —
//! checked inside the handler, not via the `require_auth` middleware, since
//! an unauthenticated `GET /oauth/authorize` must redirect to login rather
//! than 401.

use axum::{
    Json, Router,
    extract::{Form, Query, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use akashic_context::AppState;
use akashic_domain::types::PendingConsentInput;

/// Build the public MCP OAuth discovery + authorize/consent router.
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
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/authorize/consent", post(consent))
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

/// RFC 8414 authorization-server metadata body. `client_id_metadata_document_supported`
/// (SEP-991) tells a discovering client it may present any `https://` URL as
/// its `client_id` directly — there is no `registration_endpoint` to call
/// first (Task 5 removed the dynamic-client-registration endpoint; CIMD
/// replaces DCR).
fn authorization_server_body(base: &str) -> serde_json::Value {
    serde_json::json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "client_id_metadata_document_supported": true,
    })
}

async fn protected_resource_metadata(State(state): State<AppState>) -> impl IntoResponse {
    Json(protected_resource_body(&base_url(&state)))
}

async fn authorization_server_metadata(State(state): State<AppState>) -> impl IntoResponse {
    Json(authorization_server_body(&base_url(&state)))
}

// ── CIMD fetcher holding (Task 4's module, wired in here — Task 5) ──────────

/// Process-lifetime `CimdFetcher`, lazily constructed from the FIRST
/// `AppState` it sees. `CimdFetcher` cannot live on `AppState` itself:
/// `AppState` is defined in `akashic-context`, and `cimd` lives in
/// `akashic-http` — `akashic-context` does not (and should not) depend on
/// `akashic-http`, so holding it there would create a reverse dependency
/// (cycle). A module-static `OnceLock` gives the same "one instance, reused
/// across requests" behavior without that cycle.
///
/// Safety of the "first `AppState` wins" init: every `AppState` constructed
/// within one process shares the same `Config` (or, in tests, a
/// process-consistent test config — see `akashic-server/tests/common/mod.rs`'s
/// `build_test_config`), so which specific `AppState` happens to win the
/// race is immaterial — `mcp_cimd_allow_loopback` is identical either way.
/// Unit/store tests below never go through this `OnceLock` — they either
/// test pure functions directly or construct a `CimdFetcher` themselves
/// (see `cimd.rs`'s own `#[cfg(test)]`).
static CIMD: std::sync::OnceLock<crate::auth::cimd::CimdFetcher> = std::sync::OnceLock::new();

fn cimd(state: &AppState) -> &'static crate::auth::cimd::CimdFetcher {
    CIMD.get_or_init(|| crate::auth::cimd::CimdFetcher::new(state.config.mcp_cimd_allow_loopback))
}

// ── authorize + consent + token exchange (RFC 6749 §4.1 + PKCE RFC 7636 + CIMD SEP-991) ──

/// `GET /oauth/authorize` query parameters (RFC 6749 §4.1.1 + PKCE RFC 7636).
/// `client_id` is a CIMD `https://` URL (spec §4), not an opaque identifier.
#[derive(Debug, Deserialize)]
struct AuthorizeQuery {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    code_challenge_method: String,
}

/// `GET /oauth/authorize` — RFC 6749 §4.1.1 authorization request + CIMD
/// client validation (SEP-991) + PKCE (RFC 7636). On success this renders a
/// consent screen instead of minting a code directly — [`consent`] (the
/// paired `POST` handler below) does that, on approval.
///
/// Ordered early-return chain (fix-round-1, controller ruling — see the
/// review this addressed): **structural 400 → session 303 → CIMD 400 →
/// `redirect_uri` 400 → consent page**.
///
/// 1. `response_type`/`code_challenge_method`/`code_challenge` — 400,
///    never a redirect (no trustworthy `redirect_uri` exists yet). Cheapest
///    checks, no I/O, so they run first regardless of what else changes.
/// 2. **Web session** — checked BEFORE the CIMD fetch. This is the load-
///    bearing reorder: [`cimd`]'s `fetch_and_validate` makes a real outbound
///    HTTP request (SSRF-guarded, but still a distinguishing oracle —
///    reachable vs. blocked vs. malformed-document vs. wrong-`client_id`
///    all read differently). Fetching before authenticating would let ANY
///    anonymous caller drive that oracle against arbitrary `https://` URLs
///    of their choosing. Requiring a session first means only an
///    already-authenticated user can ever trigger a fetch. No session →
///    303 to the login page (unchanged mechanics from Task 4).
/// 3. **CIMD fetch/validate** — any `CimdError` collapses to the SAME fixed,
///    non-leaking `error_description` (see [`invalid_client_response`]) —
///    the specific error is logged via `tracing::warn!` for operators only,
///    never echoed to the caller. This is the other half of the same fix:
///    even an authenticated caller must not be able to distinguish *why* an
///    arbitrary URL's CIMD fetch failed.
/// 4. **`redirect_uri` match** — must be EXACTLY one of the CIMD document's
///    own `redirect_uris`, else 400 (still pre-trust: this specific URI
///    isn't validated yet even though the document is).
/// 5. Past this point `redirect_uri` is fully CIMD-trusted — the pending
///    consent is created and the consent screen rendered; a `server_error`
///    persisting it falls back to `redirect_with_error`, since redirecting
///    is safe now.
async fn authorize(
    State(state): State<AppState>,
    Query(q): Query<AuthorizeQuery>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    // 1. Structural checks — BEFORE `redirect_uri` is trusted, so 400, never
    // a redirect (spec §4 failure-response principle).
    if q.response_type != "code" {
        return oauth_authorize_400(
            "unsupported_response_type",
            "response_type must be \"code\"",
        );
    }
    if q.code_challenge_method != "S256" {
        return oauth_authorize_400("invalid_request", "code_challenge_method must be \"S256\"");
    }
    if q.code_challenge.is_empty() {
        return oauth_authorize_400("invalid_request", "code_challenge is required");
    }

    // 2. Web session — BEFORE the CIMD fetch (see fn-doc point 2: closes the
    // unauthenticated-fetch-oracle finding). No session: bounce to the login
    // page with `next` set to THIS authorize request (raw path+query, never
    // re-encoded), so a successful login lands the browser back here to
    // resume. `next` is a same-origin PATH only (validated on the login
    // side) — never the full URL with a host, which would fail that
    // same-origin check.
    let Some(session) = crate::auth::oauth_device::extract_session_user(&state, &headers).await
    else {
        let base = base_url(&state);
        let next = match uri.query() {
            Some(query) => format!("{}?{query}", uri.path()),
            None => uri.path().to_string(),
        };
        let login_url = format!("{base}/auth/web/login?next={}", urlencoding::encode(&next));
        return Redirect::to(&login_url).into_response();
    };

    // 3. CIMD (SEP-991): `client_id` IS the document URL — fetch, validate,
    // and cache it (`crate::auth::cimd::CimdFetcher`, Task 4). Every failure
    // shape collapses to the same fixed `invalid_client_response()` message
    // (see that fn's doc comment for why); the specific `CimdError` is
    // logged here, operator-only.
    let doc = match cimd(&state).fetch_and_validate(&q.client_id).await {
        Ok(doc) => doc,
        Err(e) => {
            tracing::warn!(event = "mcp_oauth_cimd_validation_failed", error = %e);
            return invalid_client_response(
                "client_id metadata document could not be fetched or validated",
            );
        }
    };
    // 4. `redirect_uri` must exactly match one of the CIMD document's own
    // declared URIs — still pre-trust (this specific URI isn't validated
    // yet even though the document itself is), so 400, never a redirect.
    if !doc.redirect_uris.iter().any(|u| u == &q.redirect_uri) {
        return invalid_client_response(
            "redirect_uri is not one of the client's registered redirect_uris",
        );
    }

    // 5. From here on `redirect_uri` is CIMD-validated — every further
    // failure is safe to redirect to it with `?error=...&state=...`.
    let meta = PendingConsentInput {
        client_id: q.client_id.clone(),
        client_name: doc.client_name.clone(),
        redirect_uri: q.redirect_uri.clone(),
        oauth_state: q.state.clone(),
        code_challenge: q.code_challenge.clone(),
    };
    match state
        .auth_store
        .issue_pending_consent(session.user_id, &session.user_login, &meta)
        .await
    {
        Ok(consent_id) => {
            Html(consent_page_html(consent_id, &doc, &q.redirect_uri)).into_response()
        }
        Err(e) => {
            tracing::warn!(event = "mcp_oauth_issue_pending_consent_failed", error = %e);
            redirect_with_error(&q.redirect_uri, "server_error", &q.state)
        }
    }
}

/// 400 response for the CIMD/`redirect_uri` validation that must never
/// redirect — there is no CIMD-trusted URI to send the browser to yet.
///
/// `description` is `&'static str` BY CONSTRUCTION, never a caller-derived
/// `Display` (fix-round-1): the underlying `CimdError` can contain raw
/// fetch/DNS/HTTP-status detail, and echoing it into the response body would
/// hand a caller — even an authenticated one, now that the session check
/// runs before the CIMD fetch (see [`authorize`]) — a distinguishing oracle
/// over this server's outbound CIMD fetch for any `https://` URL of their
/// choosing (reachable vs. SSRF-blocked vs. malformed-document vs. ... all
/// read differently). The specific error is logged via `tracing::warn!` at
/// the call site instead, for operators only.
fn invalid_client_response(description: &'static str) -> Response {
    oauth_authorize_400("invalid_client", description)
}

/// 400 RFC 6749 §5.2-shaped error body shared by every `/oauth/authorize`
/// and `/oauth/authorize/consent` failure that must not redirect.
/// `description` is `&'static str`, not a caller-derived `Display` — every
/// body on this path is one of our own fixed literals, never echoed-back
/// input or a wrapped internal error (see [`invalid_client_response`]'s doc
/// comment for the finding this closes).
fn oauth_authorize_400(error: &'static str, description: &'static str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": error,
            "error_description": description,
        })),
    )
        .into_response()
}

/// Render the CIMD-validated client's confirmation page. `consent_id` is
/// embedded as a hidden form field the browser round-trips to
/// `POST /oauth/authorize/consent` — it is the ONLY thing the form submits
/// besides the `approve`/`deny` decision, so the actual client/redirect/PKCE
/// metadata is never re-derived from (attacker-controllable) form input,
/// only read from the server-side pending-consent row `consent_id` points
/// at.
///
/// All three untrusted strings interpolated below (`client_name`,
/// `client_id`, `redirect_uri` — each is either CIMD-document content or a
/// caller-supplied query param) are HTML-escaped first — see
/// `consent_page_html_escapes_client_name_and_redirect_uri` for the XSS
/// regression this guards.
fn consent_page_html(
    consent_id: uuid::Uuid,
    doc: &crate::auth::cimd::ClientMetadata,
    redirect_uri: &str,
) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let name = esc(doc.client_name.as_deref().unwrap_or("(unnamed client)"));
    let cid = esc(&doc.client_id);
    let ruri = esc(redirect_uri);
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Authorize MCP client — Akashic Record</title></head>
<body style="font-family:system-ui;max-width:32rem;margin:4rem auto;padding:0 1rem">
<h1>Authorize MCP client?</h1>
<p><strong>{name}</strong></p>
<p>Client ID: <code>{cid}</code></p>
<p>Redirects to: <code>{ruri}</code></p>
<p>This grants the client a 90-day MCP access token for your account.</p>
<form method="post" action="/oauth/authorize/consent">
<input type="hidden" name="consent_id" value="{consent_id}">
<button type="submit" name="decision" value="approve">Approve</button>
<button type="submit" name="decision" value="deny">Deny</button>
</form></body></html>"#
    )
}

/// `POST /oauth/authorize/consent` form body.
#[derive(Debug, Deserialize)]
struct ConsentForm {
    consent_id: String,
    decision: String,
}

/// `POST /oauth/authorize/consent` — redeems a pending consent created by
/// [`authorize`] and, on approval, mints the authorization code.
///
/// CSRF defense (see `OauthConsentRepo`'s domain doc comment for the full
/// rationale): redeeming a consent requires the SAME session `user_id` that
/// created the row, so a forged cross-site POST — even one carrying a valid,
/// unexpired `consent_id` — fails unless it also rides the victim's own
/// session cookie, at which point it's no longer distinguishable from the
/// user's own action (the standard limit of any session-bound CSRF defense).
/// `consent_id` itself is an unguessable `UUID` and single-use (atomic CAS
/// in the repo layer), closing the two other legs of the CSRF triangle
/// (guessing / replaying).
async fn consent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ConsentForm>,
) -> Response {
    let Some(session) = crate::auth::oauth_device::extract_session_user(&state, &headers).await
    else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response();
    };

    // A malformed consent_id is folded into the same "unknown" 400 as a
    // well-formed-but-missing one below — no oracle distinguishing
    // "malformed" from "unknown/expired/used/not-yours".
    let Ok(consent_id) = uuid::Uuid::parse_str(&form.consent_id) else {
        return oauth_authorize_400("invalid_request", "unknown consent_id");
    };

    let row = match state
        .auth_store
        .redeem_pending_consent(consent_id, session.user_id)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return oauth_authorize_400(
                "invalid_request",
                "consent_id is unknown, expired, already used, or does not belong to this session",
            );
        }
        Err(e) => {
            tracing::warn!(event = "mcp_oauth_redeem_pending_consent_failed", error = %e);
            // A repo failure is a server fault, not a client error — 500,
            // matching `token_exchange`'s existing `server_error` convention
            // (same body shape: `oauth_token_error`, no `error_description`
            // wrapping an internal error into the client-facing message).
            return oauth_token_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error");
        }
    };

    if form.decision != "approve" {
        return redirect_with_error(&row.redirect_uri, "access_denied", &row.oauth_state);
    }

    match state
        .auth_store
        .issue_oauth_code(
            &row.client_id,
            row.user_id,
            &row.user_login,
            &row.code_challenge,
            &row.redirect_uri,
        )
        .await
    {
        Ok(code) => redirect_with_code(&row.redirect_uri, &code, &row.oauth_state),
        Err(e) => {
            tracing::warn!(event = "mcp_oauth_issue_code_failed", error = %e);
            redirect_with_error(&row.redirect_uri, "server_error", &row.oauth_state)
        }
    }
}

/// 303 to `redirect_uri` carrying `?error=<error>&state=<state>` (RFC 6749
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

/// 303 to `redirect_uri` carrying `?code=<code>&state=<state>` (RFC 6749
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
///
/// `client_id` is a CIMD URL (spec §4) — compared with plain string equality
/// (`!=`), byte-for-byte (Task 4's shape was a UUID parse-then-compare;
/// there is no UUID anymore, so there is nothing left to parse).
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

    if consumed.client_id != req.client_id || consumed.redirect_uri != req.redirect_uri {
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
        // Task 5 (spec §4): CIMD replaces DCR.
        assert_eq!(v["client_id_metadata_document_supported"], true);
        assert!(
            v.get("registration_endpoint").is_none(),
            "registration_endpoint must be gone — there is no more dynamic-client-registration endpoint"
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

    // ── consent_page_html (Task 5) ───────────────────────────────────────────

    fn sample_doc(client_name: Option<&str>) -> crate::auth::cimd::ClientMetadata {
        crate::auth::cimd::ClientMetadata {
            client_id: "https://client.example/metadata.json".to_string(),
            redirect_uris: vec!["http://127.0.0.1:33418/callback".to_string()],
            client_name: client_name.map(str::to_string),
            logo_uri: None,
        }
    }

    /// XSS regression: `client_name` is CIMD-document content — served by
    /// whatever host the untrusted `client_id` URL names — so it MUST be
    /// HTML-escaped before landing in the consent page. Also covers
    /// `redirect_uri`, which is caller-supplied via the query string.
    #[test]
    fn consent_page_html_escapes_client_name_and_redirect_uri() {
        let doc = crate::auth::cimd::ClientMetadata {
            client_id: "https://client.example/metadata.json".to_string(),
            redirect_uris: vec!["http://127.0.0.1:33418/callback".to_string()],
            client_name: Some("<script>alert(1)</script>".to_string()),
            logo_uri: None,
        };
        let html = consent_page_html(
            uuid::Uuid::nil(),
            &doc,
            "http://127.0.0.1:33418/callback?x=<img src=x onerror=alert(2)>",
        );
        assert!(
            !html.contains("<script>") && !html.contains("<img"),
            "unescaped markup leaked into consent page HTML: {html}"
        );
        assert!(
            html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "escaped client_name should be present: {html}"
        );
        assert!(
            html.contains("&lt;img src=x onerror=alert(2)&gt;"),
            "escaped redirect_uri should be present: {html}"
        );
    }

    #[test]
    fn consent_page_html_falls_back_to_unnamed_client_when_name_absent() {
        let doc = sample_doc(None);
        let html = consent_page_html(uuid::Uuid::nil(), &doc, "http://127.0.0.1:33418/callback");
        assert!(html.contains("(unnamed client)"));
    }

    #[test]
    fn consent_page_html_embeds_consent_id_in_hidden_field() {
        let doc = sample_doc(Some("Test Client"));
        let id = uuid::Uuid::new_v4();
        let html = consent_page_html(id, &doc, "http://127.0.0.1:33418/callback");
        assert!(
            html.contains(&format!(r#"name="consent_id" value="{id}""#)),
            "hidden consent_id field missing or wrong value: {html}"
        );
        assert!(html.contains(r#"action="/oauth/authorize/consent""#));
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

    // ── token_exchange (Task 4, client_id shape updated Task 5): direct
    // DB-backed tests ────────────────────────────────────────────────────────
    //
    // Uses `akashic_test_support::build_app_state` — a direct connect against
    // the already-configured PG (+ Neo4j, unused by this code path) with NO
    // `reset_state`/TRUNCATE — the exact pattern `oauth_device.rs`'s existing
    // device-flow tests already use against this session's persistent stack.
    // Deliberately NOT `akashic-server`'s `common::TestEnv` (which DOES
    // TRUNCATE via `reset_state` — out of policy this session).
    //
    // `client_id` is now a CIMD URL string (spec §4), not a DB-registered
    // UUID — these tests exercise `token_exchange` directly (never
    // `authorize`), so no CIMD document ever needs to be fetched over HTTP;
    // the string just needs to be URL-shaped and unique per test to avoid
    // `mcp_oauth_codes` collisions.

    fn test_client(name: &str) -> (String, String) {
        (
            format!("https://client.example/{name}.json"),
            "http://127.0.0.1:33418/callback".to_string(),
        )
    }

    async fn cleanup_client_codes(client_id: &str) {
        let pool = akashic_test_support::test_pg_pool().await;
        sqlx::query("DELETE FROM mcp_oauth_codes WHERE client_id = $1")
            .bind(client_id)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn token_exchange_happy_path_mints_and_validates_ak_token() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let (client_id, redirect_uri) = test_client("t5_happy");
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = base64url_nopad(&Sha256::digest(verifier.as_bytes()));

        let code = state
            .auth_store
            .issue_oauth_code(
                &client_id,
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
                client_id: client_id.clone(),
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
        // just the oauth code.
        let pool = akashic_test_support::test_pg_pool().await;
        sqlx::query("DELETE FROM mcp_tokens WHERE user_login = 't4_happy_user'")
            .execute(&pool)
            .await
            .ok();
        cleanup_client_codes(&client_id).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn token_exchange_replay_is_invalid_grant() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let (client_id, redirect_uri) = test_client("t5_replay");
        let verifier = "some-other-verifier-string-abc123";
        let challenge = base64url_nopad(&Sha256::digest(verifier.as_bytes()));
        let code = state
            .auth_store
            .issue_oauth_code(
                &client_id,
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
            client_id: client_id.clone(),
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

        cleanup_client_codes(&client_id).await;
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
        let (client_id, redirect_uri) = test_client("t5_burn_client");
        let verifier = "verifier-for-client-mismatch-test";
        let challenge = base64url_nopad(&Sha256::digest(verifier.as_bytes()));
        let code = state
            .auth_store
            .issue_oauth_code(
                &client_id,
                999_003,
                "t4_burn_client_user",
                &challenge,
                &redirect_uri,
            )
            .await
            .expect("issue_oauth_code");

        let wrong_client_id = "https://attacker.example/mismatch.json".to_string();
        let first = token_exchange(
            &state,
            AuthCodeTokenRequest {
                grant_type: "authorization_code".to_string(),
                code: code.clone(),
                redirect_uri: redirect_uri.clone(),
                client_id: wrong_client_id,
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
                client_id: client_id.clone(),
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

        cleanup_client_codes(&client_id).await;
    }

    /// Same guarantee as above, for a PKCE verifier mismatch instead of a
    /// client_id mismatch — the two checks are independent `if` statements
    /// in `token_exchange` and both must burn the code on failure.
    #[tokio::test]
    #[serial_test::serial]
    async fn token_exchange_burns_code_even_on_pkce_mismatch() {
        let state = akashic_test_support::build_app_state("http://unused".into()).await;
        let (client_id, redirect_uri) = test_client("t5_burn_pkce");
        let correct_verifier = "the-correct-verifier-value-here";
        let challenge = base64url_nopad(&Sha256::digest(correct_verifier.as_bytes()));
        let code = state
            .auth_store
            .issue_oauth_code(
                &client_id,
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
                client_id: client_id.clone(),
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
                client_id: client_id.clone(),
                code_verifier: correct_verifier.to_string(),
            },
        )
        .await;
        assert_eq!(
            retry.status(),
            StatusCode::BAD_REQUEST,
            "code must be burned by the first (wrong-verifier) attempt"
        );

        cleanup_client_codes(&client_id).await;
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
                client_id: "https://client.example/irrelevant.json".to_string(),
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
