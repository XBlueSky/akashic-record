//! B2 audit log adapter: records every MCP write-tool invocation attempt.
//!
//! `PgAuditRepo` implements `akashic_kernel::AuditPort` (the abstraction the
//! quota decorators + the MCP proxy depend on). Moved out of `akashic-mcp`
//! (Slice D) so the only audit SQL lives in the store-pg adapter, like every
//! other repo. Fire-and-forget: audit failures emit
//! `tracing::error!(event = "audit_write_failed", …)` and never fail the write.
//!
//! Schema: `crate::init_auth_schema` (`audit_log` table).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::{error, warn};

use akashic_kernel::actor::Authenticated;
use akashic_kernel::{AuditPort, parse_actor_id};

/// Maximum bytes of upstream response stored in `audit_log.response_summary`.
pub const AUDIT_RESPONSE_SUMMARY_MAX_BYTES: usize = 200;

/// PostgreSQL adapter implementing [`AuditPort`]. Constructed at the
/// composition root (`main.rs`) and injected into `QuotaLlm`/`QuotaEmbedding`;
/// also built ad-hoc by the MCP proxy from its pool.
#[derive(Clone)]
pub struct PgAuditRepo {
    pg: PgPool,
}

impl PgAuditRepo {
    pub fn new(pg: PgPool) -> Self {
        Self { pg }
    }
}

#[async_trait]
impl AuditPort for PgAuditRepo {
    /// Record a single audit_log row for a write tool invocation ATTEMPT.
    ///
    /// `success` carries whether the upstream accepted it (a 2xx). Recording
    /// attempts (not just successes) preserves the "every write is auditable"
    /// invariant. Errors are logged and discarded.
    async fn record_write(
        &self,
        auth: Arc<dyn Authenticated>,
        tool_name: String,
        args: Value,
        response_body: Vec<u8>,
        success: bool,
        peer_ip: Option<std::net::IpAddr>,
    ) {
        // 1. Parse actor_user_id from "gitlab:user:<N>" format.
        let actor_user_id = parse_actor_id(auth.actor_id()).unwrap_or_else(|| {
            warn!(
                event = "audit_actor_id_parse_failed",
                actor_id = auth.actor_id()
            );
            0
        });

        // 2. Compute after_hash = sha256(canonicalize_args(args)).
        let after_hash = Sha256::digest(canonicalize_args(&args)).to_vec();

        // 3. Compute target_id (best-effort).
        let target_id = extract_target_id(&tool_name, &args);

        // 4. Compute response_summary: first 200 bytes, lossy UTF-8.
        let response_summary = response_summary(&response_body);

        // 5. Serialize auth_method to its wire string ("device_flow" | "gitlab_passthrough").
        let auth_method = serde_json::to_value(auth.auth_method())
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".into());

        // 6. INSERT.
        let result = sqlx::query(
            "INSERT INTO audit_log \
                (actor_user_id, actor_token_id, auth_method, action, target_id, after_hash, ip, success, response_summary) \
             VALUES ($1, $2, $3, $4, $5, $6, $7::inet, $8, $9)",
        )
        .bind(actor_user_id)
        .bind(auth.actor_token_id())
        .bind(&auth_method)
        .bind(&tool_name)
        .bind(target_id)
        .bind(after_hash.as_slice())
        .bind(peer_ip.map(|ip| ip.to_string()))
        .bind(success)
        .bind(response_summary)
        .execute(&self.pg)
        .await;

        if let Err(e) = result {
            error!(
                event = "audit_write_failed",
                error = %e,
                action = %tool_name,
                actor_user_id,
                actor_token_id = auth.actor_token_id(),
            );
        } else {
            // C5: count successful audit writes by action.
            metrics::counter!(
                "akashic_audit_log_writes_total",
                "action" => tool_name.clone(),
            )
            .increment(1);
        }
    }
}

/// Produce a canonical byte representation of a JSON value where object keys
/// are sorted lexicographically. Arrays preserve order. Used as input to
/// SHA-256 so semantically identical args produce identical hashes regardless
/// of key order at write time.
pub fn canonicalize_args(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => {
            out.extend_from_slice(n.to_string().as_bytes());
        }
        Value::String(s) => {
            let escaped = serde_json::to_string(s).expect("string serialization is infallible");
            out.extend_from_slice(escaped.as_bytes());
        }
        Value::Array(arr) => {
            out.push(b'[');
            for (i, v) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(v, out);
            }
            out.push(b']');
        }
        Value::Object(obj) => {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            out.push(b'{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                let escaped_key =
                    serde_json::to_string(k).expect("key serialization is infallible");
                out.extend_from_slice(escaped_key.as_bytes());
                out.push(b':');
                write_canonical(&obj[*k], out);
            }
            out.push(b'}');
        }
    }
}

