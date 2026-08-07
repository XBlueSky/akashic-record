//! Client ID Metadata Documents (SEP-991, MCP 2026-07-28) — spec §4.
//!
//! Classic OAuth requires clients to pre-register with the authorization
//! server. CIMD replaces that with a convention: the OAuth `client_id`
//! itself IS an `https://` URL, and that URL, fetched, returns a small JSON
//! document (`client_name`, `redirect_uris`, `logo_uri`, ...) describing the
//! client. This module is that fetch: given a `client_id` URL, retrieve,
//! validate, and cache the document it points to. Task 5 wires the result
//! into `/oauth/authorize`; this module has no handler/`AppState` coupling
//! of its own.
//!
//! ## Threat model: fetching an attacker-controlled URL is SSRF
//!
//! The `client_id` is presented by an untrusted caller (anyone hitting
//! `/oauth/authorize`) and this module makes an outbound HTTP request to
//! whatever host it names. Absent guards, that's a textbook SSRF primitive:
//! an attacker sets `client_id=https://internal-admin.local/...` (or an IP
//! literal, or a hostname that resolves to one) and uses this server as a
//! proxy into internal-only endpoints. Defenses here:
//!
//! - **Scheme pinned to `https`** (never `http`, except the explicit
//!   dev/test `allow_loopback` escape hatch — see `mcp_cimd_allow_loopback`
//!   in `akashic-config`). No fragment allowed (SEP-991 requires the
//!   document's `client_id` field to equal the fetch URL byte-for-byte
//!   modulo trailing slash; a fragment is client-side-only and can't
//!   round-trip through that comparison, so its presence is rejected
//!   up front rather than silently stripped).
//! - **No redirects followed** (`redirect::Policy::none()`). A same-origin
//!   allowlisted URL could otherwise 302 to an internal target after the
//!   guard below has already passed.
//! - **SSRF guard on the resolved IP(s)**, not just the URL string: literal
//!   IPv4/IPv6 loopback, private (RFC1918/ULA), link-local, and unspecified
//!   addresses are rejected, and hostnames are resolved via DNS and checked
//!   the same way. IPv4-mapped IPv6 literals (`::ffff:127.0.0.1`) are
//!   unwrapped to their embedded IPv4 address before the IPv4 checks run —
//!   `Ipv6Addr::is_loopback`/`is_unspecified` do NOT recognize this form (it
//!   only matches literal `::1`/`::`), so skipping the unwrap would let
//!   `https://[::ffff:127.0.0.1]/x` sail past the guard while resolving to
//!   the exact same loopback socket `127.0.0.1` does. This is the one
//!   documented hardening beyond the brief's guard sketch.
//! - **Response size capped** at `MAX_DOCUMENT_BYTES`, enforced BEFORE
//!   buffering: `Content-Length` is checked first as a fast-path rejection
//!   (a malicious/misconfigured server can lie about or omit it — chunked
//!   responses have none at all), and the body is then read via
//!   `bytes_stream()` with a running total checked after every chunk,
//!   bailing with `CimdError::TooLarge` the instant the cap is crossed.
//!   This is the actual enforcement point: a naive `resp.bytes().await`
//!   buffers the ENTIRE body up front regardless of the cap and only
//!   checks the length afterward, so a chunked/no-`Content-Length`
//!   response would stream unbounded into memory until `FETCH_TIMEOUT`
//!   fires instead of being rejected early.
//! - **5s fetch timeout** so a slow/hanging internal target can't tie up a
//!   request thread indefinitely.
//!
//! ### Accepted residual: TOCTOU on hostname resolution
//!
//! For hostname (non-IP-literal) `client_id` URLs, the SSRF guard resolves
//! DNS once to check the target, then `reqwest` resolves DNS again
//! (independently) when it actually connects. A DNS-rebinding attacker who
//! controls the authoritative nameserver could answer the guard's lookup
//! with a public IP and the connection's lookup with a private one,
//! bypassing the guard entirely. This race is accepted rather than closed
//! (e.g. by resolving once and connecting to the pinned IP with a `Host`
//! header) because the blast radius at this call site is bounded: the
//! fetch carries no credentials, cookies, or authorization state (it's a
//! GET with only an `Accept` header), the response is parsed as inert JSON
//! metadata (never executed, never proxied back to the caller verbatim),
//! and the worst case is this server making one GET request to an internal
//! host — which the operator's own network segmentation should already
//! treat as the actual security boundary. Revisit if a future caller starts
//! attaching credentials to this client or relaying the response body.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures::StreamExt;

