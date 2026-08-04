#!/usr/bin/env node
// backend/scripts/audit-gate-rs.mjs
//
// Category-aware cargo audit gate. Reads `cargo audit --json` on stdin,
// reads backend/security/risk-acceptance.yml as the allowlist, and exits 0 or 1
// per the threshold rules documented in backend/security/README.md.

import { readFileSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, resolve } from 'node:path';
import yaml from 'js-yaml';

const BLOCKING_CATEGORIES = new Set(['crypto-failure', 'code-execution', 'memory-corruption', 'privilege-escalation']);
const HARD_BLOCK_SEVERITIES = new Set(['high', 'critical']);
const WARN_ONLY_KINDS = new Set(['unmaintained', 'unsound', 'notice', 'yanked']);

const RUSTSEC_RE = /^RUSTSEC-\d{4}-\d{4}$/;
const DATE_RE = /^\d{4}-\d{2}-\d{2}$/;

const LEDGER_REQUIRED_FIELDS = [
  'rustsec', 'package', 'via', 'severity', 'categories', 'kind',
  'category_label', 'rationale', 'mitigation', 'accepted_by',
  'accepted_on', 'expires', 'review_action',
];

/**
 * Flatten cargo audit's --json output into a flat list of advisory records.
 * Each record carries: { id, name, severity, categories[], kind, url, title }.
 *
 * cargo audit --json shape:
 *   {
 *     "vulnerabilities": { "list": [ {advisory: {id, package, severity, categories[], url, title}, ...} ] },
 *     "warnings": { "unmaintained": [{advisory: {...}}], "unsound": [...], "notice": [...], "yanked": [...] }
 *   }
 *
 * Severities are normalized to lowercase. Absent severity on a vulnerability is
 * defaulted to 'medium' (defensive); a warning is emitted in that case.
 */
export function flattenCargoAuditJson(auditJson) {
  const out = [];
  const seen = new Set();
  const defaulted = [];

  function push(rec, kind) {
    const adv = rec?.advisory ?? rec;
    if (!adv || !adv.id) return;
    if (seen.has(adv.id + '|' + kind)) return;
    seen.add(adv.id + '|' + kind);

    let severity = adv.severity ? String(adv.severity).toLowerCase() : null;
    if (kind === 'vulnerability' && !severity) {
      severity = 'medium';
      defaulted.push(adv.id);
    }
    out.push({
      id: adv.id,
      name: adv.package ?? rec.package ?? 'unknown',
      severity,
      categories: Array.isArray(adv.categories) ? adv.categories : [],
      kind,
      url: adv.url ?? `https://rustsec.org/advisories/${adv.id}`,
      title: adv.title ?? '',
    });
  }

  // Vulnerabilities
  const vlist = auditJson?.vulnerabilities?.list ?? [];
  for (const v of vlist) push(v, 'vulnerability');

  // Warnings (per-kind buckets)
  const warnings = auditJson?.warnings ?? {};
  for (const k of ['unmaintained', 'unsound', 'notice', 'yanked']) {
    const arr = warnings[k] ?? [];
    for (const w of arr) push(w, k);
  }

  return { advisories: out, defaultedSeverity: defaulted };
}

/**
 * Validate ledger entry shape. Returns array of error strings (empty = valid).
 *
 * unreachable_justification + verified_unreachable_by are conditionally required:
 * if either is non-empty, the other must also be non-empty (override is a pair).
 */
function validateLedgerEntry(entry, idx) {
  const errs = [];
  for (const f of LEDGER_REQUIRED_FIELDS) {
    if (!(f in entry)) errs.push(`entry[${idx}]: missing field '${f}'`);
  }
  if (entry.rustsec && !RUSTSEC_RE.test(entry.rustsec)) errs.push(`entry[${idx}]: rustsec '${entry.rustsec}' does not match RUSTSEC pattern`);
  if (entry.accepted_on && !DATE_RE.test(entry.accepted_on)) errs.push(`entry[${idx}]: accepted_on must be YYYY-MM-DD`);
  if (entry.expires && !DATE_RE.test(entry.expires)) errs.push(`entry[${idx}]: expires must be YYYY-MM-DD`);
  if (entry.categories !== undefined && !Array.isArray(entry.categories)) errs.push(`entry[${idx}]: categories must be an array`);

  const uj = (entry.unreachable_justification ?? '').trim();
  const vu = (entry.verified_unreachable_by ?? '').trim();
  if ((uj && !vu) || (!uj && vu)) {
    errs.push(`entry[${idx}]: unreachable_justification and verified_unreachable_by must both be set or both empty`);
  }
  return errs;
}

