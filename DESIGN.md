# Design

Visual system of Akashic Record. Canonical tokens live in
`frontend/src/app.css` (`@theme inline` — the ONLY place colors are
defined; semantic vars are aliases). Dark-only via `<html class="dark">`.

## Theme

Dark mode only. Deep navy backgrounds — never pure black, never light.
The interface is a dark chamber; luminosity is reserved for content and
importance (glow), not surfaces.

## Colors

| Token | Value | Usage |
|-------|-------|-------|
| `--color-background` | `hsl(220 30% 4%)` | Page background (deep navy) |
| `--color-foreground` | `hsl(215 20% 89%)` | Body text |
| `--color-card` | `hsl(220 25% 7%)` | Card/panel backgrounds |
| `--color-sidebar` | `hsl(220 28% 6%)` | Sidebar layer |
| `--color-secondary` | `hsl(220 20% 12%)` | Second neutral layer |
| `--color-muted` | `hsl(220 15% 15%)` | Muted surfaces |
| `--color-muted-foreground` | `hsl(215 13% 45%)` | Secondary text (small/labels only — not long prose) |
| `--color-primary` / `--color-accent` / `--color-ring` | `hsl(217 91% 60%)` (#3B82F6) | Active states, focus, links, central glow |
| `--color-border` | `hsl(220 15% 10%)` | Borders (very subtle) |
| `--color-destructive` | `hsl(0 63% 31%)` | Destructive actions |

Category colors (badges/glows): ARCHITECTURE blue-400/500 · BUG_FIX
red-400/500 · CONFIG amber-400/500 · ONBOARDING green-400/500 ·
DECISION purple-400/500.

Strategy: **Restrained** — tinted navy neutrals + one blue accent; category
colors are semantic, not decorative. Amber glow is reserved for human
knowledge (notes).

## Typography

- **Inter Variable** for UI (`--font-sans`, with PingFang TC / Microsoft
  JhengHei UI fallbacks for zh-TW).
- **JetBrains Mono Variable** for code, symbols, file paths, tags,
  metadata — monospace is a first-class citizen.

Fixed rem/px scale (product register — no fluid type):

| Size | Usage |
|------|-------|
| 10px | Micro labels (legend, timestamps) |
| 11px | Small labels (badges, file paths) |
| 12px | Metadata (facts, secondary info) |
| 13px | Default component text (notes, sidebar) |
| 14px (`text-sm`) | UI text (buttons, inputs) |
| `text-lg`+ | Landing page title only |

Prose (docs reading, note bodies): cap at 65–75ch line length.

## Components

- shadcn-svelte (new-york) primitives under `frontend/src/lib/components/ui/`,
  themed entirely through the token aliases — no per-component colors.
- Glass morphism: blur 32px, ~45% card opacity — **only** for overlays and
  layered panels (sidebar over graph, modal over page). Never on inline
  elements (badges, buttons).
- Glow: subtle blue box-shadow/filters on primary interactive elements and
  graph nodes; amber for note surfaces. Sparingly.
- Radius scale from `--radius: 0.625rem` (sm→4xl derived).
- Every interactive component ships default/hover/focus/active/disabled/
  loading/error states; skeletons over spinners for content loading.

## Layout

- Dense by principle: compact sidebars, tight tables, information-first.
- App shell: left sidebar navigation + content; graph surfaces go
  full-bleed. Responsive behavior is structural (collapse panels), not
  fluid type.

## Motion

- GSAP for orchestrated moments (landing, graph transitions); CSS
  transitions 150–250ms ease-out for product state changes.
- Motion conveys state — no decorative loops in Work-mode surfaces;
  Exploration surfaces (landing 3D, graph) may breathe.
- `prefers-reduced-motion: reduce` always provides crossfade/instant
  alternatives.
