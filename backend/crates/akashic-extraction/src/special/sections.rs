use regex::Regex;

/// A hierarchical section parsed from markdown headings.
#[derive(Debug, Clone)]
pub struct RawSection {
    pub heading: String,
    pub content: String,
    pub depth: u16,    // 0-based: 0=H1, 1=H2, 2=H3...
    pub position: u16, // order within same parent
    pub children: Vec<RawSection>,
    pub tags: Vec<String>,
}

/// A flat heading extracted from the markdown source before tree construction.
struct FlatHeading {
    heading: String,
    content: String,
    level: u16, // 1-based: 1=H1, 2=H2, etc.
}

/// Parse markdown into a hierarchical section tree based on headings.
///
/// Each heading (`#` through `######`) becomes a section node. Content between
/// headings belongs to the preceding heading. Sub-headings nest as children.
/// If the source contains no headings, the entire document is returned as a
/// single section with heading "Content".
pub fn parse_markdown_sections(source: &str) -> Vec<RawSection> {
    let heading_re = Regex::new(r"(?m)^(#{1,6})\s+(.+)$").unwrap();

    // Collect heading positions
    let mut headings: Vec<(usize, u16, String)> = Vec::new();
    for cap in heading_re.captures_iter(source) {
        let m = cap.get(0).unwrap();
        let level = cap[1].len() as u16;
        let title = cap[2].trim().to_string();
        headings.push((m.start(), level, title));
    }

    if headings.is_empty() {
        let content = source.trim().to_string();
        let tags = extract_tags(&content);
        return vec![RawSection {
            heading: "Content".to_string(),
            content,
            depth: 0,
            position: 0,
            children: Vec::new(),
            tags,
        }];
    }

    // Build flat list with content slices
    let mut flat: Vec<FlatHeading> = Vec::new();
    for (i, (start, level, title)) in headings.iter().enumerate() {
        // Content starts after the heading line
        let content_start = source[*start..]
            .find('\n')
            .map_or(source.len(), |p| start + p + 1);
        let content_end = if i + 1 < headings.len() {
            headings[i + 1].0
        } else {
            source.len()
        };
        let content = source[content_start..content_end].trim().to_string();
        flat.push(FlatHeading {
            heading: title.clone(),
            content,
            level: *level,
        });
    }

    build_tree(&flat, 0, flat.len())
}

/// Recursively build a tree of `RawSection` from a flat slice of headings.
///
/// Processes `flat[start..end]`. The first heading's level defines the
/// "current" level; consecutive headings at the same level become siblings,
/// and deeper headings become children of the preceding sibling.
fn build_tree(flat: &[FlatHeading], start: usize, end: usize) -> Vec<RawSection> {
    if start >= end {
        return Vec::new();
    }

    let top_level = flat[start].level;
    let mut sections: Vec<RawSection> = Vec::new();
    let mut i = start;

    while i < end {
        let h = &flat[i];
        if h.level < top_level {
            // Shouldn't happen within a well-formed slice; stop.
            break;
        }
        if h.level > top_level {
            // Deeper heading encountered before any same-level heading —
            // treat it as a new top level within this slice.
            break;
        }

        // Find the range of children (everything deeper until next same-or-higher level)
        let child_start = i + 1;
        let mut child_end = child_start;
        while child_end < end && flat[child_end].level > top_level {
            child_end += 1;
        }

        let children = build_tree(flat, child_start, child_end);
        let tags = extract_tags(&h.content);

        sections.push(RawSection {
            heading: h.heading.clone(),
            content: h.content.clone(),
            depth: h.level - 1, // 0-based
            position: sections.len() as u16,
            children,
            tags,
        });

        i = child_end;
    }

    sections
}

/// Extract inline code references from content as tags.
///
/// Matches backtick-enclosed identifiers (including `::` paths like
/// `std::io::Read`). Results are lowercased, deduplicated, and limited to 10.
pub fn extract_tags(content: &str) -> Vec<String> {
    let re = Regex::new(r"`([a-zA-Z_]\w*(?:::\w+)*)`").unwrap();
    let mut tags: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for cap in re.captures_iter(content) {
        let tag = cap[1].to_lowercase();
        if seen.insert(tag.clone()) {
            tags.push(tag);
            if tags.len() >= 10 {
                break;
            }
        }
    }

    tags
}