/// Wall-clock budget for the outbound fetch. Short enough that a hung
/// internal target (see TOCTOU note above) can't pin a request thread.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on the client-metadata document body. CIMDs are a handful of
/// short string/array fields; 64KiB is generous headroom, not a real-world
/// document size.
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
/// How long a successfully-validated document is served from cache before
/// the next `fetch_and_validate` call re-fetches it.
const CACHE_TTL: Duration = Duration::from_mins(5);

/// The document a `client_id` URL is expected to serve, per SEP-991 §4.
///
/// `#[serde(deny_unknown_fields)]` is deliberately NOT set — the spec allows
/// additional fields, and this struct only needs the subset the authorize
/// flow (Task 5) consumes.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ClientMetadata {
    pub client_id: String,
    pub redirect_uris: Vec<String>,
    pub client_name: Option<String>,
    pub logo_uri: Option<String>,
}

/// Every way `fetch_and_validate` can fail. Deliberately typed (not
/// `anyhow`) so the `/oauth/authorize` handler (Task 5) can pattern-match
/// and always respond with a 400 rather than an OAuth redirect — redirecting
/// on a validation failure would let an attacker bounce a browser through
/// this server toward a URL of their choosing.
#[derive(Debug, thiserror::Error)]
pub enum CimdError {
    #[error("client_id must be an https URL without a fragment")]
    InvalidUrl,
    #[error("client_id host resolves to a private or loopback address")]
    SsrfBlocked,
    #[error("failed to fetch client metadata: {0}")]
    FetchFailed(String),
    #[error("client metadata document exceeds {MAX_DOCUMENT_BYTES} bytes")]
    TooLarge,
    #[error("client metadata document is not valid: {0}")]
    InvalidDocument(String),
    #[error("document client_id does not equal the document URL")]
    ClientIdMismatch,
}

/// Fetches, validates, and caches Client ID Metadata Documents.
///
/// One instance is meant to live for the process lifetime (constructed once
/// into `AppState` by Task 5), so the in-memory cache is actually useful.
pub struct CimdFetcher {
    client: reqwest::Client,
    allow_loopback: bool,
    cache: Mutex<HashMap<String, (Instant, ClientMetadata)>>,
}

