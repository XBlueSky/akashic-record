// frontend/scripts/__tests__/audit-gate.test.mjs
import { describe, it, expect } from 'vitest';
import { evaluateAudit } from '../audit-gate.mjs';

const today = '2026-04-28';

const ledgerEmpty = [];

const ledgerEsbuildAccepted = [{
  ghsa: 'GHSA-67mh-4wv8-2f99',
  package: 'esbuild',
  via: 'svelte-i18n',
  severity: 'moderate',
  cwe: ['CWE-346'],
  category: 'dev-server',
  rationale: 'Dev-only.',
  mitigation: 'Not exposed.',
  accepted_by: 'tonyhu',
  accepted_on: '2026-04-28',
  expires: '2026-10-28',
  review_action: 'Re-review on expiry.',
}];

function advisory(overrides = {}) {
  return {
    source: 1234,
    name: 'pkg',
    severity: 'moderate',
    cwe: [],
    url: 'https://example/advisory',
    title: 't',
    ghsa: 'GHSA-aaaa-bbbb-cccc',
    ...overrides,
  };
}

describe('evaluateAudit — severity blocking', () => {
  it('blocks on high severity', () => {
    const r = evaluateAudit([advisory({ severity: 'high', name: 'lodash', ghsa: 'GHSA-r5fr-rjxr-66jc' })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking).toHaveLength(1);
    expect(r.blocking[0].reason).toMatch(/high/);
  });

  it('blocks on critical severity', () => {
    const r = evaluateAudit([advisory({ severity: 'critical' })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking).toHaveLength(1);
  });
});

describe('evaluateAudit — moderate CWE blocking', () => {
  it('blocks moderate with CWE-79 (XSS)', () => {
    const r = evaluateAudit([advisory({ severity: 'moderate', cwe: ['CWE-79'] })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking[0].reason).toMatch(/CWE-79/);
  });

  it('blocks moderate with CWE-1321 (proto pollution)', () => {
    const r = evaluateAudit([advisory({ severity: 'moderate', cwe: ['CWE-1321'] })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
  });

  it('blocks moderate with CWE-918 (SSRF)', () => {
    const r = evaluateAudit([advisory({ severity: 'moderate', cwe: ['CWE-918'] })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
  });

  it('blocks moderate with CWE-287 or CWE-863 (auth)', () => {
    const r1 = evaluateAudit([advisory({ severity: 'moderate', cwe: ['CWE-287'] })], ledgerEmpty, today);
    const r2 = evaluateAudit([advisory({ severity: 'moderate', cwe: ['CWE-863'] })], ledgerEmpty, today);
    expect(r1.exitCode).toBe(1);
    expect(r2.exitCode).toBe(1);
  });

  it('blocks moderate without ledger entry even when CWE is non-blocking', () => {
    const r = evaluateAudit([advisory({ severity: 'moderate', cwe: ['CWE-346'] })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking[0].reason).toMatch(/needs.*risk-acceptance/i);
  });
});

describe('evaluateAudit — ledger', () => {
  it('allows a moderate advisory with a valid ledger entry', () => {
    const adv = advisory({ severity: 'moderate', cwe: ['CWE-346'], ghsa: 'GHSA-67mh-4wv8-2f99', name: 'esbuild' });
    const r = evaluateAudit([adv], ledgerEsbuildAccepted, today);
    expect(r.exitCode).toBe(0);
    expect(r.warnings).toHaveLength(1);
  });

  it('blocks if ledger entry is expired', () => {
    const adv = advisory({ severity: 'moderate', cwe: ['CWE-346'], ghsa: 'GHSA-67mh-4wv8-2f99', name: 'esbuild' });
    const expired = [{ ...ledgerEsbuildAccepted[0], expires: '2024-01-01' }];
    const r = evaluateAudit([adv], expired, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking[0].reason).toMatch(/expired/i);
  });

  it('blocks if ledger entry severity does not match live advisory', () => {
    const adv = advisory({ severity: 'high', cwe: ['CWE-346'], ghsa: 'GHSA-67mh-4wv8-2f99', name: 'esbuild' });
    const r = evaluateAudit([adv], ledgerEsbuildAccepted, today);
    expect(r.exitCode).toBe(1);
    // High advisory is blocked outright (rule 1) regardless of ledger.
    expect(r.blocking[0].reason).toMatch(/high|stale-shape/);
  });

  it('warns (non-fatal) on stale ledger entry not present in current audit', () => {
    const r = evaluateAudit([], ledgerEsbuildAccepted, today);
    expect(r.exitCode).toBe(0);
    expect(r.warnings.some(w => /stale|no longer/i.test(w))).toBe(true);
  });

  it('blocks moderate with CWE-79 even if ledger entry exists (XSS is hard block)', () => {
    // Sub-spec AC-2 rule 2 (CWE-79 blocks) takes precedence over rule 3 (ledger allow).
    // Operators must upgrade or remove the dep, not allowlist XSS.
    const adv = advisory({ severity: 'moderate', cwe: ['CWE-79'], ghsa: 'GHSA-67mh-4wv8-2f99', name: 'esbuild' });
    const r = evaluateAudit([adv], ledgerEsbuildAccepted, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking[0].reason).toMatch(/CWE-79/);
  });
});

describe('evaluateAudit — clean run', () => {
  it('exits 0 with no advisories and no ledger', () => {
    const r = evaluateAudit([], [], today);
    expect(r.exitCode).toBe(0);
    expect(r.blocking).toHaveLength(0);
  });

  it('does not gate low or info severities', () => {
    const r = evaluateAudit([
      advisory({ severity: 'low', cwe: ['CWE-79'] }),
      advisory({ severity: 'info' }),
    ], ledgerEmpty, today);
    expect(r.exitCode).toBe(0);
  });
});
