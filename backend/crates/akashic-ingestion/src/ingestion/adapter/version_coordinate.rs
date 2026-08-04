//! A best-effort "applies-to" coordinate for ingested docs (Doc/Human spaces).
//! Captured at ingest time; the staleness consumer is sub-projects B/D. The
//! URL-path inference is intentionally minimal — most pages yield `None`.

/// Where a document/section applies. All fields optional; serialized to JSONB.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct VersionCoordinate {
    pub branch: Option<String>,
    pub milestone: Option<String>,
    pub product_line: Option<String>,  // e.g. a product family name
    pub version_range: Option<String>, // e.g. "7" or "7.0-7.2"
    pub as_of: Option<String>,         // ISO date, best-effort
}

/// Best-effort version coordinate from a URL path. Currently: a version-like
/// segment (`/v7/`, `v7-to-v8`) → `version_range` = the digits after the `v`.
/// Everything else → None.
pub fn infer_version_coordinate(url: &str) -> Option<VersionCoordinate> {
    let path = url.split_once("://").map_or(url, |(_, rest)| rest);
    for seg in path.split(['/', '-']) {
        if let Some(rest) = seg.to_lowercase().strip_prefix('v')
            && !rest.is_empty()
            && rest.chars().all(|c| c.is_ascii_digit() || c == '.')
        {
            return Some(VersionCoordinate {
                version_range: Some(rest.to_string()),
                ..Default::default()
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_path_segment_is_inferred() {
        let v = infer_version_coordinate("https://docs.example.com/v7/app-lifecycle").unwrap();
        assert_eq!(v.version_range.as_deref(), Some("7"));
        assert!(v.product_line.is_none());
    }

    #[test]
    fn range_style_segment_takes_first_version() {
        let v = infer_version_coordinate("https://docs.example.com/v7-to-v8/migration").unwrap();
        assert_eq!(v.version_range.as_deref(), Some("7"));
    }

    #[test]
    fn bare_v_without_digit_is_none() {
        // "/v/" (no version digit) is NOT a version hint; nor is a word that
        // merely starts with 'v'.
        assert!(infer_version_coordinate("https://docs.example.com/v/project-structure").is_none());
        assert!(infer_version_coordinate("https://docs.example.com/vitepress/guide").is_none());
    }

    #[test]
    fn unrelated_url_is_none() {
        assert!(infer_version_coordinate("https://docs.example.com/components/button").is_none());
    }

    #[test]
    fn serde_roundtrips_through_json() {
        let v = VersionCoordinate {
            version_range: Some("7".into()),
            ..Default::default()
        };
        let json = serde_json::to_value(&v).unwrap();
        let back: VersionCoordinate = serde_json::from_value(json).unwrap();
        assert_eq!(back.version_range.as_deref(), Some("7"));
    }
}
