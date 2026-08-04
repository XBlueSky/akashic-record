//! Cross-service HTTP link matching — pure, DB-free.
//!
//! Matches client `http_call` `(method, path)` against server `route`
//! `(method, path)` across the whole graph and emits one `CrossServiceLink`
//! per (call, route) match. Method is case-insensitive with an `"ANY"`
//! wildcard (C1's fallback verb); path is exact or route-param
//! (`:id` / `{id}`). Mirrors `super::dead_code` (pure algo + DTOs + formatter).

/// A client HTTP call site: `(caller)-[:MAKES_HTTP_CALL]->(http_call)`.
/// `caller_repo`/`caller_fqn` ride along for report labels only.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HttpCallSite {
    pub caller_pg_id: String,
    pub caller_repo: String,
    pub caller_fqn: String,
    pub http_method: String,
    pub http_path: String,
}

/// A server route site: `(route)-[:ROUTES_TO]->(handler)`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RouteSite {
    pub handler_pg_id: String,
    pub handler_repo: String,
    pub handler_fqn: String,
    pub http_method: String,
    pub http_path: String,
}

/// One matched (call, route) pair to persist as a `HTTP_CALLS` edge.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CrossServiceLink {
    pub caller_pg_id: String,
    pub handler_pg_id: String,
    /// The CALL's method/path (client side).
    pub http_method: String,
    pub http_path: String,
    /// "exact" | "param".
    pub matched_via: String,
    /// `caller_repo != handler_repo`.
    pub cross_repo: bool,
}

/// Normalize a path for exact comparison: strip a single trailing `/`
/// except for the root `/`.
fn normalize_path(p: &str) -> &str {
    if p.len() > 1 {
        p.strip_suffix('/').unwrap_or(p)
    } else {
        p
    }
}

/// True if a route segment is a param placeholder (`:id` or `{id}`).
fn is_param_segment(seg: &str) -> bool {
    seg.starts_with(':') || (seg.starts_with('{') && seg.ends_with('}') && seg.len() >= 2)
}

/// Method match: case-insensitive equality, OR either side is `"ANY"`.
fn method_matches(call_method: &str, route_method: &str) -> bool {
    call_method.eq_ignore_ascii_case("ANY")
        || route_method.eq_ignore_ascii_case("ANY")
        || call_method.eq_ignore_ascii_case(route_method)
}

/// Path match. Returns `Some("exact")` / `Some("param")` on a match, `None`
/// otherwise. Exact wins when the normalized strings are equal (a param-free
/// route that matches is reported "exact").
pub fn path_matches(call_path: &str, route_path: &str) -> Option<&'static str> {
    let call_n = normalize_path(call_path);
    let route_n = normalize_path(route_path);
    if call_n == route_n {
        return Some("exact");
    }
    let call_segs: Vec<&str> = call_n.split('/').collect();
    let route_segs: Vec<&str> = route_n.split('/').collect();
    if call_segs.len() != route_segs.len() {
        return None;
    }
    let mut saw_param = false;
    for (c, r) in call_segs.iter().zip(route_segs.iter()) {
        if is_param_segment(r) {
            // Param segment matches any single NON-EMPTY call segment.
            if c.is_empty() {
                return None;
            }
            saw_param = true;
        } else if c != r {
            return None;
        }
    }
    // A param-free route that reaches here would have matched "exact" above,
    // so any survivor with no param is a defensive "exact"; with a param it's
    // "param".
    Some(if saw_param { "param" } else { "exact" })
}

/// Match every client call against every route; one `CrossServiceLink` per
/// (call, route) match. Deterministic order: outer = call input order, inner
/// = route input order.
pub fn link_calls(calls: &[HttpCallSite], routes: &[RouteSite]) -> Vec<CrossServiceLink> {
    // Any path match — exact or param — requires equal normalized segment
    // counts (path_matches returns None on a length mismatch), so bucket routes
    // by that count and compare each call only against its own bucket. This
    // turns the O(calls × routes) cross-product into O(calls × same-length
    // routes). Buckets keep routes in input order, so the output order (calls
    // outer, routes inner in input order) is byte-for-byte unchanged.
    let seg_count = |p: &str| normalize_path(p).split('/').count();
    let mut by_seg_count: std::collections::HashMap<usize, Vec<&RouteSite>> =
        std::collections::HashMap::new();
    for route in routes {
        by_seg_count
            .entry(seg_count(&route.http_path))
            .or_default()
            .push(route);
    }

    let mut out = Vec::new();
    for call in calls {
        let Some(bucket) = by_seg_count.get(&seg_count(&call.http_path)) else {
            continue;
        };
        for route in bucket {
            if !method_matches(&call.http_method, &route.http_method) {
                continue;
            }
            if let Some(via) = path_matches(&call.http_path, &route.http_path) {
                out.push(CrossServiceLink {
                    caller_pg_id: call.caller_pg_id.clone(),
                    handler_pg_id: route.handler_pg_id.clone(),
                    http_method: call.http_method.clone(),
                    http_path: call.http_path.clone(),
                    matched_via: via.to_string(),
                    cross_repo: call.caller_repo != route.handler_repo,
                });
            }
        }
    }
    out
}

