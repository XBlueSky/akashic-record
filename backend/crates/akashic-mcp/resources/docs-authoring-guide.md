# docs-kit Authoring Guide

This is the human-readable half of the docs-kit corpus contract — the
machine-readable half is `get_docs_schema` (manifest / docs.toml JSON
Schema) and `check_docs_coverage` (the same `parse_nav` + `check_links`
validator the platform's own ingest path runs). Read the section(s) whose
`when` matches what you're doing via `list_authoring_sections`, then fetch
the body with `get_authoring_guide(section)`.

<!-- section: overview | when: read this first to understand the corpus contract at a glance -->
## Overview

A docs-kit corpus is a tree of markdown pages plus one index page. The
index page has a `## All pages` section listing every other page, grouped
under `### ` group headings, as `- [Title](relative/path.md) — one-line
description.`. That nav section is the ONLY thing that makes a page
reachable — a page that exists but isn't linked from `## All pages` is an
`orphan_page` finding, not silently ignored.

Every relative link (`[text](path.md)` or `[text](path.md#anchor)`) and
every heading-derived anchor is validated against the corpus's own files —
`check_docs_coverage` runs the exact same check the platform runs on
publish, so a corpus that passes it locally will pass on ingest too.

<!-- section: page-writing | when: writing a new self-contained docs page -->
## Writing self-contained pages

Each page should make sense read on its own — a reader may land on it
directly from search, not by walking the nav tree from the index. Concretely:

- Start with a single `#` H1 title.
- State what the page covers in the first paragraph before any subsection.
- Don't assume the reader already read a "previous" page in the nav
  ordering — link to prerequisite pages explicitly instead of implying
  they were already read.
- Prefer one topic per page over one giant page with many unrelated `##`
  sections; the corpus's nav tree is the place for cross-page structure.

<!-- section: bilingual | when: writing or maintaining a page that ships in more than one language -->
## Bilingual pages

If a corpus's `manifest.languages` lists more than one language, every page
carries a `**Language:** <code>` line directly under the H1 title (e.g.
`**Language:** en`, `**Language:** zh-TW`) — this is how the platform and
the authoring tools tell which language a given file is, since the
`.akashic/docs.toml` `languages` list is corpus-wide, not per-file.

Identifiers stay untranslated in every language: function/type/config
names, CLI flags, file paths, and error strings are copied verbatim, not
transliterated or translated — a reader searching for the literal
identifier must find it regardless of which language variant they're
reading.

<!-- section: tables-for-parity | when: documenting a set of options, flags, endpoints, or fields that should stay easy to diff across languages -->
## Tables for parity

For anything shaped like a list of (name, type, description) — config
keys, CLI flags, API fields, error codes — prefer a markdown table over
prose. Tables are what let translators (human or automated) and reviewers
diff two language variants of the same page column-by-column and catch a
missing/renamed row immediately, which free-form prose does not.

```markdown
| Flag | Type | Description |
| --- | --- | --- |
| `--verbose` | bool | Print debug-level logging. |
```

<!-- section: snippets | when: including runnable code examples that should stay in sync with real source -->
## Snippets

A `.akashic/docs.toml` may declare a `[snippets]` section (not modeled by
the platform's `DocsToml` schema — it tolerates but does not interpret it;
see `get_docs_schema("docs-toml")`) that the kit's own tooling uses to pull
fenced code blocks from real source files rather than hand-typing them, so
an example can't silently drift from the code it's demonstrating. Fenced
code blocks are also exempt from link-scanning — a C++ lambda capture
`[](args){...}` inside a ```cpp fence, or a `[x](y)`-shaped inline code
span, is never mistaken for a markdown link by `check_docs_coverage`, so
snippet content never needs escaping to avoid false `broken_link` findings.

<!-- section: nav-index | when: adding, removing, or reorganizing pages in the corpus's index / nav tree -->
## The nav index

The index page's `## All pages` section is the corpus's only source of
navigational structure:

```markdown
## All pages

### Guide

- [Setup](guide/setup.md) — Install and configure the tool.
- [Usage](guide/usage.md) — Day-to-day usage patterns.
```

Each `### ` heading is a group; each `- [Title](path)` line under it is a
page. Adding a new page requires adding its line here — a page that exists
in the tree but isn't listed becomes an `orphan_page` (warn-severity)
finding, not a silent gap. Removing a page means removing both the file
and its nav line; leaving the nav line produces a `broken_link` finding
from every page (including the index) that still points at it.

<!-- section: assets | when: adding images, diagrams, or other non-markdown files to a corpus page -->
## Assets

Non-markdown files (images, diagrams) are ingested alongside markdown pages
using the same relative-path convention as a page-to-page link — reference
them exactly like a markdown link (`![alt](../assets/diagram.png)`). They
are not link-scanned for anchors (`check_docs_coverage`'s anchor check only
applies when the target resolves to a page already loaded as markdown
content), but the path itself is still checked against the corpus's file
set — an asset reference to a file that isn't part of the corpus produces
the same `broken_link` finding a missing page would.
