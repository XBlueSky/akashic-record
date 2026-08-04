use std::collections::HashSet;
use std::net::{IpAddr, Ipv6Addr};

use anyhow::{Context, Result};

use scraper::{Html, Selector};

tokio::task_local! {
    /// The seed host the in-progress crawl is authorized to reach on a
    /// non-public IP. Set by `crawl_pages` (runs only for an admin-approved
    /// source); absent for the probe and ad-hoc fetches ⇒ strict SSRF.
    pub(crate) static CRAWL_ALLOWED_HOST: String;
}

#[derive(Debug, PartialEq, Eq)]
enum RedirectAction {
    Follow,
    Stop,
    Error,
}

/// Follow only if the redirect target host equals the original request host.
/// Cross-host (incl. an allowed host -> internal host hop) -> Stop. Over the
/// hop cap -> Error (loop guard).
fn same_host_redirect_decision(
    target_host: Option<&str>,
    original_host: Option<&str>,
    chain_len: usize,
) -> RedirectAction {
    if chain_len >= 10 {
        return RedirectAction::Error;
    }
    match (target_host, original_host) {
        (Some(t), Some(o)) if t == o => RedirectAction::Follow,
        _ => RedirectAction::Stop, // host changed, or a host is missing -> don't follow
    }
}

/// Build the crawl HTTP client: a request timeout plus a redirect policy that
/// follows a 3xx ONLY if it stays on the ORIGINAL request host. A cross-host
/// redirect is stopped (the caller sees the 3xx as the response), which blocks
/// the "allowed host 302s to an internal host" SSRF — reqwest would otherwise
/// follow it without re-running assert_fetchable. Capped at 10 hops (loop guard).
pub(crate) fn build_crawl_client(timeout_secs: u64) -> reqwest::Client {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        let original = attempt
            .previous()
            .first()
            .and_then(|u| u.host_str().map(str::to_string));
        let target = attempt.url().host_str().map(str::to_string);
        match same_host_redirect_decision(
            target.as_deref(),
            original.as_deref(),
            attempt.previous().len(),
        ) {
            RedirectAction::Follow => attempt.follow(),
            RedirectAction::Stop => attempt.stop(),
            RedirectAction::Error => attempt.error("crawl redirect limit exceeded"),
        }
    });
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .redirect(policy)
        .build()
        // Fail loud rather than silently fall back to Client::default(), whose
        // DEFAULT redirect policy follows cross-host hops and would re-open the
        // redirect-SSRF this builder exists to close. .build() only errors on a
        // (practically impossible) TLS-init failure.
        .expect("crawl HTTP client build failed (reqwest TLS init)")
}

