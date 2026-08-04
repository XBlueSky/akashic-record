# Frontend — Akashic Record

## Adding a dependency

Run `npm run audit` after any `npm install` of a new package or version bump.
The gate (`scripts/audit-gate.mjs`) blocks `high` advisories and the
moderate XSS / SSRF / prototype-pollution / auth-bypass advisories outright;
other moderates require an entry in `frontend/security/risk-acceptance.yml`.
See `frontend/security/README.md` for the threshold rules and risk-acceptance
schema.

A specific tripwire: re-introducing `dagre` brings back a `lodash@4.17.23`
high-severity advisory. If a future feature genuinely needs DAG layout,
prefer `elkjs` or pursue an upstream-fixed dagre fork; otherwise add a
risk-acceptance entry with isolation rationale.
