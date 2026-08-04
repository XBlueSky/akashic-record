//! Go structural interface satisfaction — pure, DB-free.
//!
//! Moved from `akashic-ingestion::ingestion::go_structural` (A1 Task 2).

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Decide whether a `"type"` chunk's OWN declaration is an interface.
///
/// Stricter than `content.contains("interface{")`: a Go struct may carry an
/// inline interface FIELD type which must not be treated as an interface.
/// Anchors on the first `type <ident> <kind>` head.
pub fn go_type_is_interface(content: &str) -> bool {
    let head = content
        .lines()
        .map(str::trim_start)
        .find(|l| l.split_whitespace().next() == Some("type"));
    let head = match head {
        Some(h) => h,
        None => return false,
    };

    let mut tokens = head.split_whitespace();
    if tokens.next() != Some("type") {
        return false;
    }
    if tokens.next().is_none() {
        return false;
    }
    match tokens.next() {
        Some(kind) => {
            let kind = kind.trim_end_matches(['{', '}']);
            let kind = kind.split('{').next().unwrap_or(kind);
            let kind = kind.split_whitespace().next().unwrap_or(kind);
            kind == "interface"
        }
        None => false,
    }
}

/// Parse the required method names from a Go interface type's source text.
pub fn go_interface_methods(content: &str) -> Vec<String> {
    if !go_type_is_interface(content) {
        return vec![];
    }
    let brace_pos = match content
        .find("interface {")
        .or_else(|| content.find("interface{"))
        .and_then(|pos| content[pos..].find('{').map(|off| pos + off + 1))
    {
        Some(p) => p,
        None => return vec![],
    };

    let rest = &content[brace_pos..];
    let mut depth = 1i32;
    let mut body_end: Option<usize> = None;
    for (off, ch) in rest.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    body_end = Some(off);
                    break;
                }
            }
            _ => {}
        }
    }
    let body = match body_end {
        Some(end) => &rest[..end],
        None => return vec![],
    };

    let mut methods = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        if let Some(paren) = trimmed.find('(') {
            let name = trimmed[..paren].trim();
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                methods.push(name.to_string());
            }
        }
    }
    methods
}

/// Parse the receiver TYPE name from a Go method's source text.
pub fn go_method_receiver(content: &str) -> Option<String> {
    let func_pos = content.find("func")?;
    let after_kw = &content[func_pos + 4..];
    let trimmed = after_kw.trim_start();
    let after_func = trimmed.strip_prefix('(')?;

    let end = after_func.find(')')?;
    let receiver_list = &after_func[..end];

    let type_part = receiver_list
        .split_whitespace()
        .nth(1)
        .unwrap_or(receiver_list.trim());

    let bare = type_part.trim_start_matches('*').trim();
    if bare.is_empty() {
        return None;
    }
    Some(bare.to_string())
}

/// One Go package's worth of structural data for interface matching.
#[derive(Default)]
struct PackageScope {
    interfaces: Vec<(Uuid, Vec<String>)>,
    type_id_by_name: HashMap<String, Uuid>,
    methods_by_receiver: HashMap<String, HashSet<String>>,
}

fn match_within_package(scope: &PackageScope, out: &mut Vec<(Uuid, Uuid)>) {
    for (iface_id, required) in &scope.interfaces {
        for (recv_type, method_set) in &scope.methods_by_receiver {
            if required.iter().all(|m| method_set.contains(m))
                && let Some(&struct_id) = scope.type_id_by_name.get(recv_type)
                && struct_id != *iface_id
            {
                out.push((struct_id, *iface_id));
            }
        }
    }
}

/// Given `(chunk_id, name, chunk_type, content, module_path)` rows for a repo's
/// Go chunks, return `(struct_type_chunk_id, interface_chunk_id)` IMPLEMENTS
/// pairs by method-name-set containment over LOCAL interfaces.
pub fn go_structural_implements(
    rows: &[(Uuid, String, String, String, String)],
) -> Vec<(Uuid, Uuid)> {
    let mut by_package: HashMap<&str, PackageScope> = HashMap::new();

    for (id, name, chunk_type, content, module_path) in rows {
        let scope = by_package.entry(module_path.as_str()).or_default();
        match chunk_type.as_str() {
            "type" => {
                scope.type_id_by_name.insert(name.clone(), *id);

                let required = go_interface_methods(content);
                if !required.is_empty() {
                    scope.interfaces.push((*id, required));
                }
            }
            "method" => {
                if let Some(recv) = go_method_receiver(content) {
                    scope
                        .methods_by_receiver
                        .entry(recv)
                        .or_default()
                        .insert(name.clone());
                }
            }
            _ => {}
        }
    }

    let mut pairs: Vec<(Uuid, Uuid)> = Vec::new();
    for scope in by_package.values() {
        match_within_package(scope, &mut pairs);
    }

    pairs.sort();
    pairs.dedup();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_methods_shape_two_methods() {
        let content = r#"type Shape interface {
    Area() float64
    Perimeter() float64
}"#;
        let methods = go_interface_methods(content);
        assert_eq!(methods.len(), 2, "expected 2 methods, got {methods:?}");
    }

    #[test]
    fn struct_is_not_interface() {
        let content = "type Point struct { X, Y float64 }";
        assert!(!go_type_is_interface(content));
        assert!(go_interface_methods(content).is_empty());
    }

    #[test]
    fn method_receiver_value_receiver() {
        let content = "func (p Point) Area() float64 { return 0 }";
        assert_eq!(go_method_receiver(content).as_deref(), Some("Point"));
    }

    #[test]
    fn method_receiver_pointer_receiver() {
        let content = "func (p *Point) Scale(f float64) {}";
        assert_eq!(go_method_receiver(content).as_deref(), Some("Point"));
    }

    #[test]
    fn plain_function_has_no_receiver() {
        let content = "func Add(a, b int) int { return a + b }";
        assert_eq!(go_method_receiver(content), None);
    }

    #[test]
    fn structural_implements_basic() {
        let shape_id = Uuid::from_bytes([1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let rect_id = Uuid::from_bytes([2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let area_method_id = Uuid::from_bytes([3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let perim_method_id = Uuid::from_bytes([4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        let rows = vec![
            (
                shape_id,
                "Shape".to_string(),
                "type".to_string(),
                "type Shape interface {\n    Area() float64\n    Perimeter() float64\n}"
                    .to_string(),
                "pkg/geo".to_string(),
            ),
            (
                rect_id,
                "Rectangle".to_string(),
                "type".to_string(),
                "type Rectangle struct { W, H float64 }".to_string(),
                "pkg/geo".to_string(),
            ),
            (
                area_method_id,
                "Area".to_string(),
                "method".to_string(),
                "func (r Rectangle) Area() float64 { return r.W * r.H }".to_string(),
                "pkg/geo".to_string(),
            ),
            (
                perim_method_id,
                "Perimeter".to_string(),
                "method".to_string(),
                "func (r Rectangle) Perimeter() float64 { return 2*(r.W+r.H) }".to_string(),
                "pkg/geo".to_string(),
            ),
        ];

        let pairs = go_structural_implements(&rows);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0], (rect_id, shape_id));
    }
}
