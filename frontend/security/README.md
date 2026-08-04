# Frontend Security — Dependency Posture

This directory holds the frontend dependency-audit policy. The mechanism is
documented below.

## Running the audit gate locally

```bash
cd frontend
npm ci
npm run audit
```

The gate exits 0 when the production dependency tree is clean (or only
contains advisories that are allowlisted via this directory's
`risk-acceptance.yml`). It exits 1 on any blocking issue.

## Threshold rules

The gate at `frontend/scripts/audit-gate.mjs` blocks on:

1. Any advisory of severity `high` or `critical`.
2. Any `moderate` advisory whose CWE list contains one of:
   - `CWE-79` (Cross-Site Scripting),
   - `CWE-918` (SSRF),
   - `CWE-1321` (Prototype Pollution),
   - `CWE-287` / `CWE-863` (auth issues).
3. Any `moderate` advisory not covered above that does **not** have a
   matching, non-expired entry in `risk-acceptance.yml`.

Severities `low` and `info` are not gated.

## Adding a risk-acceptance entry

Add a new entry to `risk-acceptance.yml`. All twelve fields are required;
the gate validates the schema. The entry shape is documented in the file's
header comment.

Default expiry is **6 months** from the day the entry is added. Hard-expiry
uses a strict greater-than comparison (`today > expires`): an entry whose
`expires` equals today is still valid, and the gate begins to fail the day
**after** the recorded expiry. To renew, update `accepted_on`,
update `expires`, and add a fresh paragraph to `rationale` explaining what
changed (or what stayed the same, and why that's still acceptable).

Do **not** allowlist `high` advisories or `moderate` advisories with
blocking CWEs — those must be cleared by upgrade or replacement. The gate
will reject such entries even if added.

## Why a custom gate (not `audit-ci`, etc.)

The threshold is category-aware: we want to block all `high` plus the
`moderate` advisories whose CWEs touch user content (XSS, prototype
pollution, SSRF) or authentication. Off-the-shelf tools express either
"all moderates" or "no moderates"; ours blocks the moderates that matter
and surfaces the rest as documented warnings. See sub-spec §4 R-7 for the
drift-management plan.

## Stale entries

If `npm audit` no longer reports an advisory but `risk-acceptance.yml`
still has its GHSA, the gate prints a non-fatal warning. Remove the entry
in your next PR. The gate does not fail on stale entries — failing on good
news is a footgun.
