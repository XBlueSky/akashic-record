# Akashic Record — Project Guidelines

## Design Context

### Brand Personality
**Mystical, Vast, Intelligent** — A living knowledge palace, not a static documentation site.

### Two Emotional Modes
- **Exploration** (Landing, Graph): Awe + curiosity. "I'm exploring a vast knowledge palace."
- **Work** (Notes, Sidebar, Detail): Control + efficiency. Dense, precise, fast.

### Design Principles
1. **Information density over decorative whitespace.** Every pixel earns its place.
2. **Monospace is a design choice, not a fallback.** JetBrains Mono for all technical content.
3. **Glow means importance.** Blue glow = primary. Amber glow = human knowledge. Don't overuse.
4. **Glass is for layered depth, not decoration.** Only on panels/overlays, never inline elements.
5. **The graph IS the interface.** Everything else serves the constellation graph.

### Tech Stack
- Svelte 5 (runes: $state, $props, $derived, $effect)
- Tailwind CSS 4.0 + shadcn-svelte (new-york style, zinc base)
- Dark mode only. Deep navy backgrounds, never pure black.
- Inter (UI) + JetBrains Mono (code/technical)
- D3.js for graph, Three.js for landing 3D, GSAP for animations
- Rust (axum) backend + Neo4j + PostgreSQL/pgvector

### Color Reference
- Primary: `#3B82F6` (blue)
- Background: `hsl(220 30% 4%)` (deep navy)
- Categories: blue (ARCHITECTURE), red (BUG_FIX), amber (CONFIG), green (ONBOARDING), purple (DECISION)

See `.impeccable.md` for full design context with typography scale and color system.

## Backend dependency policy

Run `cargo audit --json | node backend/scripts/audit-gate-rs.mjs` after any
`cargo update`, `cargo add`, or `backend/Cargo.toml` edit. See
`backend/security/README.md` for threshold rules, the override hatch, and
the ledger schema. Re-introducing rustls-webpki below 0.103.13 hard-blocks
on four medium+crypto-failure advisories. The override hatch
(unreachable_justification) is narrow — only for the "compiled but
unreachable" case (e.g. unused sqlx driver families) — and never applies
to high/critical advisories.