/// SSRF-guarded GET with the crawler User-Agent; errors on a non-success
/// status. Returns the response for the caller to read — `fetch_page` adds a
/// content-type check, `fetch_text` does not.
async fn guarded_get(client: &reqwest::Client, url: &str) -> Result<reqwest::Response> {
    assert_fetchable(url).await?;
    let resp = client
        .get(url)
        .header("User-Agent", "AkashicRecord/1.0 (knowledge-indexer)")
        .send()
        .await
        .context("HTTP request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }
    Ok(resp)
}

/// Fetch a page and return the HTML body.
pub(crate) async fn fetch_page(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = guarded_get(client, url).await?;

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !content_type.contains("text/html") {
        anyhow::bail!("Not HTML: {content_type}");
    }

    resp.text().await.context("Failed to read response body")
}

/// Fetch a URL as text, SSRF-guarded, WITHOUT enforcing a content-type. Used by
/// bulk adapters that fetch JSON / plain-text index files (search_index.json,
/// llms-full.txt, MediaWiki API) which `fetch_page`'s text/html check rejects.
pub(crate) async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String> {
    guarded_get(client, url)
        .await?
        .text()
        .await
        .context("Failed to read response body")
}

/// Convert HTML to markdown, stripping navigation and boilerplate.
pub(crate) fn html_to_markdown(html_str: &str) -> String {
    let doc = Html::parse_document(html_str);
    let mut out = String::new();

    // Selectors for content extraction
    let body_sel = Selector::parse("body").expect("valid CSS selector");
    let skip_sel =
        Selector::parse("nav, footer, header, aside, script, style, noscript, svg, iframe")
            .expect("valid CSS selector");

    let body = match doc.select(&body_sel).next() {
        Some(b) => b,
        None => return String::new(),
    };

    // Collect node IDs of elements to skip
    let skip_ids: HashSet<_> = body.select(&skip_sel).map(|el| el.id()).collect();

    // Handled block elements: each emits its FULL descendant text, so a handled
    // block nested inside another handled block would emit the same text twice
    // (e.g. `<li><p>x</p></li>` or a nested list). Collect their ids and skip an
    // element that has a handled-block ancestor — only the OUTERMOST block emits.
    let block_sel = Selector::parse("pre, h1, h2, h3, h4, h5, h6, p, li, blockquote")
        .expect("valid CSS selector");
    let block_ids: HashSet<_> = body.select(&block_sel).map(|el| el.id()).collect();

    // Process descendant elements
    for node in body.descendants() {
        if let Some(el) = scraper::ElementRef::wrap(node) {
            // Skip if inside a nav/footer/etc, or nested within a handled block
            // (whose text this element's text is already part of).
            if el.ancestors().any(|ancestor| {
                scraper::ElementRef::wrap(ancestor)
                    .map(|a| skip_ids.contains(&a.id()) || block_ids.contains(&a.id()))
                    .unwrap_or(false)
            }) {
                continue;
            }

            let text = el.text().collect::<Vec<_>>().join(" ").trim().to_string();
            if text.is_empty() {
                continue;
            }

            if el.value().name() == "pre"
                || (el.value().name() == "code"
                    && el
                        .parent()
                        .map(|p| {
                            scraper::ElementRef::wrap(p)
                                .map(|pe| pe.value().name() == "pre")
                                .unwrap_or(false)
                        })
                        .unwrap_or(false))
            {
                // Only handle <pre> (not inline <code>)
                if el.value().name() == "pre" {
                    out.push_str("\n```\n");
                    out.push_str(&text);
                    out.push_str("\n```\n\n");
                }
            } else if el.value().name() == "h1" {
                out.push_str(&format!("\n# {text}\n\n"));
            } else if el.value().name() == "h2" {
                out.push_str(&format!("\n## {text}\n\n"));
            } else if el.value().name() == "h3" {
                out.push_str(&format!("\n### {text}\n\n"));
            } else if el.value().name() == "h4" {
                out.push_str(&format!("\n#### {text}\n\n"));
            } else if el.value().name() == "h5" {
                out.push_str(&format!("\n##### {text}\n\n"));
            } else if el.value().name() == "h6" {
                out.push_str(&format!("\n###### {text}\n\n"));
            } else if el.value().name() == "p" {
                out.push_str(&text);
                out.push_str("\n\n");
            } else if el.value().name() == "li" {
                out.push_str(&format!("- {text}\n"));
            } else if el.value().name() == "blockquote" {
                out.push_str(&format!("> {text}\n\n"));
            }
            // Skip td/th — they produce noisy output without table structure
        }
    }

    out
}

/// Extract same-domain links from HTML.
pub(crate) fn extract_links(
    html_str: &str,
    base_url: &str,
    domain: &str,
    url_pattern: Option<&str>,
) -> Vec<String> {
    let doc = Html::parse_document(html_str);
    let a_sel = Selector::parse("a[href]").unwrap();
    let base = reqwest::Url::parse(base_url).ok();

    let skip_extensions = [
        ".pdf", ".zip", ".tar", ".gz", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".ico", ".css",
        ".js", ".woff", ".woff2", ".ttf", ".eot", ".mp4", ".mp3", ".avi", ".mov",
    ];

    let mut links = Vec::new();

    for element in doc.select(&a_sel) {
        let href = match element.value().attr("href") {
            Some(h) => h,
            None => continue,
        };

        // Resolve relative URLs
        let resolved = if let Some(ref base) = base {
            match base.join(href) {
                Ok(u) => u,
                Err(_) => continue,
            }
        } else {
            match reqwest::Url::parse(href) {
                Ok(u) => u,
                Err(_) => continue,
            }
        };

        // Same domain only
        if resolved.host_str() != Some(domain) {
            continue;
        }

        // Skip binary files
        let path = resolved.path().to_lowercase();
        if skip_extensions.iter().any(|ext| path.ends_with(ext)) {
            continue;
        }

        // Skip fragments-only and mailto
        if resolved.scheme() != "http" && resolved.scheme() != "https" {
            continue;
        }

        // Apply URL pattern filter (simple glob)
        if let Some(pattern) = url_pattern
            && !pattern.is_empty()
            && !glob_match(pattern, resolved.path())
        {
            continue;
        }

        // Normalize: remove fragment, keep path
        let mut clean = resolved.clone();
        clean.set_fragment(None);
        links.push(clean.to_string());
    }

    links
}

/// Simple glob matching: `*` matches any (possibly empty) sequence of
/// characters. The pattern is anchored at BOTH ends — `prefix*` matches at the
/// start, `*suffix` at the end, `*mid*` anywhere, and a `*`-free pattern must
/// match the whole text (not act as a prefix).
fn glob_match(pattern: &str, text: &str) -> bool {
    let starts_any = pattern.starts_with('*');
    let ends_any = pattern.ends_with('*');
    let parts: Vec<&str> = pattern.split('*').filter(|p| !p.is_empty()).collect();

    // Empty pattern or all `*`s: matches anything.
    if parts.is_empty() {
        return true;
    }

    let last = parts.len() - 1;
    let mut pos = 0usize;

    for (i, part) in parts.iter().enumerate() {
        let head_anchored = i == 0 && !starts_any;
        let tail_anchored = i == last && !ends_any;

        if tail_anchored {
            // The final literal must be a SUFFIX (use ends_with, not find, so a
            // repeated occurrence anchors to the last one), and it must sit at
            // or after everything already consumed.
            if !text.ends_with(part) {
                return false;
            }
            let start = text.len() - part.len();
            if start < pos || (head_anchored && start != 0) {
                return false;
            }
            pos = text.len();
        } else {
            match text[pos..].find(part) {
                Some(idx) => {
                    if head_anchored && idx != 0 {
                        return false;
                    }
                    pos += idx + part.len();
                }
                None => return false,
            }
        }
    }

    true
}

/// Normalize a URL for deduplication (remove fragment, trailing slash).
pub(crate) fn normalize_url(url: &str) -> String {
    if let Ok(mut u) = reqwest::Url::parse(url) {
        u.set_fragment(None);
        let mut s = u.to_string();
        if s.ends_with('/') && s.len() > 1 {
            s.pop();
        }
        s
    } else {
        url.to_string()
    }
}

/// SSRF guard: reject non-http(s) schemes and any host that resolves to a
/// non-public IP (loopback, private, link-local incl. the cloud-metadata
/// address, CGNAT, etc.). Called for every fetched URL — the operator-supplied
/// seed and every crawled link.
async fn assert_fetchable(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).context("Invalid URL")?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => anyhow::bail!("Refusing to fetch disallowed URL scheme: {other}"),
    }
    let host = parsed.host_str().context("URL has no host")?;
    let port = parsed.port_or_known_default().unwrap_or(80);

    // Literal IP hosts are checked directly; names are resolved via DNS.
    let addrs: Vec<IpAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![ip]
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .with_context(|| format!("DNS resolution failed for {host}"))?
            .map(|sa| sa.ip())
            .collect()
    };
    if addrs.is_empty() {
        anyhow::bail!("Host {host} did not resolve to any address");
    }
    let allowed = CRAWL_ALLOWED_HOST.try_with(|h| h.clone()).ok();
    check_addrs(host, &addrs, allowed.as_deref())
}

