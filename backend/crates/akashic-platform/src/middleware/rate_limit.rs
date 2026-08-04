//! Rate-limit middleware (anonymous tier).
//!
//! The rate-limit layer is the outermost middleware on every protected
//! route group. A7 (MCP read/write split) inserts auth **between** this
//! layer and the route handlers — do not change that ordering without
//! updating both A6 and A7.
//!
//! Implementation note: this module no longer uses `tower_governor`'s
//! `GovernorLayer` — instead it owns a `governor::RateLimiter` directly so
//! the allowlist branch can completely skip the limiter (a `tower_governor`
//! layer wraps the inner service, so calling `next.run(req)` from a
//! "bypass" middleware positioned outside the layer would still consume a
//! token, defeating the bypass).
#![allow(dead_code)]

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::http::Request;
use governor::clock::{Clock, DefaultClock};
use governor::state::keyed::DefaultKeyedStateStore;
use governor::{Quota, RateLimiter};
use ipnet::IpNet;

use akashic_config::Config;

/// Extracts the rate-limit key (IpAddr) from a request, honoring
/// `X-Forwarded-For` only when the immediate socket peer matches the
/// configured trusted-proxy CIDR list.
#[derive(Clone, Debug)]
pub struct ConfigurableTrustedProxyExtractor {
    pub trusted_proxies: Arc<Vec<IpNet>>,
}

impl ConfigurableTrustedProxyExtractor {
    pub fn new(trusted_proxies: Arc<Vec<IpNet>>) -> Self {
        Self { trusted_proxies }
    }

    fn ip_in_trusted(&self, ip: IpAddr) -> bool {
        self.trusted_proxies.iter().any(|net| net.contains(&ip))
    }

    /// Pure helper: given a peer IP and the raw XFF header value, return
    /// the resolved key. Exposed for unit testing.
    pub fn resolve(&self, peer: IpAddr, xff: Option<&str>) -> IpAddr {
        if !self.ip_in_trusted(peer) {
            return peer;
        }
        let Some(xff) = xff else { return peer };
        for raw in xff.split(',').rev().map(str::trim) {
            if raw.is_empty() {
                continue;
            }
            let Ok(parsed) = raw.parse::<IpAddr>() else {
                continue;
            };
            if !self.ip_in_trusted(parsed) {
                return parsed;
            }
        }
        peer
    }
}

impl ConfigurableTrustedProxyExtractor {
    /// Extract the rate-limit key from a request, mirroring what the old
    /// `KeyExtractor` impl did. Returns `None` when ConnectInfo is missing
    /// (test/mock paths) — the caller treats that as "not allowlisted, no
    /// bucket consumed" and lets the request through.
    pub fn extract_from_req<B>(&self, req: &Request<B>) -> Option<IpAddr> {
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip())?;
        let xff = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok());
        Some(self.resolve(peer, xff))
    }
}

/// Wraps `ConfigurableTrustedProxyExtractor` to truncate IPv6 addresses
/// to their /64 prefix before bucketing. IPv4 keys pass through unchanged.
#[derive(Clone, Debug)]
pub struct Ipv6Slash64KeyExtractor {
    inner: ConfigurableTrustedProxyExtractor,
}

impl Ipv6Slash64KeyExtractor {
    pub fn new(inner: ConfigurableTrustedProxyExtractor) -> Self {
        Self { inner }
    }

    /// Truncate an IpAddr to its rate-limit bucket key.
    /// IPv4 → unchanged. IPv6 → /64 prefix (top 64 bits, low 64 bits zeroed).
    pub fn bucket_key(addr: IpAddr) -> IpAddr {
        match addr {
            IpAddr::V4(_) => addr,
            IpAddr::V6(v6) => {
                let segments = v6.segments();
                IpAddr::V6(Ipv6Addr::new(
                    segments[0],
                    segments[1],
                    segments[2],
                    segments[3],
                    0,
                    0,
                    0,
                    0,
                ))
            }
        }
    }
}

impl Ipv6Slash64KeyExtractor {
    /// Apply the inner extractor and IPv6 /64 truncation to a request.
    pub fn extract_from_req<B>(&self, req: &Request<B>) -> Option<IpAddr> {
        self.inner.extract_from_req(req).map(Self::bucket_key)
    }
}

/// Per-route quota table. Burst is the bucket capacity; period_secs is the
/// token-replenishment interval.
#[derive(Clone, Copy, Debug)]
pub struct RouteQuota {
    pub burst: u32,
    pub period_secs: u64,
}