/**
 * Pure rule evaluator. See sub-spec §2 AC-2.
 *
 * @param {Array} advisories - flattened advisory records
 * @param {Array} ledger - parsed risk-acceptance.yml entries
 * @param {string} todayISO - YYYY-MM-DD; used for expiry comparison
 * @returns {{exitCode:number, blocking:Array, warnings:Array}}
 */
export function evaluateAudit(advisories, ledger, todayISO) {
  const blocking = [];
  const warnings = [];

  // Validate ledger shape first.
  ledger.forEach((e, i) => {
    const errs = validateLedgerEntry(e, i);
    errs.forEach(msg => blocking.push({ reason: `ledger schema: ${msg}`, advisory: null }));
  });

  // Index ledger by rustsec ID.
  const ledgerById = new Map();
  for (const e of ledger) if (e.rustsec) ledgerById.set(e.rustsec, e);

  // Stale-entry warnings.
  const liveIds = new Set(advisories.map(a => a.id));
  for (const e of ledger) {
    if (e.rustsec && !liveIds.has(e.rustsec)) {
      warnings.push(`ledger entry refers to ${e.rustsec} (${e.package}) which is no longer flagged by cargo audit — consider removing`);
    }
  }

  for (const adv of advisories) {
    // Rule 5: warn-only kinds bypass all gating.
    if (WARN_ONLY_KINDS.has(adv.kind)) {
      warnings.push(`${adv.kind}: ${adv.id} (${adv.name}) — ${adv.url}`);
      continue;
    }

    // Only `kind: vulnerability` reaches here. Severity dispatch:
    if (adv.severity === 'low' || adv.severity == null) {
      // low or unset (after the flatten default) — not gated.
      warnings.push(`low/unset-severity vulnerability ${adv.id} (${adv.name}) — not gated`);
      continue;
    }

    // Rule 1: high/critical hard-block. NEVER overridable.
    if (HARD_BLOCK_SEVERITIES.has(adv.severity)) {
      const ledgerEntry = ledgerById.get(adv.id);
      if (ledgerEntry && (ledgerEntry.unreachable_justification ?? '').trim()) {
        blocking.push({
          reason: `ledger entry attempts to allowlist a ${adv.severity}-severity advisory ${adv.id} — rejected (rule 1 is non-overridable)`,
          advisory: adv,
        });
      } else {
        blocking.push({
          reason: `${adv.severity} severity advisory: ${adv.id} (${adv.name}) — ${adv.url}`,
          advisory: adv,
        });
      }
      continue;
    }

    // Severity is 'medium' from here.
    const catHits = (adv.categories ?? []).filter(c => BLOCKING_CATEGORIES.has(c));
    const ledgerEntry = ledgerById.get(adv.id);

    if (catHits.length > 0) {
      // Rule 2: medium + blocking-category. May be overridden via rule 3-bis if
      // the ledger entry carries non-empty unreachable_justification AND
      // verified_unreachable_by.
      if (ledgerEntry) {
        // Validate the ledger entry's basic shape against the live advisory
        // before considering an override.
        if (ledgerEntry.expires < todayISO) {
          blocking.push({
            reason: `ledger entry for ${adv.id} expired on ${ledgerEntry.expires} (today ${todayISO})`,
            advisory: adv,
          });
          continue;
        }
        if (ledgerEntry.severity !== adv.severity || ledgerEntry.package !== adv.name) {
          blocking.push({
            reason: `ledger entry for ${adv.id} has stale-shape (severity/package mismatch with live advisory)`,
            advisory: adv,
          });
          continue;
        }
        const uj = (ledgerEntry.unreachable_justification ?? '').trim();
        const vu = (ledgerEntry.verified_unreachable_by ?? '').trim();
        if (!uj || !vu) {
          blocking.push({
            reason: `ledger entry for ${adv.id} attempts to override blocking-category advisory but lacks unreachable justification (rule 3-bis requires both unreachable_justification and verified_unreachable_by)`,
            advisory: adv,
          });
          continue;
        }
        // Rule 3-bis: allowed.
        warnings.push(`medium+blocking-category advisory ${adv.id} (${adv.name}) allowlisted via ledger unreachable-override; expires ${ledgerEntry.expires}`);
        continue;
      }
      blocking.push({
        reason: `medium advisory with blocking category ${catHits.join(',')}: ${adv.id} (${adv.name}) — ${adv.url}`,
        advisory: adv,
      });
      continue;
    }

    // Severity 'medium', non-blocking categories. Rule 3 (ledger lookup) or rule 4 (block needs-ledger).
    if (ledgerEntry) {
      if (ledgerEntry.expires < todayISO) {
        blocking.push({
          reason: `ledger entry for ${adv.id} expired on ${ledgerEntry.expires} (today ${todayISO})`,
          advisory: adv,
        });
        continue;
      }
      if (ledgerEntry.severity !== adv.severity || ledgerEntry.package !== adv.name) {
        blocking.push({
          reason: `ledger entry for ${adv.id} has stale-shape (severity/package mismatch with live advisory)`,
          advisory: adv,
        });
        continue;
      }
      warnings.push(`medium advisory ${adv.id} (${adv.name}) allowlisted via ledger; expires ${ledgerEntry.expires}`);
      continue;
    }

    // Rule 4: medium with no ledger entry → block.
    blocking.push({
      reason: `medium advisory ${adv.id} (${adv.name}) needs upgrade or risk-acceptance entry — ${adv.url}`,
      advisory: adv,
    });
  }

  return {
    exitCode: blocking.length > 0 ? 1 : 0,
    blocking,
    warnings,
  };
}

