# Contributing to Akashic Record

Thanks for your interest in contributing! This document covers how to set up
the project, the checks your change must pass, and how to submit it.

By participating you agree to abide by our [Code of Conduct](CODE_OF_CONDUCT.md).

## Project layout

Akashic Record is two services in one repository (see [README.md](README.md)
for the full architecture):

- **`backend/`** — a Rust (edition 2024) Cargo workspace of `crates/*`, following
  a hexagonal architecture: `akashic-domain` holds the ports/traits and pure
  algorithms; `akashic-store-pg` / `akashic-store-neo4j` are the database
  adapters; `akashic-retrieval` / `akashic-ingestion` / `akashic-curation` hold
  the services; `akashic-mcp` / `akashic-http` are the interface layers;
  `akashic-server` is a thin composition root.
- **`frontend/`** — a SvelteKit (Svelte 5) single-page app.

## Development setup

Full setup — the PostgreSQL 16 + pgvector and Neo4j 5 dev stack, environment
variables, and how to run each service — is documented in [README.md](README.md).
Copy [`.env.example`](.env.example) to `.env` and fill it in; never commit a real
`.env` or any credential.

## Before you open a pull request

### Backend (Rust)

Run all three from `backend/` and make sure each is clean:

```bash
cargo fmt --all --check          # formatting (run `cargo fmt` to fix)
cargo clippy --release --all-targets   # must be 0 warnings
cargo test -p <crate>            # test the crate(s) you changed
```

If you change dependencies (`cargo add`, `cargo update`, or a `Cargo.toml`
edit), also run the dependency-advisory gate:

```bash
cargo audit --json | node scripts/audit-gate-rs.mjs
```

See [`backend/security/README.md`](backend/security/README.md) for the gate's
threshold rules and the (narrow) override hatch.

### Frontend (Svelte)

From `frontend/`, run the project's `check`, `lint`, and `test` scripts (see
`package.json`) and make sure the build passes.

## Commit conventions

- Use clear, conventional-style commit subjects (`feat:`, `fix:`, `docs:`,
  `test:`, `refactor:`, `chore:`).
- Sign off every commit (DCO): `git commit -s`, which appends
  `Signed-off-by: Your Name <you@example.com>`.
- Keep commits focused; explain the *why* in the body when it isn't obvious.

## Pull requests

1. Fork and branch from `master`.
2. Make your change with tests where behaviour changes.
3. Ensure the checks above pass.
4. Open a PR describing the change and its motivation. Link any related issue.

Small, well-scoped PRs are reviewed fastest. If you're planning a large change,
open an issue first to discuss the approach.

## Reporting bugs and requesting features

Open an issue with a clear title, reproduction steps (for bugs), and the
behaviour you expected. For security issues, do **not** open a public issue —
see [SECURITY.md](SECURITY.md).