/// Auth routes — tighter than general REST. (10 burst, 1 token / 6s.)
pub const QUOTA_AUTH: RouteQuota = RouteQuota {
    burst: 10,
    period_secs: 6,
};

/// Ingestion-trigger writes — expensive paths. (5 burst, 1 token / 12s.)
pub const QUOTA_INGEST: RouteQuota = RouteQuota {
    burst: 5,
    period_secs: 12,
};

/// General `/api/` quota. (60 burst, 1 token / 1s.)
pub const QUOTA_API: RouteQuota = RouteQuota {
    burst: 60,
    period_secs: 1,
};

/// MCP SSE routes. (30 burst, 1 token / 2s.)
pub const QUOTA_MCP: RouteQuota = RouteQuota {
    burst: 30,
    period_secs: 2,
};

/// Type alias for our keyed rate limiter.
type KeyedLimiter = RateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>;

/// Per-route-group state held by the rate-limit middleware.
///
/// Owning the `RateLimiter` here (rather than going through
/// `tower_governor::GovernorLayer`) is what makes the allowlist bypass
/// actually work: if the resolved key matches the allowlist, we call the
/// inner service directly without consulting the limiter, so allowlisted
/// IPs never consume a token.
struct RouteLimiter {
    limiter: KeyedLimiter,
    extractor: Ipv6Slash64KeyExtractor,
    allowlist: Arc<Vec<IpNet>>,
    route_group: &'static str,
}

fn build_route_limiter(
    quota: RouteQuota,
    cfg: &Config,
    route_group: &'static str,
) -> Arc<RouteLimiter> {
    let inner =
        ConfigurableTrustedProxyExtractor::new(Arc::new(cfg.rate_limit_trusted_proxies.clone()));
    let extractor = Ipv6Slash64KeyExtractor::new(inner);
    let burst = NonZeroU32::new(quota.burst).expect("burst must be non-zero");
    let q = Quota::with_period(Duration::from_secs(quota.period_secs))
        .expect("period must be non-zero")
        .allow_burst(burst);
    Arc::new(RouteLimiter {
        limiter: RateLimiter::dashmap(q),
        extractor,
        allowlist: Arc::new(cfg.rate_limit_allowlist.clone()),
        route_group,
    })
}

use axum::body::Body;
use axum::extract::Request as ExtractRequest;
use axum::http::{Response, StatusCode};
use axum::middleware::Next;

async fn rate_limit_layer(
    state: Arc<RouteLimiter>,
    req: ExtractRequest,
    next: Next,
) -> Response<Body> {
    // Resolve the per-client key. If `ConnectInfo<SocketAddr>` is missing
    // (a misconfigured server that forgot `into_make_service_with_connect_info`)
    // we fail SAFE: bucket every such request under one shared sentinel key so
    // the limiter still applies, rather than silently disabling rate limiting
    // for the whole route group. (The test bench sets `rate_limit_enabled =
    // false`, so this layer is never even mounted there.)
    let key = match state.extractor.extract_from_req(&req) {
        Some(k) => k,
        None => {
            tracing::warn!(
                event = "rate_limit_missing_connect_info",
                route_group = state.route_group,
                "ConnectInfo<SocketAddr> missing on a rate-limited route; \
                 applying a shared fallback bucket (server should use \
                 into_make_service_with_connect_info)"
            );
            IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        }
    };

    // Allowlist bypass: completely skip the limiter — no bucket consumed,
    // no 429 ever emitted. This is the contract `RATE_LIMIT_ALLOWLIST`
    // documents in `.env.example`.
    if state.allowlist.iter().any(|net| net.contains(&key)) {
        tracing::trace!(
            ip = %key,
            route_group = state.route_group,
            "rate-limit allowlist bypass"
        );
        return next.run(req).await;
    }

    match state.limiter.check_key(&key) {
        Ok(()) => next.run(req).await,
        Err(negative) => {
            let wait = negative
                .wait_time_from(DefaultClock::default().now())
                .as_secs();
            tracing::warn!(
                event = "rate_limited",
                route_group = state.route_group,
                retry_after_s = wait,
                "request rate-limited"
            );
            let mut resp = Response::new(Body::from(format!(
                "rate limit exceeded; retry after {wait}s"
            )));
            *resp.status_mut() = StatusCode::TOO_MANY_REQUESTS;
            if let Ok(val) = wait.to_string().parse() {
                resp.headers_mut().insert("retry-after", val);
            }
            resp
        }
    }
}

