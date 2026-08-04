//! File-based routing (SvelteKit / Nuxt) — pure, DB-free.
//!
//! Moved from `akashic-ingestion::ingestion::file_routes` (A1 Task 2).

/// A route inferred from a file's relative path.
pub struct FileRoute {
    pub route: String,
    pub kind: &'static str, // "page" | "endpoint"
}

/// Derive a SvelteKit/Nuxt route from a file's relative path, or None.
pub fn file_based_route(rel_path: &str) -> Option<FileRoute> {
    let segs: Vec<&str> = rel_path.split('/').filter(|s| !s.is_empty()).collect();
    let basename = *segs.last()?;

    // SvelteKit: `routes` segment + `+page.*` / `+server.*` basename.
    if let Some(ri) = segs.iter().position(|s| *s == "routes") {
        let is_page = basename.starts_with("+page.");
        let is_server = basename.starts_with("+server.");
        if is_page || is_server {
            let mut parts: Vec<&str> = Vec::new();
            for s in &segs[ri + 1..segs.len() - 1] {
                if s.starts_with('(') && s.ends_with(')') {
                    continue; // route group — not part of the URL
                }
                parts.push(s);
            }
            let route = if parts.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", parts.join("/"))
            };
            return Some(FileRoute {
                route,
                kind: if is_server { "endpoint" } else { "page" },
            });
        }
        return None;
    }

    // Nuxt: `pages` segment + `.vue` file.
    if let Some(pi) = segs.iter().position(|s| *s == "pages") {
        if let Some(stem) = basename.strip_suffix(".vue") {
            let mut parts: Vec<&str> = segs[pi + 1..segs.len() - 1].to_vec();
            if stem != "index" {
                parts.push(stem);
            }
            let route = if parts.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", parts.join("/"))
            };
            return Some(FileRoute {
                route,
                kind: "page",
            });
        }
        return None;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sveltekit_basic_about() {
        let fr = file_based_route("src/routes/about/+page.svelte").unwrap();
        assert_eq!(fr.route, "/about");
        assert_eq!(fr.kind, "page");
    }

    #[test]
    fn sveltekit_root_index() {
        let fr = file_based_route("src/routes/+page.svelte").unwrap();
        assert_eq!(fr.route, "/");
        assert_eq!(fr.kind, "page");
    }

    #[test]
    fn sveltekit_dynamic_segment() {
        let fr = file_based_route("src/routes/blog/[slug]/+page.svelte").unwrap();
        assert_eq!(fr.route, "/blog/[slug]");
        assert_eq!(fr.kind, "page");
    }

    #[test]
    fn sveltekit_route_group_dropped() {
        let fr = file_based_route("src/routes/(app)/dashboard/+page.svelte").unwrap();
        assert_eq!(fr.route, "/dashboard");
        assert_eq!(fr.kind, "page");
    }

    #[test]
    fn sveltekit_server_endpoint_kind() {
        let fr = file_based_route("src/routes/api/users/+server.ts").unwrap();
        assert_eq!(fr.route, "/api/users");
        assert_eq!(fr.kind, "endpoint");
    }

    #[test]
    fn sveltekit_layout_none() {
        let result = file_based_route("src/routes/+layout.svelte");
        assert!(result.is_none());
    }

    #[test]
    fn nuxt_index_root() {
        let fr = file_based_route("pages/index.vue").unwrap();
        assert_eq!(fr.route, "/");
        assert_eq!(fr.kind, "page");
    }

    #[test]
    fn nuxt_dynamic_segment() {
        let fr = file_based_route("pages/users/[id].vue").unwrap();
        assert_eq!(fr.route, "/users/[id]");
        assert_eq!(fr.kind, "page");
    }

    #[test]
    fn non_routes_lib_util() {
        let result = file_based_route("src/lib/util.ts");
        assert!(result.is_none());
    }
}
