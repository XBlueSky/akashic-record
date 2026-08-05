#!/usr/bin/env node
// frontend/scripts/audit-gate.mjs
//
// Category-aware npm audit gate. Reads `npm audit --omit=dev --json` on stdin,
// reads frontend/security/risk-acceptance.yml as the allowlist, and exits 0 or 1
// per the threshold rules documented in frontend/security/README.md.

import { readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dirname, resolve } from "node:path";
import yaml from "js-yaml";

const BLOCKING_CWES = new Set(["CWE-79", "CWE-918", "CWE-1321", "CWE-287", "CWE-863"]);
const BLOCKING_SEVERITIES = new Set(["high", "critical"]);
const GATED_SEVERITIES = new Set(["moderate", "high", "critical"]);

const GHSA_RE = /^GHSA-[a-z0-9]{4}-[a-z0-9]{4}-[a-z0-9]{4}$/;
const DATE_RE = /^\d{4}-\d{2}-\d{2}$/;

const LEDGER_REQUIRED_FIELDS = [
	"ghsa",
	"package",
	"via",
	"severity",
	"cwe",
	"category",
	"rationale",
	"mitigation",
	"accepted_by",
	"accepted_on",
	"expires",
	"review_action",
];

/**
 * Flatten npm audit's nested `vulnerabilities` map into a flat list of
 * advisory records. Each record carries: { ghsa, name, severity, cwe[], url, title }.
 */
export function flattenNpmAuditJson(auditJson) {
	const out = [];
	const vulns = auditJson?.vulnerabilities ?? {};
	const seen = new Set();
	for (const pkgName of Object.keys(vulns)) {
		const node = vulns[pkgName];
		const viaArr = Array.isArray(node.via) ? node.via : [];
		for (const via of viaArr) {
			if (typeof via !== "object" || via === null) continue;
			// npm audit's "via" entries that are advisories carry a `source` and `url`.
			// Transitive references (string entries) are skipped.
			const ghsa = extractGhsaFromUrl(via.url) || via.ghsa || `unknown-${via.source ?? "x"}`;
			if (seen.has(ghsa + "|" + (via.name ?? pkgName))) continue;
			seen.add(ghsa + "|" + (via.name ?? pkgName));
			out.push({
				ghsa,
				name: via.name ?? pkgName,
				severity: via.severity ?? node.severity ?? "unknown",
				cwe: Array.isArray(via.cwe) ? via.cwe : [],
				url: via.url ?? "",
				title: via.title ?? "",
			});
		}
	}
	return out;
}

function extractGhsaFromUrl(url) {
	if (typeof url !== "string") return null;
	const m = url.match(/(GHSA-[a-z0-9]{4}-[a-z0-9]{4}-[a-z0-9]{4})/);
	return m ? m[1] : null;
}

/**
 * Validate ledger entry shape. Returns array of error strings (empty = valid).
 */
