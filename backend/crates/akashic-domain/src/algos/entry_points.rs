//! Entry-point detection — pure, DB-free.
//!
//! Moved from `akashic-ingestion::ingestion::entry_points` (A1 Task 2).

use regex::Regex;
use std::sync::LazyLock;
use uuid::Uuid;

/// An entry point detected from a chunk's content.
#[derive(Debug, Clone)]
pub struct DetectedEntryPoint {
    pub chunk_id: Uuid,
    pub chunk_name: String,
    pub module_path: String,
    pub entry_type: String,
    pub display_name: String,
}

// ── Rust patterns ─────────────────────────────────────────────────────────────

static RE_RUST_MAIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^(?:pub\s+)?fn\s+main\s*\(").unwrap());

static RE_RUST_TOKIO_MAIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"#\[tokio::main\]").unwrap());

static RE_RUST_AXUM_HANDLER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\.route\(\s*"([^"]+)"\s*,\s*(get|post|put|delete|patch)\("#).unwrap()
});

static RE_RUST_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"#\[(?:tokio::)?test\]").unwrap());

// ── TypeScript patterns ───────────────────────────────────────────────────────

static RE_TS_APP_ROUTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:^|[^A-Za-z0-9_$])(?:app|router)\.(get|post|put|delete|patch|all)\(\s*['"]([^'"]+)['"]"#,
    )
    .unwrap()
});

static RE_TS_EXPORT_DEFAULT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^export\s+default\s+").unwrap());

static RE_TS_LISTENER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"addEventListener\(\s*['"]([^'"]+)['"]"#).unwrap());

/// Detect if a chunk is an entry point based on its content and language.
///
/// Returns `None` if not an entry point, or `Some((entry_type, display_name))`.
pub fn detect(content: &str, chunk_name: &str, language: &str) -> Option<(String, String)> {
    match language {
        "rust" => detect_rust(content, chunk_name),
        "typescript" | "javascript" => detect_typescript(content, chunk_name),
        _ => None,
    }
}

fn detect_rust(content: &str, chunk_name: &str) -> Option<(String, String)> {
    if RE_RUST_MAIN.is_match(content) || RE_RUST_TOKIO_MAIN.is_match(content) {
        return Some(("main".into(), "fn main".into()));
    }

    if let Some(cap) = RE_RUST_AXUM_HANDLER.captures(content) {
        let path = cap.get(1).map_or("/", |m| m.as_str());
        let method = cap.get(2).map_or("get", |m| m.as_str()).to_uppercase();
        return Some(("api_handler".into(), format!("{method} {path}")));
    }

    if RE_RUST_TEST.is_match(content) {
        return Some(("test".into(), format!("test::{chunk_name}")));
    }

    None
}

fn detect_typescript(content: &str, chunk_name: &str) -> Option<(String, String)> {
    if let Some(cap) = RE_TS_APP_ROUTE.captures(content) {
        let method = cap.get(1).map_or("get", |m| m.as_str()).to_uppercase();
        let path = cap.get(2).map_or("/", |m| m.as_str());
        return Some(("api_handler".into(), format!("{method} {path}")));
    }

    if let Some(cap) = RE_TS_LISTENER.captures(content) {
        let event = cap.get(1).map_or("event", |m| m.as_str());
        return Some(("event_listener".into(), format!("on:{event}")));
    }

    if RE_TS_EXPORT_DEFAULT.is_match(content) {
        return Some(("module_entry".into(), format!("default:{chunk_name}")));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_main() {
        let content = "pub fn main() {\n    println!(\"hello\");\n}";
        let result = detect(content, "main", "rust");
        assert_eq!(result, Some(("main".into(), "fn main".into())));
    }

    #[test]
    fn rust_tokio_main() {
        let content = "#[tokio::main]\nasync fn main() {}";
        let result = detect(content, "main", "rust");
        assert_eq!(result, Some(("main".into(), "fn main".into())));
    }

    #[test]
    fn rust_axum_handler() {
        let content = r#".route("/api/auth/login", post(handle_login))"#;
        let result = detect(content, "handle_login", "rust");
        assert_eq!(
            result,
            Some(("api_handler".into(), "POST /api/auth/login".into()))
        );
    }

    #[test]
    fn rust_test_fn() {
        let content = "#[test]\nfn test_parse() {}";
        let result = detect(content, "test_parse", "rust");
        assert_eq!(result, Some(("test".into(), "test::test_parse".into())));
    }

    #[test]
    fn ts_express_route() {
        let content = r#"app.post('/api/users', createUser)"#;
        let result = detect(content, "createUser", "typescript");
        assert_eq!(
            result,
            Some(("api_handler".into(), "POST /api/users".into()))
        );
    }

    #[test]
    fn ts_event_listener() {
        let content = r#"addEventListener('click', handleClick)"#;
        let result = detect(content, "handleClick", "typescript");
        assert_eq!(result, Some(("event_listener".into(), "on:click".into())));
    }

    #[test]
    fn ts_export_default() {
        let content = "export default function App() {}";
        let result = detect(content, "App", "typescript");
        assert_eq!(result, Some(("module_entry".into(), "default:App".into())));
    }

    #[test]
    fn not_entry_point() {
        let content = "fn helper_func(x: i32) -> i32 { x + 1 }";
        let result = detect(content, "helper_func", "rust");
        assert_eq!(result, None);
    }

    #[test]
    fn unknown_language() {
        let content = "def main(): pass";
        let result = detect(content, "main", "python");
        assert_eq!(result, None);
    }

    #[test]
    fn ts_route_rejects_identifier_suffix() {
        let content = r#"myapp.get('/internal', noop)"#;
        assert_eq!(detect(content, "noop", "typescript"), None);

        let content = r#"subrouter.post('/internal', noop)"#;
        assert_eq!(detect(content, "noop", "typescript"), None);
    }

    #[test]
    fn ts_route_standalone_still_matches() {
        let content = r#"app.get('/health', healthCheck)"#;
        assert_eq!(
            detect(content, "healthCheck", "typescript"),
            Some(("api_handler".into(), "GET /health".into()))
        );

        let content = r#"router.delete('/items/:id', removeItem)"#;
        assert_eq!(
            detect(content, "removeItem", "typescript"),
            Some(("api_handler".into(), "DELETE /items/:id".into()))
        );
    }

    #[test]
    fn ts_route_member_access_matches() {
        let content = r#"this.app.post('/login', login)"#;
        assert_eq!(
            detect(content, "login", "typescript"),
            Some(("api_handler".into(), "POST /login".into()))
        );
    }
}