/// Whether `ip` is a public, routable address safe to fetch.
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 (CGNAT)
                || o[0] >= 224) // 224.0.0.0/4 multicast + 240.0.0.0/4 reserved
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped addresses (::ffff:a.b.c.d) connect to the v4 address on
            // dual-stack sockets — classify them by their v4 form so a mapped
            // metadata/private/loopback address can't slip past as "public".
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || is_ipv6_unique_local(v6)
                || is_ipv6_link_local(v6))
        }
    }
}

fn is_ipv6_unique_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

fn is_ipv6_link_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

/// True only for corp-internal RFC1918 (v4) / unique-local (v6) addresses. By
/// construction this EXCLUDES metadata (169.254.169.254), loopback, link-local,
/// CGNAT, multicast and unspecified — none of which are `is_private()` /
/// unique-local — so they can never be relaxed.
fn is_corp_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private(),
        IpAddr::V6(v6) => is_ipv6_unique_local(v6),
    }
}

/// Allowed only when the fetched host equals the crawl's authorized host AND
/// the IP is corp-private (metadata/loopback never relaxed).
fn allow_trusted_internal(host: &str, ip: IpAddr, allowed_host: Option<&str>) -> bool {
    allowed_host == Some(host) && is_corp_private_ip(ip)
}

