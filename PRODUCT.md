# Product

## Register

product

## Users

Platform developers exploring C/C++ codebases through a knowledge graph.
Backend/systems engineers who value precision, density, and code-level
detail. They onboard to unfamiliar repos, understand module dependencies,
review AI-generated knowledge notes, and trace function call chains.

With the docs platform, a second reader joins: **downstream developers
reading library documentation** (library consumers) — often first-time
users of the library, arriving via search or a shared link, reading rendered
docs and jumping into the code graph. Context is always technical — they
read monospace, think in graphs, and want information density over
whitespace.

## Product Purpose

Akashic Record is a self-hosted knowledge-about-code platform: it ingests
repos (AST, call graph, GraphRAG), stores human notes, and serves search
and retrieval to humans (web) and coding agents (MCP). The docs-kit
platform extends it into the canonical rendered documentation site for
internal C++ repos — same corpus serving humans (docs area), agents
(MCP / llms.txt), and owners (publish contract). Success: a downstream
developer answers their question without asking a human; an agent retrieves
docs anchored to code at the same SHA.

## Brand Personality

**Mystical, Vast, Intelligent** — a living knowledge palace, not a static
documentation site. Entering a vast dark chamber where constellations of
code knowledge float in space, connected by luminous threads.

Two emotional modes:
- **Exploration** (Landing, Graph): awe + curiosity — "I'm exploring a
  living knowledge palace."
- **Work** (Notes, Sidebar, Detail, Docs reading): control + efficiency —
  "I can find what I need instantly." Dense, precise, fast; like a
  well-organized terminal or IDE panel.

## Anti-references

- Generic SaaS dashboards with white backgrounds.
- Notion-style minimalism (too flat for the metaphor).
- Overly playful/colorful designs (Figma, Linear — too casual).

## Design Principles

1. **Information density over decorative whitespace.** Every pixel earns
   its place.
2. **Monospace is a design choice, not a fallback.** JetBrains Mono for all
   technical content — it signals precise, machine-readable data.
3. **Glow means importance.** Blue glow = primary. Amber glow = human
   knowledge. Don't overuse.
4. **Glass is for layered depth, not decoration.** Only on panels/overlays,
   never inline elements.
5. **The graph IS the interface.** Everything else serves the constellation
   graph; when in doubt, make the graph bigger.

## Accessibility & Inclusion

- WCAG AA contrast targets within the dark theme (body text ≥4.5:1 against
  deep navy surfaces).
- `prefers-reduced-motion` honored on all animated surfaces (Three.js
  landing, GSAP transitions, graph motion) — crossfade or instant
  alternatives.
- Full keyboard operability for command palette, dropdowns, and navigation.
- Bilingual UI chrome (en / zh-TW) via svelte-i18n; content language is
  corpus-driven.