/// Client call sites that matched NO route — the "dangling" list. A call is
/// dangling when no `CrossServiceLink` shares its
/// `(caller_pg_id, http_method, http_path)` — i.e. PER-CALL-SITE, so a caller
/// that makes a mix of matched and unmatched calls still reports its unmatched
/// ones. Each item has empty `handler_*`/`matched_via` and `cross_repo=false`
/// (the placeholder convention used for unmatched rows).
pub fn dangling_calls(
    calls: &[HttpCallSite],
    links: &[CrossServiceLink],
) -> Vec<CrossServiceLinkItem> {
    let linked: std::collections::HashSet<(&str, &str, &str)> = links
        .iter()
        .map(|l| {
            (
                l.caller_pg_id.as_str(),
                l.http_method.as_str(),
                l.http_path.as_str(),
            )
        })
        .collect();
    calls
        .iter()
        .filter(|c| {
            !linked.contains(&(
                c.caller_pg_id.as_str(),
                c.http_method.as_str(),
                c.http_path.as_str(),
            ))
        })
        .map(|c| CrossServiceLinkItem {
            caller_repo: c.caller_repo.clone(),
            caller_fqn: c.caller_fqn.clone(),
            handler_repo: String::new(),
            handler_fqn: String::new(),
            http_method: c.http_method.clone(),
            http_path: c.http_path.clone(),
            matched_via: String::new(),
            cross_repo: false,
        })
        .collect()
}

// ── Report DTOs + formatter (mirror dead_code::DeadCodeReport) ────────────────

/// One rendered link (or unmatched call) row for the report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CrossServiceLinkItem {
    pub caller_repo: String,
    pub caller_fqn: String,
    pub handler_repo: String,
    pub handler_fqn: String,
    pub http_method: String,
    pub http_path: String,
    pub matched_via: String,
    pub cross_repo: bool,
}

/// Summary + rendered links + dangling (unmatched) client calls.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CrossServiceLinkReport {
    pub total_calls: usize,
    pub total_routes: usize,
    pub links_created: usize,
    pub cross_repo_links: usize,
    pub intra_repo_links: usize,
    pub links: Vec<CrossServiceLinkItem>,
    pub unmatched: Vec<CrossServiceLinkItem>,
}