/// Best-effort extraction of the resource id targeted by a write tool call,
/// for the `audit_log.target_id` column. Returns `None` when the tool does not
/// target a single id (e.g. `save_note` server-assigns), the field is absent,
/// or the tool has no extractor arm. Adding a new write tool requires extending
/// the arms here (the `extract_target_id_synced_with_write_tools` test in
/// `akashic-mcp` guards the WRITE_TOOLS ↔ arms invariant).
pub fn extract_target_id(tool_name: &str, args: &serde_json::Value) -> Option<String> {
    match tool_name {
        // save_note: id is server-assigned; not in the request.
        "save_note" => None,
        // supersede_note: caller must specify which note is being retired.
        "supersede_note" => args
            .get("old_note_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        // link_cross_service_calls: global rebuild — no single target id.
        "link_cross_service_calls" => None,
        other => {
            tracing::warn!(
                event = "audit_target_id_no_extractor",
                tool = other,
                "write tool has no target_id extractor; audit row target_id will be NULL"
            );
            None
        }
    }
}

/// Truncate `body` to AUDIT_RESPONSE_SUMMARY_MAX_BYTES bytes (UTF-8-safe via
/// `from_utf8_lossy`) for the audit_log.response_summary column.
fn response_summary(body: &[u8]) -> String {
    let n = body.len().min(AUDIT_RESPONSE_SUMMARY_MAX_BYTES);
    String::from_utf8_lossy(&body[..n]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_object_round_trips() {
        let v = serde_json::json!({});
        assert_eq!(canonicalize_args(&v), b"{}");
    }

    #[test]
    fn key_order_does_not_affect_canonical_form() {
        let a = serde_json::json!({"a": 1, "b": 2, "c": [1, 2, 3]});
        let b = serde_json::json!({"c": [1, 2, 3], "b": 2, "a": 1});
        assert_eq!(canonicalize_args(&a), canonicalize_args(&b));
    }

    #[test]
    fn array_order_is_preserved() {
        let a = serde_json::json!({"x": [1, 2, 3]});
        let b = serde_json::json!({"x": [3, 2, 1]});
        assert_ne!(canonicalize_args(&a), canonicalize_args(&b));
    }

    #[test]
    fn nested_object_keys_are_sorted_recursively() {
        let a = serde_json::json!({"outer": {"a": 1, "b": 2}});
        let b = serde_json::json!({"outer": {"b": 2, "a": 1}});
        assert_eq!(canonicalize_args(&a), canonicalize_args(&b));
    }

    #[test]
    fn strings_with_special_chars_escape_correctly() {
        let v = serde_json::json!({"k": "line1\nline2\"quoted\""});
        let bytes = canonicalize_args(&v);
        let s = std::str::from_utf8(&bytes).unwrap();
        let parsed: Value = serde_json::from_str(s).unwrap();
        assert_eq!(parsed, v);
    }

    #[test]
    fn extract_target_id_save_note_returns_none() {
        let args = serde_json::json!({"title": "x", "body": "y"});
        assert_eq!(extract_target_id("save_note", &args), None);
    }

    #[test]
    fn extract_target_id_supersede_note_returns_old_id() {
        let args = serde_json::json!({"old_note_id": "abc-123", "new_note_id": "def-456"});
        assert_eq!(
            extract_target_id("supersede_note", &args),
            Some("abc-123".into())
        );
    }

    #[test]
    fn extract_target_id_supersede_note_missing_field_returns_none() {
        let args = serde_json::json!({"new_note_id": "def-456"});
        assert_eq!(extract_target_id("supersede_note", &args), None);
    }

    #[test]
    fn extract_target_id_link_cross_service_calls_returns_none() {
        let args = serde_json::json!({"include_intra_repo": true});
        assert_eq!(extract_target_id("link_cross_service_calls", &args), None);
    }

    #[test]
    fn extract_target_id_unknown_tool_returns_none() {
        let args = serde_json::json!({"id": "x"});
        assert_eq!(extract_target_id("hypothetical_future_tool", &args), None);
    }

    #[test]
    fn response_summary_truncates_at_200_bytes() {
        let body = "x".repeat(500);
        let s = response_summary(body.as_bytes());
        assert_eq!(s.len(), 200);
        assert!(s.chars().all(|c| c == 'x'));
    }

    #[test]
    fn response_summary_handles_non_utf8_bytes() {
        let body: Vec<u8> = vec![0xff, 0xfe, 0x41, 0x42, 0x43];
        let s = response_summary(&body);
        assert!(s.ends_with("ABC"));
    }
}