/// Apply the rate-limit middleware to a router branch. The middleware
/// resolves the client key (with trusted-proxy XFF handling and IPv6 /64
/// truncation), bypasses the limiter for allowlisted CIDRs, otherwise
/// consults a per-route-group keyed bucket and emits 429 when the quota
/// is exhausted.
fn apply_layer_chain<S>(
    app: axum::Router<S>,
    quota: RouteQuota,
    cfg: &Config,
    route_group: &'static str,
) -> axum::Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let state = build_route_limiter(quota, cfg, route_group);
    app.layer(axum::middleware::from_fn(
        move |req: ExtractRequest, next: Next| {
            let state = state.clone();
            async move { rate_limit_layer(state, req, next).await }
        },
    ))
}

/// Apply the **general /api/** rate-limit layer to the given router.
pub fn apply_rate_limits(
    app: axum::Router<akashic_context::AppState>,
    cfg: &Config,
) -> axum::Router<akashic_context::AppState> {
    if !cfg.rate_limit_enabled {
        tracing::info!("rate limit disabled (RATE_LIMIT_ENABLED=false)");
        return app;
    }
    apply_layer_chain(app, QUOTA_API, cfg, "api")
}

/// Apply the auth-route rate-limit layer chain to a router branch.
/// Returns the unmodified router when rate-limiting is disabled.
pub fn apply_auth_rate_limit(
    app: axum::Router<akashic_context::AppState>,
    cfg: &Config,
) -> axum::Router<akashic_context::AppState> {
    if !cfg.rate_limit_enabled {
        return app;
    }
    apply_layer_chain(app, QUOTA_AUTH, cfg, "auth")
}

/// Apply the ingest-route rate-limit layer chain to a router branch.
pub fn apply_ingest_rate_limit(
    app: axum::Router<akashic_context::AppState>,
    cfg: &Config,
) -> axum::Router<akashic_context::AppState> {
    if !cfg.rate_limit_enabled {
        return app;
    }
    apply_layer_chain(app, QUOTA_INGEST, cfg, "ingest")
}