/// Render the report as agent-facing text: summary + links (optionally
/// cross-repo-only) + dangling calls, each list capped.
pub fn format_cross_service_links(
    report: &CrossServiceLinkReport,
    include_intra_repo: bool,
    max_links: usize,
    max_unmatched: usize,
) -> String {
    let mut out = format!(
        "Cross-service HTTP_CALLS link (all repos).\n\
         Sites: {} http_calls · {} routes.\n\
         Links: {} created · {} cross-repo · {} intra-repo.\n\n",
        report.total_calls,
        report.total_routes,
        report.links_created,
        report.cross_repo_links,
        report.intra_repo_links,
    );

    let shown: Vec<&CrossServiceLinkItem> = report
        .links
        .iter()
        .filter(|l| include_intra_repo || l.cross_repo)
        .collect();

    if shown.is_empty() {
        out.push_str("No links to show.\n\n");
    } else {
        for l in shown.iter().take(max_links) {
            let scope = if l.cross_repo {
                "cross-repo"
            } else {
                "intra-repo"
            };
            out.push_str(&format!(
                "[{}] {} {}  {}::{} -> {}::{}  ({})\n",
                l.matched_via,
                l.http_method,
                l.http_path,
                l.caller_repo,
                l.caller_fqn,
                l.handler_repo,
                l.handler_fqn,
                scope,
            ));
        }
        if shown.len() > max_links {
            out.push_str(&format!(
                "(+{} more, truncated — raise max_links)\n",
                shown.len() - max_links
            ));
        }
        out.push('\n');
    }

    if !report.unmatched.is_empty() {
        out.push_str(&format!(
            "Dangling client calls (no matching route — candidate missing routes / external endpoints): {}\n",
            report.unmatched.len()
        ));
        for u in report.unmatched.iter().take(max_unmatched) {
            out.push_str(&format!(
                "  {} {}  (from {}::{})\n",
                u.http_method, u.http_path, u.caller_repo, u.caller_fqn
            ));
        }
        if report.unmatched.len() > max_unmatched {
            out.push_str(&format!(
                "  (+{} more, truncated — raise max_unmatched)\n",
                report.unmatched.len() - max_unmatched
            ));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(repo: &str, method: &str, path: &str) -> HttpCallSite {
        HttpCallSite {
            caller_pg_id: format!("c-{repo}-{method}-{path}"),
            caller_repo: repo.into(),
            caller_fqn: "caller_fn".into(),
            http_method: method.into(),
            http_path: path.into(),
        }
    }

    fn route(repo: &str, method: &str, path: &str) -> RouteSite {
        RouteSite {
            handler_pg_id: format!("h-{repo}-{method}-{path}"),
            handler_repo: repo.into(),
            handler_fqn: "handler_fn".into(),
            http_method: method.into(),
            http_path: path.into(),
        }
    }

    #[test]
    fn exact_match_one_link() {
        let links = link_calls(
            &[call("client", "GET", "/api/widget")],
            &[route("server", "GET", "/api/widget")],
        );
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].matched_via, "exact");
        assert_eq!(links[0].http_path, "/api/widget");
        assert!(links[0].cross_repo);
    }

    #[test]
    fn param_match_colon() {
        let links = link_calls(
            &[call("client", "GET", "/api/users/42")],
            &[route("server", "GET", "/api/users/:id")],
        );
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].matched_via, "param");
    }

    #[test]
    fn param_match_brace() {
        let links = link_calls(
            &[call("client", "GET", "/api/users/42")],
            &[route("server", "GET", "/api/users/{id}")],
        );
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].matched_via, "param");
    }

    #[test]
    fn method_mismatch_no_link() {
        let links = link_calls(
            &[call("client", "POST", "/api/widget")],
            &[route("server", "GET", "/api/widget")],
        );
        assert!(links.is_empty());
    }

    #[test]
    fn any_method_matches_either_side() {
        // ANY on the call side.
        assert_eq!(
            link_calls(&[call("c", "ANY", "/x")], &[route("s", "GET", "/x")]).len(),
            1
        );
        // ANY on the route side.
        assert_eq!(
            link_calls(&[call("c", "DELETE", "/x")], &[route("s", "ANY", "/x")]).len(),
            1
        );
    }

    #[test]
    fn segment_count_mismatch_no_link() {
        let links = link_calls(&[call("c", "GET", "/a/b")], &[route("s", "GET", "/a/b/c")]);
        assert!(links.is_empty());
    }

    #[test]
    fn non_param_differing_segment_no_link() {
        let links = link_calls(
            &[call("c", "GET", "/api/x")],
            &[route("s", "GET", "/api/y")],
        );
        assert!(links.is_empty());
    }

    #[test]
    fn one_call_two_routes_two_links() {
        let links = link_calls(
            &[call("client", "GET", "/api/widget")],
            &[
                route("server-a", "GET", "/api/widget"),
                route("server-b", "GET", "/api/widget"),
            ],
        );
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn trailing_slash_normalized() {
        let links = link_calls(
            &[call("c", "GET", "/api/x/")],
            &[route("s", "GET", "/api/x")],
        );
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].matched_via, "exact");
    }

    #[test]
    fn cross_repo_flag_set_correctly() {
        // Same repo → intra.
        let intra = link_calls(&[call("same", "GET", "/x")], &[route("same", "GET", "/x")]);
        assert_eq!(intra.len(), 1);
        assert!(!intra[0].cross_repo);
        // Different repo → cross.
        let cross = link_calls(&[call("a", "GET", "/x")], &[route("b", "GET", "/x")]);
        assert!(cross[0].cross_repo);
    }

    #[test]
    fn empty_inputs_empty_output() {
        assert!(link_calls(&[], &[route("s", "GET", "/x")]).is_empty());
        assert!(link_calls(&[call("c", "GET", "/x")], &[]).is_empty());
        assert!(link_calls(&[], &[]).is_empty());
    }

    #[test]
    fn param_empty_call_segment_no_match() {
        // "/api/users/" normalizes to "/api/users" (3 segs: ["", "api", "users"])
        // vs "/api/users/:id" (4 segs: ["", "api", "users", ":id"]) → segment-count
        // mismatch, NOT the empty-segment guard (tested by param_segment_guard_rejects_empty_call_segment).
        let links = link_calls(
            &[call("c", "GET", "/api/users/")],
            &[route("s", "GET", "/api/users/:id")],
        );
        assert!(links.is_empty());
    }

    #[test]
    fn root_path_not_stripped() {
        let links = link_calls(&[call("c", "GET", "/")], &[route("s", "GET", "/")]);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].matched_via, "exact");
    }

    #[test]
    fn format_has_summary_and_links() {
        let report = CrossServiceLinkReport {
            total_calls: 2,
            total_routes: 1,
            links_created: 1,
            cross_repo_links: 1,
            intra_repo_links: 0,
            links: vec![CrossServiceLinkItem {
                caller_repo: "client".into(),
                caller_fqn: "caller_fn".into(),
                handler_repo: "server".into(),
                handler_fqn: "widget_handler".into(),
                http_method: "GET".into(),
                http_path: "/api/widget".into(),
                matched_via: "exact".into(),
                cross_repo: true,
            }],
            unmatched: vec![CrossServiceLinkItem {
                caller_repo: "client".into(),
                caller_fqn: "orphan_fn".into(),
                handler_repo: String::new(),
                handler_fqn: String::new(),
                http_method: "GET".into(),
                http_path: "/api/missing".into(),
                matched_via: String::new(),
                cross_repo: false,
            }],
        };
        let s = format_cross_service_links(&report, true, 25, 25);
        assert!(s.contains("2 http_calls"));
        assert!(s.contains("widget_handler"));
        assert!(s.contains("Dangling client calls"));
        assert!(s.contains("/api/missing"));
    }

    #[test]
    fn format_cross_repo_only_hides_intra() {
        let report = CrossServiceLinkReport {
            total_calls: 1,
            total_routes: 1,
            links_created: 1,
            cross_repo_links: 0,
            intra_repo_links: 1,
            links: vec![CrossServiceLinkItem {
                caller_repo: "same".into(),
                caller_fqn: "f".into(),
                handler_repo: "same".into(),
                handler_fqn: "g".into(),
                http_method: "GET".into(),
                http_path: "/x".into(),
                matched_via: "exact".into(),
                cross_repo: false,
            }],
            unmatched: vec![],
        };
        let s = format_cross_service_links(&report, false, 25, 25);
        assert!(s.contains("No links to show"));
    }

    #[test]
    fn param_segment_guard_rejects_empty_call_segment() {
        // "/" splits to ["", ""] (2 segs); "/:id" splits to ["", ":id"] (2 segs).
        // Equal length, so the per-segment loop runs and the second pair
        // ("" , ":id") reaches the `is_param_segment` branch with an EMPTY call
        // segment → the guard returns None (a param must match a NON-EMPTY segment).
        let links = link_calls(&[call("c", "GET", "/")], &[route("s", "GET", "/:id")]);
        assert!(
            links.is_empty(),
            "a param route segment must not match an empty call segment"
        );
    }

    #[test]
    fn method_match_is_case_insensitive() {
        let links = link_calls(
            &[call("client", "get", "/api/widget")],
            &[route("server", "GET", "/api/widget")],
        );
        assert_eq!(
            links.len(),
            1,
            "lowercase 'get' must match 'GET' (case-insensitive)"
        );
    }

    #[test]
    fn dangling_is_per_call_site_not_per_caller() {
        // ONE caller (fn1) makes TWO calls: /api/widget (matches) + /api/missing (no route).
        let calls = vec![
            HttpCallSite {
                caller_pg_id: "fn1".into(),
                caller_repo: "client".into(),
                caller_fqn: "foo".into(),
                http_method: "GET".into(),
                http_path: "/api/widget".into(),
            },
            HttpCallSite {
                caller_pg_id: "fn1".into(),
                caller_repo: "client".into(),
                caller_fqn: "foo".into(),
                http_method: "GET".into(),
                http_path: "/api/missing".into(),
            },
        ];
        let routes = vec![RouteSite {
            handler_pg_id: "h1".into(),
            handler_repo: "server".into(),
            handler_fqn: "bar".into(),
            http_method: "GET".into(),
            http_path: "/api/widget".into(),
        }];
        let links = link_calls(&calls, &routes);
        assert_eq!(links.len(), 1, "only /api/widget should match");
        let dangling = dangling_calls(&calls, &links);
        assert_eq!(
            dangling.len(),
            1,
            "the same caller's /api/missing must still be dangling"
        );
        assert_eq!(dangling[0].http_path, "/api/missing");
        assert_eq!(dangling[0].caller_fqn, "foo");
    }
}