function validateLedgerEntry(entry, idx) {
	const errs = [];
	for (const f of LEDGER_REQUIRED_FIELDS) {
		if (!(f in entry)) errs.push(`entry[${idx}]: missing field '${f}'`);
	}
	if (entry.ghsa && !GHSA_RE.test(entry.ghsa))
		errs.push(`entry[${idx}]: ghsa '${entry.ghsa}' does not match GHSA pattern`);
	if (entry.accepted_on && !DATE_RE.test(entry.accepted_on))
		errs.push(`entry[${idx}]: accepted_on must be YYYY-MM-DD`);
	if (entry.expires && !DATE_RE.test(entry.expires))
		errs.push(`entry[${idx}]: expires must be YYYY-MM-DD`);
	if (entry.cwe && !Array.isArray(entry.cwe)) errs.push(`entry[${idx}]: cwe must be an array`);
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
		errs.forEach((msg) => blocking.push({ reason: `ledger schema: ${msg}`, advisory: null }));
	});

	// Index ledger by ghsa.
	const ledgerByGhsa = new Map();
	for (const e of ledger) if (e.ghsa) ledgerByGhsa.set(e.ghsa, e);

	// Stale-entry warnings: ledger ghsa not present in current advisories.
	const liveGhsas = new Set(advisories.map((a) => a.ghsa));
	for (const e of ledger) {
		if (e.ghsa && !liveGhsas.has(e.ghsa)) {
			warnings.push(
				`ledger entry refers to ${e.ghsa} (${e.package}) which is no longer flagged by npm audit — consider removing`,
			);
		}
	}

	for (const adv of advisories) {
		if (!GATED_SEVERITIES.has(adv.severity)) continue;

		// Rule 1: high/critical always blocks. A ledger entry attempting to
		// allowlist a high/critical advisory is itself a configuration error —
		// emit a distinct, more-emphatic blocking reason so reviewers notice
		// the policy violation rather than just the underlying advisory.
		// (Mirrors backend/scripts/audit-gate-rs.mjs rule-1 enforcement; see
		// frontend/security/README.md "the gate will reject such entries".)
		if (BLOCKING_SEVERITIES.has(adv.severity)) {
			const ledgerEntry = ledgerByGhsa.get(adv.ghsa);
			if (ledgerEntry) {
				blocking.push({
					reason: `ledger entry attempts to allowlist a ${adv.severity}-severity advisory ${adv.ghsa} (${adv.name}) — rejected (rule 1 is non-overridable)`,
					advisory: adv,
				});
			} else {
				blocking.push({
					reason: `${adv.severity} severity advisory: ${adv.ghsa} (${adv.name}) — ${adv.url}`,
					advisory: adv,
				});
			}
			continue;
		}

		// adv.severity === 'moderate' from here.
		const cweHits = (adv.cwe ?? []).filter((c) => BLOCKING_CWES.has(c));
		if (cweHits.length > 0) {
			// Rule 2: blocking-CWE moderate is hard-blocked (ledger does NOT exempt).
			// Reject any ledger entry that attempts to allowlist one of these
			// categories with a distinct error, same as rule 1.
			const ledgerEntry = ledgerByGhsa.get(adv.ghsa);
			if (ledgerEntry) {
				blocking.push({
					reason: `ledger entry attempts to allowlist a blocking-CWE moderate advisory ${adv.ghsa} (CWE ${cweHits.join(",")}, ${adv.name}) — rejected (rule 2 is non-overridable)`,
					advisory: adv,
				});
			} else {
				blocking.push({
					reason: `moderate advisory with blocking CWE ${cweHits.join(",")}: ${adv.ghsa} (${adv.name}) — ${adv.url}`,
					advisory: adv,
				});
			}
			continue;
		}

		const ledgerEntry = ledgerByGhsa.get(adv.ghsa);
		if (ledgerEntry) {
			// Rule 3: ledger allows the advisory if the entry is valid and not expired.
			// Boundary semantics (sub-spec AC-4): strict greater-than. An entry whose
			// `expires` equals today is still valid; the gate begins to fail the day
			// AFTER the recorded expiry. Both operands are ISO YYYY-MM-DD strings, so
			// lexicographic compare matches calendar order.
			if (ledgerEntry.expires < todayISO) {
				blocking.push({
					reason: `ledger entry for ${adv.ghsa} expired on ${ledgerEntry.expires} (today ${todayISO})`,
					advisory: adv,
				});
				continue;
			}
			if (ledgerEntry.severity !== adv.severity || ledgerEntry.package !== adv.name) {
				blocking.push({
					reason: `ledger entry for ${adv.ghsa} has stale-shape (severity/package mismatch with live advisory)`,
					advisory: adv,
				});
				continue;
			}
			warnings.push(
				`moderate advisory ${adv.ghsa} (${adv.name}) allowlisted via ledger; expires ${ledgerEntry.expires}`,
			);
			continue;
		}

		// Rule 4: moderate with no ledger entry → block.
		blocking.push({
			reason: `moderate advisory ${adv.ghsa} (${adv.name}) needs upgrade or risk-acceptance entry — ${adv.url}`,
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
 * CLI entry: read stdin (audit JSON), read ledger, evaluate, print, exit.
 */
async function main() {
	const here = dirname(fileURLToPath(import.meta.url));
	const ledgerPath = resolve(here, "..", "security", "risk-acceptance.yml");

	let ledger = [];
	try {
		const raw = readFileSync(ledgerPath, "utf8");
		const parsed = yaml.load(raw);
		if (parsed == null) {
			// Empty file is fine — operators may delete the entries while keeping
			// the file as scaffolding.
			ledger = [];
		} else if (Array.isArray(parsed)) {
			ledger = parsed;
		} else {
			// A mistyped top-level (e.g. `rustsec: ...` instead of `- rustsec: ...`)
			// would silently coerce to [] under a permissive policy, letting CI go
			// green with an effectively-empty ledger and disappearing previously
			// accepted entries. Hard-fail instead. Mirrors the backend gate.
			console.error(
				`audit-gate: ledger YAML root must be a list (got ${typeof parsed} '${Array.isArray(parsed) ? "array" : Object.prototype.toString.call(parsed)}') — see frontend/security/README.md for the schema`,
			);
			process.exit(1);
		}
	} catch (e) {
		if (e.code !== "ENOENT") {
			console.error(`audit-gate: failed to read ledger at ${ledgerPath}: ${e.message}`);
			process.exit(1);
		}
		// Missing ledger is fine; treat as empty.
	}

	const stdin = readFileSync(0, "utf8");
	let auditJson;
	try {
		auditJson = JSON.parse(stdin);
	} catch (e) {
		console.error(`audit-gate: stdin is not valid JSON: ${e.message}`);
		process.exit(1);
	}

	const advisories = flattenNpmAuditJson(auditJson);
	const today = new Date().toISOString().slice(0, 10);
	const result = evaluateAudit(advisories, ledger, today);

	for (const w of result.warnings) console.error(`[audit-gate] WARN: ${w}`);
	for (const b of result.blocking) console.error(`[audit-gate] BLOCK: ${b.reason}`);

	if (result.exitCode === 0) {
		console.error(
			`[audit-gate] OK: ${advisories.length} advisor${advisories.length === 1 ? "y" : "ies"} reviewed; ${result.warnings.length} warning(s)`,
		);
	} else {
		console.error(`[audit-gate] FAIL: ${result.blocking.length} blocking issue(s)`);
	}
	process.exit(result.exitCode);
}

// Run as CLI only when invoked directly (not when imported by tests).
// Use pathToFileURL so the comparison handles platform-specific path
// normalization (Windows drive letters, percent-encoding) that a naive
// `file://${process.argv[1]}` string-concat would miss.
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
	main();
}