impl CimdFetcher {
    /// `allow_loopback` should be wired from
    /// `Config::mcp_cimd_allow_loopback` — true only in dev/test, never in
    /// production (see that field's doc comment in `akashic-config`).
    pub fn new(allow_loopback: bool) -> Self {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none()) // SEP-991: never follow
            .build()
            .expect("reqwest client");
        Self {
            client,
            allow_loopback,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Fetch and validate the Client ID Metadata Document at `client_id_url`.
    ///
    /// On success, the document is cached for `CACHE_TTL` keyed on the exact
    /// input string (no normalization — repeated calls with the identical
    /// `client_id` string hit the cache; differently-formatted-but-equivalent
    /// URLs are treated as distinct cache entries, which only costs an extra
    /// fetch, never a correctness problem).
    pub async fn fetch_and_validate(
        &self,
        client_id_url: &str,
    ) -> Result<ClientMetadata, CimdError> {
        let parsed = url::Url::parse(client_id_url).map_err(|_| CimdError::InvalidUrl)?;
        let https_ok =
            parsed.scheme() == "https" || (self.allow_loopback && parsed.scheme() == "http");
        if !https_ok || parsed.fragment().is_some() {
            return Err(CimdError::InvalidUrl);
        }
        let host = parsed.host_str().ok_or(CimdError::InvalidUrl)?.to_string();
        self.ssrf_guard(&host).await?;

        // Look up and drop the lock before doing anything else — holding a
        // `std::sync::Mutex` guard across the `.await`s below would be a
        // bug (blocking lock across yield points), and even within this
        // synchronous span, scoping the guard to its own block keeps clippy
        // (`significant_drop_in_scrutinee`, workspace pedantic) quiet and
        // makes the "lock held only for this lookup" intent explicit.
        let cached = {
            let cache = self.cache.lock().unwrap();
            cache.get(client_id_url).and_then(|(at, doc)| {
                if at.elapsed() < CACHE_TTL {
                    Some(doc.clone())
                } else {
                    None
                }
            })
        };
        if let Some(doc) = cached {
            return Ok(doc);
        }

        let resp = self
            .client
            .get(parsed.clone())
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| CimdError::FetchFailed(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(CimdError::FetchFailed(format!("status {}", resp.status())));
        }
        if resp
            .content_length()
            .is_some_and(|l| l > MAX_DOCUMENT_BYTES as u64)
        {
            return Err(CimdError::TooLarge);
        }
        let bytes = Self::read_capped_body(resp).await?;
        let doc: ClientMetadata = serde_json::from_slice(&bytes)
            .map_err(|e| CimdError::InvalidDocument(e.to_string()))?;
        if doc.client_id.trim_end_matches('/') != client_id_url.trim_end_matches('/') {
            return Err(CimdError::ClientIdMismatch);
        }
        if doc.redirect_uris.is_empty() {
            return Err(CimdError::InvalidDocument("redirect_uris is empty".into()));
        }

        self.cache
            .lock()
            .unwrap()
            .insert(client_id_url.to_string(), (Instant::now(), doc.clone()));
        Ok(doc)
    }

    /// Read `resp`'s body up to `MAX_DOCUMENT_BYTES`, bailing with
    /// `CimdError::TooLarge` the instant the running total crosses the cap
    /// — never after. This is the fix for the case the `Content-Length`
    /// fast-path check in `fetch_and_validate` can't catch: a chunked (or
    /// otherwise no-`Content-Length`) response has nothing for that check
    /// to inspect, so without this, a plain `resp.bytes().await` call would
    /// buffer the ENTIRE body — regardless of size — before its own
    /// length check ever ran, streaming unbounded attacker-controlled data
    /// into memory until `FETCH_TIMEOUT` cuts it off. Streaming via
    /// `bytes_stream()` (reqwest's `stream` feature, already enabled on
    /// the workspace `reqwest` dependency) and checking after each chunk
    /// bounds the worst case to `MAX_DOCUMENT_BYTES` plus at most one
    /// chunk.
    async fn read_capped_body(resp: reqwest::Response) -> Result<bytes::Bytes, CimdError> {
        let mut stream = resp.bytes_stream();
        let mut buf = bytes::BytesMut::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| CimdError::FetchFailed(e.to_string()))?;
            buf.extend_from_slice(&chunk);
            if buf.len() > MAX_DOCUMENT_BYTES {
                return Err(CimdError::TooLarge);
            }
        }
        Ok(buf.freeze())
    }

    /// Reject hosts that are, or resolve to, private/loopback/link-local
    /// addresses (SSRF guard). `allow_loopback` (dev/test only, see
    /// `Config::mcp_cimd_allow_loopback`) skips the check entirely.
    ///
    /// TOCTOU note: for hostnames, `reqwest` independently re-resolves DNS
    /// on the actual fetch; the module doc comment above explains why that
    /// race is accepted rather than closed at this call site.
    async fn ssrf_guard(&self, host: &str) -> Result<(), CimdError> {
        if self.allow_loopback {
            return Ok(());
        }
        let ips: Vec<IpAddr> =
            if let Ok(ip) = host.trim_start_matches('[').trim_end_matches(']').parse() {
                vec![ip]
            } else {
                tokio::net::lookup_host((host, 443))
                    .await
                    .map_err(|e| CimdError::FetchFailed(format!("dns: {e}")))?
                    .map(|sa| sa.ip())
                    .collect()
            };
        let blocked = ips.iter().any(|ip| match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
            }
            IpAddr::V6(v6) => {
                // IPv4-mapped IPv6 (`::ffff:a.b.c.d`, RFC 4291 §2.5.5.2):
                // unwrap to the embedded IPv4 address and re-run the IPv4
                // checks. `Ipv6Addr::is_loopback`/`is_unspecified` do NOT
                // treat `::ffff:127.0.0.1` as loopback (only literal `::1`
                // matches), so without this branch a `client_id` host of
                // `[::ffff:127.0.0.1]` would sail straight past the guard
                // while actually dialing the loopback interface — see the
                // module doc comment's threat-model section.
                if let Some(v4) = v6.to_ipv4_mapped() {
                    v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
                } else {
                    v6.is_loopback()
                        || v6.is_unspecified()
                        || (v6.segments()[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
                        || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                }
            }
        });
        if blocked || ips.is_empty() {
            Err(CimdError::SsrfBlocked)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Fixture server state: serves one fixed response body/status and
    /// counts how many times it was hit (used to assert cache behavior).
    #[derive(Clone)]
    struct Fixture {
        hits: Arc<AtomicUsize>,
        status: StatusCode,
        body: Arc<String>,
    }

    async fn fixture_handler(State(fx): State<Fixture>) -> impl IntoResponse {
        fx.hits.fetch_add(1, Ordering::SeqCst);
        (fx.status, (*fx.body).clone())
    }

    /// Bind a loopback listener first (so its port is known), build the
    /// response body via `body_fn(&url)` (most fixtures need to embed their
    /// own URL as `client_id` in the body), then start serving `/cimd`.
    /// Returns the fixture's URL and its hit counter.
    ///
    /// The server task is intentionally not joined/aborted — it exits with
    /// the test process, and each `#[tokio::test]` gets its own runtime.
    async fn spawn_fixture(
        status: StatusCode,
        body_fn: impl FnOnce(&str) -> String,
    ) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture listener");
        let addr = listener.local_addr().expect("local_addr");
        let url = format!("http://{addr}/cimd");
        let body = body_fn(&url);
        let hits = Arc::new(AtomicUsize::new(0));
        let fx = Fixture {
            hits: hits.clone(),
            status,
            body: Arc::new(body),
        };
        let app = axum::Router::new()
            .route("/cimd", get(fixture_handler))
            .with_state(fx);
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("fixture server");
        });
        (url, hits)
    }

    fn valid_body(client_id_url: &str) -> String {
        format!(
            r#"{{"client_id":"{client_id_url}","redirect_uris":["https://client.example.com/callback"],"client_name":"Test Client","logo_uri":null}}"#
        )
    }

    // ── (1) valid document succeeds; second call hits the cache ─────────
    #[tokio::test]
    async fn valid_document_succeeds_and_second_call_hits_cache() {
        let (url, hits) = spawn_fixture(StatusCode::OK, valid_body).await;
        let fetcher = CimdFetcher::new(true);

        let first = fetcher.fetch_and_validate(&url).await.unwrap();
        assert_eq!(first.client_id, url);
        assert_eq!(
            first.redirect_uris,
            vec!["https://client.example.com/callback".to_string()]
        );
        assert_eq!(first.client_name.as_deref(), Some("Test Client"));
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        let second = fetcher.fetch_and_validate(&url).await.unwrap();
        assert_eq!(second.client_id, first.client_id);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "second call must be served from cache, not hit the fixture again"
        );
    }

    // ── (2) client_id in the document != the fetch URL ──────────────────
    #[tokio::test]
    async fn client_id_mismatch_is_rejected() {
        let (url, _hits) = spawn_fixture(StatusCode::OK, |_url| {
            r#"{"client_id":"https://mismatched.example.com/other","redirect_uris":["https://client.example.com/callback"],"client_name":null,"logo_uri":null}"#.to_string()
        })
        .await;
        let fetcher = CimdFetcher::new(true);
        let err = fetcher.fetch_and_validate(&url).await.unwrap_err();
        assert!(matches!(err, CimdError::ClientIdMismatch), "got {err:?}");
    }

    // ── (3) non-JSON body ─────────────────────────────────────────────
    #[tokio::test]
    async fn non_json_document_is_rejected() {
        let (url, _hits) =
            spawn_fixture(StatusCode::OK, |_url| "not json at all".to_string()).await;
        let fetcher = CimdFetcher::new(true);
        let err = fetcher.fetch_and_validate(&url).await.unwrap_err();
        assert!(matches!(err, CimdError::InvalidDocument(_)), "got {err:?}");
    }

    // ── (4) empty redirect_uris ──────────────────────────────────────
    #[tokio::test]
    async fn empty_redirect_uris_is_rejected() {
        let (url, _hits) = spawn_fixture(StatusCode::OK, |url| {
            format!(
                r#"{{"client_id":"{url}","redirect_uris":[],"client_name":null,"logo_uri":null}}"#
            )
        })
        .await;
        let fetcher = CimdFetcher::new(true);
        let err = fetcher.fetch_and_validate(&url).await.unwrap_err();
        assert!(matches!(err, CimdError::InvalidDocument(_)), "got {err:?}");
    }

    // ── (5) oversize document (fixture returns 70KiB) ────────────────
    #[tokio::test]
    async fn oversize_document_is_rejected() {
        let (url, _hits) = spawn_fixture(StatusCode::OK, |_url| "a".repeat(70 * 1024)).await;
        let fetcher = CimdFetcher::new(true);
        let err = fetcher.fetch_and_validate(&url).await.unwrap_err();
        assert!(matches!(err, CimdError::TooLarge), "got {err:?}");
    }

    // ── (5b) oversize document, no Content-Length (chunked) ───────────
    // Regression for the streaming-cap fix: unlike (5) above — whose
    // fixture returns a plain `String` body, which axum gives a known
    // `Content-Length` up front, so the fast-path check in
    // `fetch_and_validate` alone would already catch it — this fixture
    // streams its body via `axum::body::Body::from_stream`, which axum/
    // hyper cannot precompute a length for, so the response has NO
    // `Content-Length` header and falls back to chunked transfer
    // encoding. Before the fix, `resp.bytes().await` would buffer this
    // entire (unbounded) body before ever checking its length; the fix
    // (`CimdFetcher::read_capped_body`) must catch it mid-stream instead.
    // 12 chunks of 8KiB = 96KiB, comfortably over `MAX_DOCUMENT_BYTES`
    // (64KiB).
    #[tokio::test]
    async fn chunked_oversize_document_without_content_length_is_rejected() {
        async fn streaming_oversize_handler() -> axum::body::Body {
            let chunks = (0..12).map(|_| {
                Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from("a".repeat(8 * 1024)))
            });
            axum::body::Body::from_stream(futures::stream::iter(chunks))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture listener");
        let addr = listener.local_addr().expect("local_addr");
        let url = format!("http://{addr}/cimd");
        let app = axum::Router::new().route("/cimd", get(streaming_oversize_handler));
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("fixture server");
        });

        let fetcher = CimdFetcher::new(true);
        let err = fetcher.fetch_and_validate(&url).await.unwrap_err();
        assert!(matches!(err, CimdError::TooLarge), "got {err:?}");
    }

    // ── (6) allow_loopback: false rejects loopback client_id URLs ────
    #[tokio::test]
    async fn allow_loopback_false_rejects_http_scheme_as_invalid_url() {
        let fetcher = CimdFetcher::new(false);
        let err = fetcher
            .fetch_and_validate("http://127.0.0.1/cimd")
            .await
            .unwrap_err();
        assert!(matches!(err, CimdError::InvalidUrl), "got {err:?}");
    }

    #[tokio::test]
    async fn allow_loopback_false_rejects_https_loopback_as_ssrf_blocked() {
        let fetcher = CimdFetcher::new(false);
        let err = fetcher
            .fetch_and_validate("https://127.0.0.1/cimd")
            .await
            .unwrap_err();
        assert!(matches!(err, CimdError::SsrfBlocked), "got {err:?}");
    }

    // ── (7) fragment in the client_id URL ─────────────────────────────
    #[tokio::test]
    async fn fragment_url_is_rejected_as_invalid_url() {
        let fetcher = CimdFetcher::new(true);
        let err = fetcher
            .fetch_and_validate("https://example.com/client#fragment")
            .await
            .unwrap_err();
        assert!(matches!(err, CimdError::InvalidUrl), "got {err:?}");
    }

    // ── Bonus (beyond the brief's 7): IPv4-mapped IPv6 SSRF-guard hardening
    // ───────────────────────────────────────────────────────────────────
    // Regression-pins the hardening documented in the module header and at
    // the `to_ipv4_mapped()` call site: without unwrapping the mapped
    // address first, `Ipv6Addr::is_loopback()` does not recognize
    // `::ffff:127.0.0.1` as loopback, and this host would incorrectly pass
    // the guard. Calls the private `ssrf_guard` directly (same module) so
    // the assertion isn't dependent on how `url`/DNS happen to format an
    // IPv4-mapped literal inside a full URL.
    #[tokio::test]
    async fn ssrf_guard_rejects_ipv4_mapped_ipv6_loopback() {
        let fetcher = CimdFetcher::new(false);
        let err = fetcher.ssrf_guard("::ffff:127.0.0.1").await.unwrap_err();
        assert!(matches!(err, CimdError::SsrfBlocked), "got {err:?}");
    }
}