/**
 * CLI entry: read stdin (cargo audit JSON), read ledger, evaluate, print, exit.
 */
async function main() {
  const here = dirname(fileURLToPath(import.meta.url));
  const ledgerPath = resolve(here, '..', 'security', 'risk-acceptance.yml');

  let ledger = [];
  try {
    const raw = readFileSync(ledgerPath, 'utf8');
    const parsed = yaml.load(raw);
    if (parsed == null) {
      ledger = [];
    } else if (Array.isArray(parsed)) {
      ledger = parsed;
    } else {
      // A mistyped top-level map (e.g. someone wrote `rustsec: ...` at the
      // root instead of `- rustsec: ...`) silently coerced to [] would let
      // CI go green with an effectively-empty ledger — exactly the
      // failure-mode the gate exists to prevent. Hard-fail instead.
      console.error(`audit-gate-rs: ledger YAML root must be a list (got ${typeof parsed}); see backend/security/README.md for the schema`);
      process.exit(1);
    }
  } catch (e) {
    if (e.code !== 'ENOENT') {
      console.error(`audit-gate-rs: failed to read ledger at ${ledgerPath}: ${e.message}`);
      process.exit(1);
    }
    // Missing ledger is fine; treat as empty.
  }

  const stdin = readFileSync(0, 'utf8');
  let auditJson;
  try {
    auditJson = JSON.parse(stdin);
  } catch (e) {
    console.error(`audit-gate-rs: stdin is not valid JSON: ${e.message}`);
    process.exit(1);
  }

  const { advisories, defaultedSeverity } = flattenCargoAuditJson(auditJson);
  for (const id of defaultedSeverity) {
    console.error(`[audit-gate] WARN: ${id} had no severity in cargo audit JSON; defaulted to 'medium' for gating`);
  }

  const today = new Date().toISOString().slice(0, 10);
  const result = evaluateAudit(advisories, ledger, today);

  for (const w of result.warnings) console.error(`[audit-gate] WARN: ${w}`);
  for (const b of result.blocking) console.error(`[audit-gate] BLOCK: ${b.reason}`);

  if (result.exitCode === 0) {
    console.error(`[audit-gate] OK: ${advisories.length} advisor${advisories.length === 1 ? 'y' : 'ies'} reviewed; ${result.warnings.length} warning(s)`);
  } else {
    console.error(`[audit-gate] FAIL: ${result.blocking.length} blocking issue(s)`);
  }
  process.exit(result.exitCode);
}

// Canonical CLI-entrypoint guard: pathToFileURL(process.argv[1]) handles
// platform-specific path normalization (Windows drive letters, percent-
// encoding) that a naive `file://${process.argv[1]}` string-concat misses.
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
