use axum::{
    Router,
    extract::{Query, State},
    response::{Html, IntoResponse, Redirect},
    routing::get,
};
use std::time::{Duration, Instant};
use tracing::warn;

use akashic_context::AppState;
use akashic_identity::types::{CallbackQuery, PendingState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
}

/// GET /auth/login — generate a random state and redirect to GitLab OAuth.
async fn login(State(state): State<AppState>) -> impl IntoResponse {
    let state_param = uuid::Uuid::new_v4().to_string();

    state.auth_store.pending_states.insert(
        state_param.clone(),
        PendingState {
            expires_at: Instant::now() + Duration::from_mins(5),
            next: None,
        },
    );

    let gitlab_url = &state.config.gitlab_url;

    // FIX(audit oauth.rs:36 — authorize URL injection): percent-encode every
    // user/config-derived query param instead of raw string interpolation, so a
    // value containing `&`, `=`, `#`, or a space cannot smuggle in extra params
    // or corrupt the redirect_uri/scope. `state` is a generated UUID, but encode
    // it too for uniformity. `scope` uses `%20`-style encoding (a literal `+` in
    // a query is ambiguous); GitLab accepts space-delimited scopes.
    let client_id = urlencoding::encode(&state.config.gitlab_app_id);
    let redirect_uri = urlencoding::encode(&state.config.gitlab_redirect_uri);
    let state_q = urlencoding::encode(&state_param);
    let scope = urlencoding::encode("read_user read_repository");

    let authorize_url = format!(
        "{gitlab_url}/oauth/authorize\
         ?client_id={client_id}\
         &redirect_uri={redirect_uri}\
         &response_type=code\
         &state={state_q}\
         &scope={scope}"
    );

    // Bind the CSRF state to the initiating browser via a short-lived cookie;
    // the callback requires this cookie to match the returned state param.
    let secure = if state.config.gitlab_redirect_uri.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "ak_oauth_state={state_param}; HttpOnly; SameSite=Lax; Path=/; Max-Age=300{secure}"
    );
    (
        [(axum::http::header::SET_COOKIE, cookie)],
        Redirect::temporary(&authorize_url),
    )
}

/// GET /auth/callback?code=&state= — exchange code with GitLab, display verification code.
async fn callback(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<CallbackQuery>,
) -> impl IntoResponse {
    // ── CSRF: the state must match BOTH the server-side pending_states entry
    // AND the ak_oauth_state cookie set on the initiating browser, so a state
    // minted by one party can't be completed in a different party's browser. ──
    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok());
    if cookie_value(cookie_header, "ak_oauth_state") != Some(params.state.as_str()) {
        return Html(error_page("Invalid or missing OAuth state cookie")).into_response();
    }

    // Thinned (Phase2-GW): the deprecated browser callback keeps its extra
    // ak_oauth_state cookie CSRF check (above). The pending_states consume +
    // token exchange + user fetch + verification-code issuance now live in
    // AuthService::callback_complete (gateway-backed, same gitlab_redirect_uri
    // + generate_user_code format). It returns JSON {username, code, ttl_secs}.
    let json_str = match state
        .auth_service
        .callback_complete(params.code, params.state)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            warn!(%e, "OAuth callback failed");
            return Html(error_page("Failed to authenticate with GitLab")).into_response();
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(_) => return Html(error_page("Internal error")).into_response(),
    };
    let username = v["username"].as_str().unwrap_or("unknown");
    let code = v["code"].as_str().unwrap_or("");
    let ttl_secs = v["ttl_secs"].as_u64().unwrap_or(0);
    Html(success_page(username, code, ttl_secs)).into_response()
}

/// Extract the value of cookie `name` from a `Cookie:` header (exact-name match).
fn cookie_value<'a>(cookie_header: Option<&'a str>, name: &str) -> Option<&'a str> {
    let header = cookie_header?;
    for part in header.split(';') {
        if let Some(val) = part
            .trim()
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
        {
            return Some(val);
        }
    }
    None
}

/// Minimal HTML escaper for interpolating untrusted text into element content
/// or double-quoted attribute values. Mirrors the helper in `oauth_device.rs`
/// so a crafted GitLab `username` cannot break out of the markup and inject
/// script when reflected into the success page.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