fn check_addrs(host: &str, addrs: &[IpAddr], allowed_host: Option<&str>) -> Result<()> {
    for &ip in addrs {
        if is_public_ip(ip) {
            continue;
        }
        if allow_trusted_internal(host, ip, allowed_host) {
            continue;
        }
        anyhow::bail!("Refusing to fetch {host}: resolves to non-public address {ip}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_non_public_addresses() {
        for s in [
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254", // cloud metadata
            "100.64.0.1",      // CGNAT
            "0.0.0.0",
            "::1",
            "fd00::1", // unique-local
            "fe80::1", // link-local
        ] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(!is_public_ip(ip), "{s} must be blocked as non-public");
        }
    }

    #[test]
    fn allows_public_addresses() {
        for s in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "2606:4700:4700::1111",
        ] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(is_public_ip(ip), "{s} must be allowed");
        }
    }

    #[test]
    fn corp_private_ip_only_rfc1918_and_ula() {
        for s in ["10.17.0.1", "192.168.1.1", "172.16.0.1", "fd00::1"] {
            assert!(
                is_corp_private_ip(s.parse().unwrap()),
                "{s} should be corp-private"
            );
        }
        for s in [
            "169.254.169.254", // cloud metadata — must NOT be corp-private
            "127.0.0.1",       // loopback
            "100.64.0.1",      // CGNAT
            "8.8.8.8",         // public
            "fe80::1",         // link-local
            "::1",             // loopback v6
        ] {
            assert!(
                !is_corp_private_ip(s.parse().unwrap()),
                "{s} must NOT be corp-private"
            );
        }
    }

    #[test]
    fn allow_trusted_internal_is_host_scoped() {
        let corp: IpAddr = "10.17.250.12".parse().unwrap();
        let meta: IpAddr = "169.254.169.254".parse().unwrap();
        // allowed host + corp IP → true
        assert!(allow_trusted_internal(
            "docs.internal.example",
            corp,
            Some("docs.internal.example")
        ));
        // different host + corp IP → false (cross-host blocked)
        assert!(!allow_trusted_internal(
            "evil.internal",
            corp,
            Some("docs.internal.example")
        ));
        // allowed host + metadata IP → false (metadata never relaxed)
        assert!(!allow_trusted_internal(
            "docs.internal.example",
            meta,
            Some("docs.internal.example")
        ));
        // no allowed host → false
        assert!(!allow_trusted_internal("docs.internal.example", corp, None));
    }

    #[test]
    fn check_addrs_strict_when_no_allowed_host() {
        let corp: Vec<IpAddr> = vec!["10.0.0.1".parse().unwrap()];
        let pubip: Vec<IpAddr> = vec!["8.8.8.8".parse().unwrap()];
        assert!(check_addrs("h", &corp, None).is_err()); // no scope → strict
        assert!(check_addrs("h", &pubip, None).is_ok()); // public always ok
    }

    #[test]
    fn check_addrs_relaxes_only_matching_host_corp() {
        let corp: IpAddr = "10.17.250.12".parse().unwrap();
        let meta: IpAddr = "169.254.169.254".parse().unwrap();
        let pubip: IpAddr = "8.8.8.8".parse().unwrap();
        // matching host, corp IP → ok
        assert!(check_addrs("h", &[corp], Some("h")).is_ok());
        // wrong host, corp IP → err
        assert!(check_addrs("h", &[corp], Some("other")).is_err());
        // matching host, metadata → err
        assert!(check_addrs("h", &[meta], Some("h")).is_err());
        // matching host, mixed public+corp → ok (every addr passes)
        assert!(check_addrs("h", &[pubip, corp], Some("h")).is_ok());
        // matching host, mixed corp+metadata → err
        assert!(check_addrs("h", &[corp, meta], Some("h")).is_err());
    }

    #[tokio::test]
    async fn task_local_scope_authorizes_own_host_only() {
        let corp_url = "http://10.17.250.12/x"; // literal-IP host (no DNS), corp-private
        // Outside any scope → strict (the literal IP host is non-public, no allowed host).
        assert!(assert_fetchable(corp_url).await.is_err());
        // Scoped to that exact host → allowed.
        let scoped_ok = CRAWL_ALLOWED_HOST
            .scope("10.17.250.12".to_string(), async {
                assert_fetchable(corp_url).await
            })
            .await;
        assert!(scoped_ok.is_ok());
        // Scoped to a DIFFERENT host → still blocked (cross-host).
        let scoped_other = CRAWL_ALLOWED_HOST
            .scope("other.host".to_string(), async {
                assert_fetchable(corp_url).await
            })
            .await;
        assert!(scoped_other.is_err());
    }

    // ── same_host_redirect_decision + build_crawl_client ──────────────────────

    #[test]
    fn redirect_same_host_is_followed() {
        assert_eq!(
            same_host_redirect_decision(Some("h"), Some("h"), 0),
            RedirectAction::Follow
        );
    }

    #[test]
    fn redirect_cross_host_is_stopped() {
        assert_eq!(
            same_host_redirect_decision(Some("evil.internal"), Some("h"), 1),
            RedirectAction::Stop
        );
    }

    #[test]
    fn redirect_hop_cap_is_error() {
        assert_eq!(
            same_host_redirect_decision(Some("h"), Some("h"), 10),
            RedirectAction::Error
        );
    }

    #[test]
    fn redirect_missing_target_host_is_stopped() {
        assert_eq!(
            same_host_redirect_decision(None, Some("h"), 0),
            RedirectAction::Stop
        );
    }

    #[test]
    fn redirect_missing_original_host_is_stopped() {
        assert_eq!(
            same_host_redirect_decision(Some("h"), None, 0),
            RedirectAction::Stop
        );
    }

    #[test]
    fn build_crawl_client_smoke() {
        // Must not panic; returns a usable client.
        let _client = build_crawl_client(15);
    }

    #[test]
    fn blocks_ipv4_mapped_ipv6() {
        for s in [
            "::ffff:169.254.169.254", // mapped cloud metadata
            "::ffff:10.0.0.1",        // mapped RFC1918
            "::ffff:127.0.0.1",       // mapped loopback
            "::ffff:192.168.1.1",     // mapped private
        ] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(
                !is_public_ip(ip),
                "{s} must be blocked (mapped non-public v4)"
            );
        }
        // A mapped PUBLIC v4 is still public (no false positive).
        assert!(is_public_ip("::ffff:8.8.8.8".parse().unwrap()));
    }

    // ── glob_match (url_pattern filter) ───────────────────────────────────────

    #[test]
    fn glob_suffix_pattern_is_end_anchored() {
        // `*.html` must match only paths ENDING in `.html`, not any path that
        // merely contains it. Regression: the tail literal was never anchored.
        assert!(glob_match("*.html", "/guide/intro.html"));
        assert!(!glob_match("*.html", "/x.html.bak"));
        assert!(!glob_match("*.html", "/x.html/subpage"));
    }

    #[test]
    fn glob_starless_pattern_is_full_match() {
        // A pattern with no `*` must match the whole text, not act as a prefix.
        assert!(glob_match("/docs", "/docs"));
        assert!(!glob_match("/docs", "/docsecret"));
        assert!(!glob_match("/docs", "/docs/page"));
    }

    #[test]
    fn glob_prefix_middle_and_star_preserved() {
        // Intended wildcard behavior that must survive the anchoring fix.
        assert!(glob_match("/docs/*", "/docs/intro")); // prefix + trailing star
        assert!(glob_match("/docs/*/api", "/docs/v2/api")); // middle wildcard
        assert!(glob_match("*", "/anything")); // bare star matches all
        assert!(glob_match("*intro*", "/docs/intro/x")); // floating substring
        assert!(!glob_match("/docs/*", "/blog/intro")); // wrong prefix
    }

    #[test]
    fn glob_repeated_suffix_matches_last_occurrence() {
        // The end anchor must use the LAST occurrence, not the first.
        assert!(glob_match("*abc", "xabcabc"));
    }

    // ── html_to_markdown (nested-block de-duplication) ────────────────────────

    #[test]
    fn html_to_markdown_does_not_duplicate_nested_block_text() {
        // A <p> nested in a <li> (a handled block inside another handled block)
        // must emit its text once — via the outermost block — not once per
        // ancestor block.
        let md = html_to_markdown(
            "<html><body><ul><li><p>Install the package</p></li></ul></body></html>",
        );
        assert_eq!(md.matches("Install the package").count(), 1);
    }

    #[test]
    fn html_to_markdown_nested_list_items_not_duplicated() {
        let md = html_to_markdown(
            "<html><body><ul><li>Outer<ul><li>Inner</li></ul></li></ul></body></html>",
        );
        assert_eq!(md.matches("Inner").count(), 1);
    }

    #[test]
    fn html_to_markdown_sibling_paragraphs_still_emitted() {
        // Non-nested handled blocks must each still emit (no over-skipping).
        let md = html_to_markdown("<html><body><p>First</p><p>Second</p></body></html>");
        assert_eq!(md.matches("First").count(), 1);
        assert_eq!(md.matches("Second").count(), 1);
    }
}
