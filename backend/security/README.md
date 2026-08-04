# Backend Security — Dependency Posture

This directory holds the backend dependency-audit policy.

## Running the audit gate locally

```bash
cd backend
# First time only:
cargo install --locked cargo-audit
cd scripts && npm ci && cd ..

# Every time:
cargo audit --json | node scripts/audit-gate-rs.mjs
```

The gate exits 0 when the dependency tree is clean (or only contains advisories that are allowlisted via this directory's `risk-acceptance.yml`, plus unmaintained/unsound warn-only kinds). It exits 1 on any blocking issue.

## Threshold rules

The gate at `backend/scripts/audit-gate-rs.mjs` blocks on:

1. Any `kind: vulnerability` advisory of severity `high` or `critical`. **Never overridable by a ledger entry** — the gate rejects any such attempt.
2. Any `kind: vulnerability` advisory of severity `medium` whose RustSec `categories[]` contains one of:
   - `crypto-failure`,
   - `code-execution`,
   - `memory-corruption`,
   - `privilege-escalation`.
3. Any `kind: vulnerability` advisory of severity `medium` without a matching, non-expired ledger entry.

Severities `low` and unset, plus all `unmaintained` / `unsound` / `notice` / `yanked` kinds, are warn-only and not gated.

**Ledger entries for warn-only kinds have no effect.** A `risk-acceptance.yml` entry whose `rustsec` ID matches an `unmaintained` / `unsound` / `notice` / `yanked` advisory does NOT suppress the warning — the warning still fires on every gate run. This is deliberate (sub-spec §2 AC-2 rule 5): warn-only advisories are non-blocking, so suppression has no operational value, and allowing it would invite ledger-spam against unmaintained warnings instead of pursuing the real fix (replacement or upstream upgrade). Pursue the upstream fix; do not add a ledger entry for an unmaintained crate.

## Adding a risk-acceptance entry

Add a new entry to `risk-acceptance.yml`. The 13 base fields are required for every entry; the gate validates the schema. The entry shape is documented in the file's header comment.

Default expiry is **6 months**. Hard-expiry uses a strict greater-than comparison (`today > expires`): an entry whose `expires` equals today is still valid; the gate begins to fail the day **after** the recorded expiry. To renew, update `accepted_on`, update `expires`, and add a fresh paragraph to `rationale` explaining what changed (or what stayed the same, and why that's still acceptable).

Note: quote `accepted_on` and `expires` values as YAML strings (e.g. `"2026-04-28"`). Bare `YYYY-MM-DD` literals are parsed by js-yaml as JavaScript `Date` objects, which fail the schema validator's `YYYY-MM-DD` string check.

## When to use the unreachability override hatch

Two additional fields, `unreachable_justification` and `verified_unreachable_by`, are required IFF the entry is intended to override a rule-2 block (medium severity + blocking category). The hatch is narrow on purpose:

- **Use it when** the vulnerable code is compiled into the binary but never reachable at runtime — e.g. an unused sqlx driver family that gets pulled in by sqlx-macros for compile-time query checking but is never instantiated. The current ledger's only entry (`RUSTSEC-2023-0071` for rsa via sqlx-mysql) is the canonical example.
- **Do not use it** for "I'll fix this later" or "this is probably fine". Those are rule 4 (needs-ledger) cases for non-blocking-category mediums; they don't need the override hatch.
- **`unreachable_justification`** must articulate *why* the code is unreachable in prose, citing the structural reason (e.g. "sqlx-macros pulls all driver crates; the backend never opens a MySql connection").
- **`verified_unreachable_by`** must contain an exact runnable command (typically a `rg`/`grep` invocation against `backend/src`) and the captured zero-match output proving the claim. The gate does not run the command; the plan's acceptance-evidence task does.

The override hatch never applies to `high`/`critical` advisories. Those must be cleared by upgrade or by removing the dep.

## Stale entries

If `cargo audit` no longer reports an advisory but `risk-acceptance.yml` still has its RUSTSEC ID, the gate prints a non-fatal warning. Remove the entry in your next PR. The gate does not fail on stale entries.

## Why a custom gate (not `cargo deny`, etc.)

The threshold is category-aware against RustSec's vocabulary, parallels A4's frontend gate one-for-one, and enforces the 13-field ledger schema with the override-hatch semantics. `cargo deny` covers more ground (license / banned-crate / source-allowlist policy) but its ignore-list lacks the expiry / review-action discipline this ledger enforces, and license/banned-crate policy is out of scope for A5 (deferred to D1). See sub-spec §1 closing list.

## Why Node powers the Rust gate

Pragmatic symmetry with the frontend gate (`frontend/scripts/audit-gate.mjs`). Both ledgers share a parser, a validator, and a rule shape; operators learn one mental model. The Node runtime addition to the CI image is documented tech debt (sub-spec §4 R-11) and will be revisited in D1.