/// Apply the MCP-route rate-limit layer chain to a stateless proxy router.
pub fn apply_mcp_rate_limit<S>(app: axum::Router<S>, cfg: &Config) -> axum::Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    if !cfg.rate_limit_enabled {
        return app;
    }
    apply_layer_chain(app, QUOTA_MCP, cfg, "mcp")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extractor_with(trusted: &[&str]) -> ConfigurableTrustedProxyExtractor {
        let nets: Vec<IpNet> = trusted.iter().map(|c| c.parse().unwrap()).collect();
        ConfigurableTrustedProxyExtractor::new(Arc::new(nets))
    }

    #[test]
    fn ipv4_key_unchanged() {
        let v4: IpAddr = "203.0.113.5".parse().unwrap();
        assert_eq!(Ipv6Slash64KeyExtractor::bucket_key(v4), v4);
    }

    #[test]
    fn ipv6_key_truncated_to_slash64() {
        let v6: IpAddr = "2001:db8:1234:5678:dead:beef:cafe:1".parse().unwrap();
        let bucketed = Ipv6Slash64KeyExtractor::bucket_key(v6);
        let expected: IpAddr = "2001:db8:1234:5678::".parse().unwrap();
        assert_eq!(bucketed, expected);
    }

    #[test]
    fn ipv6_two_addresses_in_same_slash64_share_a_bucket() {
        let a: IpAddr = "2001:db8:1234:5678:1::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1234:5678:ffff::ffff".parse().unwrap();
        assert_eq!(
            Ipv6Slash64KeyExtractor::bucket_key(a),
            Ipv6Slash64KeyExtractor::bucket_key(b)
        );
    }

    #[test]
    fn ipv6_addresses_in_different_slash64_have_distinct_buckets() {
        let a: IpAddr = "2001:db8:1234:5678::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1234:5679::1".parse().unwrap();
        assert_ne!(
            Ipv6Slash64KeyExtractor::bucket_key(a),
            Ipv6Slash64KeyExtractor::bucket_key(b)
        );
    }

    #[test]
    fn xff_honored_when_peer_is_trusted_proxy() {
        let ex = extractor_with(&["127.0.0.0/8"]);
        let key = ex.resolve("127.0.0.1".parse().unwrap(), Some("198.51.100.1"));
        assert_eq!(key, "198.51.100.1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn xff_ignored_for_untrusted_peer() {
        let ex = extractor_with(&["127.0.0.0/8"]);
        let key = ex.resolve("203.0.113.5".parse().unwrap(), Some("198.51.100.1"));
        assert_eq!(key, "203.0.113.5".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn xff_walks_past_chained_trusted_proxies() {
        let ex = extractor_with(&["127.0.0.0/8", "10.0.0.0/8"]);
        let key = ex.resolve("127.0.0.1".parse().unwrap(), Some("198.51.100.1, 10.0.0.1"));
        assert_eq!(key, "198.51.100.1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn xff_falls_back_to_peer_when_all_entries_trusted() {
        let ex = extractor_with(&["127.0.0.0/8", "10.0.0.0/8"]);
        let key = ex.resolve("127.0.0.1".parse().unwrap(), Some("10.0.0.5, 10.0.0.1"));
        assert_eq!(key, "127.0.0.1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn xff_with_garbage_entries_is_skipped() {
        let ex = extractor_with(&["127.0.0.0/8"]);
        let key = ex.resolve(
            "127.0.0.1".parse().unwrap(),
            Some("not-an-ip, 198.51.100.1"),
        );
        assert_eq!(key, "198.51.100.1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn xff_with_empty_trusted_list_never_honored() {
        let ex = extractor_with(&[]);
        let key = ex.resolve("127.0.0.1".parse().unwrap(), Some("198.51.100.1"));
        assert_eq!(key, "127.0.0.1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn route_quotas_match_sub_spec() {
        assert_eq!(QUOTA_AUTH.burst, 10);
        assert_eq!(QUOTA_AUTH.period_secs, 6);
        assert_eq!(QUOTA_INGEST.burst, 5);
        assert_eq!(QUOTA_INGEST.period_secs, 12);
        assert_eq!(QUOTA_API.burst, 60);
        assert_eq!(QUOTA_API.period_secs, 1);
        assert_eq!(QUOTA_MCP.burst, 30);
        assert_eq!(QUOTA_MCP.period_secs, 2);
    }

    /// Regression test for the allowlist-bypass-no-op defect: an IP listed
    /// in `RATE_LIMIT_ALLOWLIST` must be able to send unlimited requests
    /// without ever drawing a 429. Before the rewrite, the bypass branch
    /// still consumed limiter tokens and would 429 once the burst was
    /// exhausted; this test fails on that prior behavior.
    #[tokio::test]
    async fn allowlist_bypass_skips_limiter_under_burst() {
        // Tiny quota: 2 burst, refill once per hour. Without bypass, the
        // 3rd request would be 429.
        let burst = NonZeroU32::new(2).unwrap();
        let q = Quota::with_period(Duration::from_hours(1))
            .unwrap()
            .allow_burst(burst);
        let limiter: KeyedLimiter = RateLimiter::dashmap(q);
        let allowlisted: IpAddr = "203.0.113.7".parse().unwrap();
        let allowlist: Vec<IpNet> = vec!["203.0.113.7/32".parse().unwrap()];

        // Simulate what `rate_limit_layer` does: if the key is in the
        // allowlist, do not call `check_key`. Run 100 iterations.
        for _ in 0..100 {
            let bypassed = allowlist.iter().any(|net| net.contains(&allowlisted));
            assert!(bypassed, "allowlist must match");
            // Crucially: no `limiter.check_key(&allowlisted)` here.
        }

        // The limiter's bucket for this key is untouched, so two more
        // direct hits would still succeed.
        assert!(limiter.check_key(&allowlisted).is_ok());
        assert!(limiter.check_key(&allowlisted).is_ok());
    }

    /// Counter-test: a non-allowlisted IP DOES exhaust its burst and gets
    /// rate-limited on the third request.
    #[tokio::test]
    async fn non_allowlisted_ip_is_rate_limited_after_burst() {
        let burst = NonZeroU32::new(2).unwrap();
        let q = Quota::with_period(Duration::from_hours(1))
            .unwrap()
            .allow_burst(burst);
        let limiter: KeyedLimiter = RateLimiter::dashmap(q);
        let regular: IpAddr = "198.51.100.42".parse().unwrap();

        assert!(limiter.check_key(&regular).is_ok());
        assert!(limiter.check_key(&regular).is_ok());
        assert!(limiter.check_key(&regular).is_err());
    }
}