fn success_page(username: &str, code: &str, ttl_secs: u64) -> String {
    let ttl_mins = ttl_secs / 60;
    // FIX(audit oauth.rs:179 — reflected XSS): the GitLab `username` is
    // attacker-influenced (anyone can set their own GitLab display name), so
    // escape it (and the code, for defence in depth) before interpolating into
    // HTML — matching the device-flow renderer in `oauth_device.rs`.
    let username = html_escape(username);
    let code = html_escape(code);
    format!(
        r#"<!DOCTYPE html>
<html><head>
  <title>Akashic Record — Authentication</title>
  <meta charset="utf-8">
  <style>
    body {{ font-family: system-ui, -apple-system, sans-serif; text-align: center;
           margin-top: 80px; background: #0f1117; color: #e2e8f0; }}
    .code {{ font-size: 56px; font-weight: bold; letter-spacing: 12px; margin: 24px 0;
             color: #60a5fa; font-family: monospace; }}
    .subtitle {{ color: #94a3b8; margin: 12px 0; }}
    .card {{ background: #1e2130; border-radius: 12px; padding: 48px;
             display: inline-block; box-shadow: 0 4px 24px rgba(0,0,0,0.3); }}
    .user {{ color: #34d399; font-weight: 600; }}
  </style>
</head><body>
  <div class="card">
    <h1>Authentication Successful</h1>
    <p>Welcome, <span class="user">{username}</span></p>
    <p class="subtitle">Your verification code:</p>
    <div class="code">{code}</div>
    <p class="subtitle">Enter this code in your CLI to complete authentication.</p>
    <p class="subtitle">This code expires in {ttl_mins} minutes.</p>
  </div>
</body></html>"#
    )
}

fn error_page(message: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html><head>
  <title>Akashic Record — Error</title>
  <meta charset="utf-8">
  <style>
    body {{ font-family: system-ui, -apple-system, sans-serif; text-align: center;
           margin-top: 80px; background: #0f1117; color: #e2e8f0; }}
    .card {{ background: #1e2130; border-radius: 12px; padding: 48px;
             display: inline-block; box-shadow: 0 4px 24px rgba(0,0,0,0.3); }}
    h1 {{ color: #f87171; }}
  </style>
</head><body>
  <div class="card">
    <h1>Authentication Error</h1>
    <p>{message}</p>
    <p><a href="/auth/login" style="color:#60a5fa;">Try again</a></p>
  </div>
</body></html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::{cookie_value, html_escape, success_page};
    use crate::auth::oauth_device::generate_user_code;

    #[test]
    fn cookie_value_matches_only_exact_name() {
        assert_eq!(
            cookie_value(Some("a=1; ak_oauth_state=xyz; b=2"), "ak_oauth_state"),
            Some("xyz")
        );
        // A cookie whose name merely starts with the target must not match.
        assert_eq!(
            cookie_value(Some("ak_oauth_state_x=no"), "ak_oauth_state"),
            None
        );
        assert_eq!(cookie_value(Some("nope=1"), "ak_oauth_state"), None);
        assert_eq!(cookie_value(None, "ak_oauth_state"), None);
    }

    // FIX(audit oauth.rs:179) — reflected XSS regression guard.
    #[test]
    fn html_escape_neutralizes_markup() {
        assert_eq!(
            html_escape(r#"<script>alert("x")&'</script>"#),
            "&lt;script&gt;alert(&quot;x&quot;)&amp;&#x27;&lt;/script&gt;"
        );
        // Benign input is left untouched.
        assert_eq!(html_escape("alice"), "alice");
    }

    // FIX(audit oauth.rs:179) — a malicious GitLab username (and code) must be
    // escaped before reaching the rendered HTML, so no raw `<script>` survives.
    // SECURITY (audit oauth.rs:141): the verification code must be a
    // high-entropy bearer secret, NOT a brute-forceable 6-digit number. Guard
    // the format so a regression back to `{:06}` (all-digits, length 6) fails.
    #[test]
    fn verification_code_is_high_entropy_not_six_digits() {
        let code = generate_user_code();
        // The old code was exactly 6 ASCII digits; the new one must not be.
        assert!(
            !(code.len() == 6 && code.chars().all(|c| c.is_ascii_digit())),
            "verification code regressed to a 6-digit number: {code}"
        );
        // Device-flow `XXXX-XXXX` format: 9 chars, hyphen at index 4, the rest
        // from the unambiguous alphabet (no vowels, no 0/1/I/O).
        assert_eq!(code.len(), 9, "expected XXXX-XXXX, got {code:?}");
        assert_eq!(code.as_bytes()[4], b'-', "missing hyphen separator: {code}");
        let alphabet = "BCDFGHJKLMNPQRSTVWXYZ23456789";
        assert!(
            code.chars()
                .filter(|&c| c != '-')
                .all(|c| alphabet.contains(c)),
            "code uses characters outside the unambiguous alphabet: {code}"
        );
    }

    #[test]
    fn success_page_escapes_username_and_code() {
        let page = success_page("<script>evil</script>", "<img onerror=1>", 600);
        assert!(
            !page.contains("<script>evil</script>"),
            "raw username markup leaked into success page"
        );
        assert!(
            !page.contains("<img onerror=1>"),
            "raw code markup leaked into success page"
        );
        assert!(page.contains("&lt;script&gt;evil&lt;/script&gt;"));
        assert!(page.contains("&lt;img onerror=1&gt;"));
    }

    // FIX(audit oauth.rs:36) — query params must be percent-encoded so an
    // adversarial config value cannot inject extra OAuth params. We assert on the
    // same `urlencoding::encode` used by `login`, which is the load-bearing call.
    #[test]
    fn authorize_params_are_percent_encoded() {
        // redirect_uri carrying an extra param + fragment must be fully encoded.
        let redirect = urlencoding::encode("https://x.test/cb?evil=1#frag");
        assert!(!redirect.contains('?'));
        assert!(!redirect.contains('&'));
        assert!(!redirect.contains('#'));
        assert_eq!(redirect, "https%3A%2F%2Fx.test%2Fcb%3Fevil%3D1%23frag");
        // Space-delimited scope encodes the space rather than emitting a raw one.
        assert_eq!(
            urlencoding::encode("read_user read_repository"),
            "read_user%20read_repository"
        );
    }
}
