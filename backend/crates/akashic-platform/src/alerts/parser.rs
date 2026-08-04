//! Minimal Prometheus exposition parser. Extracts metric samples from
//! `PrometheusHandle::render()` output.
//!
//! Skips `# HELP` and `# TYPE` comment lines. Supports the standard
//! shape `<metric>{<labels>} <value>` with optional `{labels}`. Labels
//! parse as a flat `HashMap<&str, &str>` (no quoted-value escaping —
//! the metrics crate's renderer doesn't produce quotes-in-quotes).

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Sample<'a> {
    pub metric: &'a str,
    pub labels: HashMap<&'a str, &'a str>,
    pub value: f64,
}

/// Parse a Prometheus exposition string into a flat vector of samples.
/// Returns an empty vec on parse failure for any individual line; that
/// line is silently skipped. Comments and blank lines are skipped.
pub fn parse(text: &str) -> Vec<Sample<'_>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Separate the `metric{labels}` head from the trailing `value [ts]`.
        // A label value may itself contain a space (e.g. action="save note"),
        // so when labels are present the head ends at the LAST `}` — splitting
        // on the last space (rsplit_once) would slice inside the braces.
        let (head, rest) = if let Some(close) = line.rfind('}') {
            (&line[..=close], &line[close + 1..])
        } else {
            match line.split_once(char::is_whitespace) {
                Some(pair) => pair,
                None => continue,
            }
        };
        let value_str = match rest.split_whitespace().next() {
            Some(v) => v,
            None => continue,
        };
        let value: f64 = match value_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let (metric, labels) = if let Some(brace_open) = head.find('{') {
            // head ends at the closing brace by construction above.
            let brace_close = head.len() - 1;
            let metric = &head[..brace_open];
            let labels_str = &head[brace_open + 1..brace_close];
            let labels = parse_labels(labels_str);
            (metric, labels)
        } else {
            (head, HashMap::new())
        };
        out.push(Sample {
            metric,
            labels,
            value,
        });
    }
    out
}

fn parse_labels(s: &str) -> HashMap<&str, &str> {
    let mut out = HashMap::new();
    for pair in s.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        let v = v.trim().trim_start_matches('"').trim_end_matches('"');
        out.insert(k.trim(), v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unlabeled_metric() {
        let text = "akashic_build_info 1\n";
        let samples = parse(text);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].metric, "akashic_build_info");
        assert_eq!(samples[0].value, 1.0);
        assert!(samples[0].labels.is_empty());
    }

    #[test]
    fn parses_single_label() {
        let text = r#"akashic_audit_log_writes_total{action="save_note"} 23"#;
        let samples = parse(text);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].metric, "akashic_audit_log_writes_total");
        assert_eq!(samples[0].labels.get("action"), Some(&"save_note"));
        assert_eq!(samples[0].value, 23.0);
    }

    #[test]
    fn parses_multiple_labels() {
        let text = r#"akashic_http_requests_total{route="/api",method="GET",status="500"} 7"#;
        let samples = parse(text);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.labels.get("route"), Some(&"/api"));
        assert_eq!(s.labels.get("method"), Some(&"GET"));
        assert_eq!(s.labels.get("status"), Some(&"500"));
        assert_eq!(s.value, 7.0);
    }

    #[test]
    fn label_value_with_space_does_not_corrupt_value() {
        let text = r#"akashic_audit_log_writes_total{action="save note"} 30"#;
        let samples = parse(text);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].metric, "akashic_audit_log_writes_total");
        assert_eq!(samples[0].labels.get("action"), Some(&"save note"));
        assert_eq!(samples[0].value, 30.0);
    }

    #[test]
    fn ignores_trailing_timestamp() {
        let text = "akashic_foo 5 1700000000";
        let samples = parse(text);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].value, 5.0);
    }

    #[test]
    fn skips_help_and_type_comments() {
        let text = "# HELP akashic_foo bar\n# TYPE akashic_foo counter\nakashic_foo 5\n";
        let samples = parse(text);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].metric, "akashic_foo");
        assert_eq!(samples[0].value, 5.0);
    }

    #[test]
    fn skips_malformed_lines() {
        let text = "akashic_ok 1\nnot a metric\nakashic_alsook 2\n";
        let samples = parse(text);
        assert_eq!(samples.len(), 2);
    }

    #[test]
    fn parses_multi_line_mixed() {
        let text =
            "# HELP foo bar\n\nakashic_a 1\nakashic_b{x=\"1\"} 2\nakashic_c{x=\"1\",y=\"2\"} 3\n";
        let samples = parse(text);
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[2].labels.len(), 2);
    }
}
