// backend/scripts/__tests__/audit-gate-rs.test.mjs
import { describe, it, expect } from 'vitest';
import { evaluateAudit, flattenCargoAuditJson } from '../audit-gate-rs.mjs';

const today = '2026-04-28';
const ledgerEmpty = [];

function advisory(overrides = {}) {
  return {
    id: 'RUSTSEC-9999-0001',
    name: 'pkg',
    severity: 'medium',
    categories: [],
    kind: 'vulnerability',
    url: 'https://rustsec.org/advisories/RUSTSEC-9999-0001',
    title: 't',
    ...overrides,
  };
}

const ledgerRsa = [{
  rustsec: 'RUSTSEC-2023-0071',
  package: 'rsa',
  via: 'sqlx-mysql',
  severity: 'medium',
  categories: ['crypto-failure'],
  kind: 'vulnerability',
  category_label: 'compiled-but-unreachable',
  rationale: 'sqlx-mysql is compiled because sqlx-macros pulls in all driver crates for compile-time query checking. Backend uses Postgres exclusively.',
  mitigation: 'No MySqlConnection or MySqlPool is ever instantiated; rsa code path is unreachable at runtime.',
  accepted_by: 'tonyhu',
  accepted_on: '2026-04-28',
  expires: '2026-10-28',
  review_action: 'Re-run cargo audit; if upstream fix exists, drop ledger entry and upgrade.',
  unreachable_justification: 'sqlx-macros pulls all driver crates for query checking; the backend code never opens a MySql connection.',
  verified_unreachable_by: "rg -i 'sqlx::mysql|MySqlConnection|MySqlPool|MySqlRow|mysql://' backend/src   # 0 matches, exit 1",
}];

const ledgerRsaNoOverride = [{
  ...ledgerRsa[0],
  unreachable_justification: '',
  verified_unreachable_by: '',
}];

describe('evaluateAudit — severity blocking (rule 1)', () => {
  it('blocks on high severity', () => {
    const r = evaluateAudit([advisory({ severity: 'high', name: 'foo', id: 'RUSTSEC-2026-0001' })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking).toHaveLength(1);
    expect(r.blocking[0].reason).toMatch(/high/);
  });

  it('blocks on critical severity', () => {
    const r = evaluateAudit([advisory({ severity: 'critical' })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking).toHaveLength(1);
  });

  it('rejects ledger override of high severity even when unreachable_justification is set', () => {
    const adv = advisory({ severity: 'high', categories: ['crypto-failure'], id: 'RUSTSEC-2023-0071', name: 'rsa' });
    const r = evaluateAudit([adv], ledgerRsa, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking[0].reason).toMatch(/high|reject/i);
  });
});

describe('evaluateAudit — category blocking (rule 2)', () => {
  it('blocks medium with crypto-failure', () => {
    const r = evaluateAudit([advisory({ severity: 'medium', categories: ['crypto-failure'] })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking[0].reason).toMatch(/crypto-failure/);
  });

  it('blocks medium with code-execution', () => {
    const r = evaluateAudit([advisory({ severity: 'medium', categories: ['code-execution'] })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
  });

  it('blocks medium with memory-corruption', () => {
    const r = evaluateAudit([advisory({ severity: 'medium', categories: ['memory-corruption'] })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
  });

  it('blocks medium with privilege-escalation', () => {
    const r = evaluateAudit([advisory({ severity: 'medium', categories: ['privilege-escalation'] })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
  });

  it('does not block medium with non-blocking category (e.g. denial-of-service) when ledger has matching entry', () => {
    const adv = advisory({ severity: 'medium', categories: ['denial-of-service'], id: 'RUSTSEC-2023-0071', name: 'rsa' });
    // Use ledgerRsa but with categories adjusted for shape match
    const ledgerDos = [{ ...ledgerRsa[0], categories: ['denial-of-service'] }];
    const r = evaluateAudit([adv], ledgerDos, today);
    expect(r.exitCode).toBe(0);
  });
});

describe('evaluateAudit — ledger valid / expired / stale-shape (rule 3)', () => {
  it('rule 4 blocks medium without ledger entry even when category is non-blocking', () => {
    const r = evaluateAudit([advisory({ severity: 'medium', categories: ['denial-of-service'] })], ledgerEmpty, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking[0].reason).toMatch(/needs.*risk-acceptance/i);
  });

  it('blocks if ledger entry is expired', () => {
    const adv = advisory({ severity: 'medium', categories: ['crypto-failure'], id: 'RUSTSEC-2023-0071', name: 'rsa' });
    const expired = [{ ...ledgerRsa[0], expires: '2024-01-01' }];
    const r = evaluateAudit([adv], expired, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking[0].reason).toMatch(/expired/i);
  });

  it('blocks on stale-shape (severity/package mismatch)', () => {
    const adv = advisory({ severity: 'medium', categories: ['denial-of-service'], id: 'RUSTSEC-2023-0071', name: 'wrong-pkg' });
    const ledgerDos = [{ ...ledgerRsa[0], categories: ['denial-of-service'] }];
    const r = evaluateAudit([adv], ledgerDos, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking[0].reason).toMatch(/stale-shape/);
  });

  it('warns (non-fatal) on stale ledger entry not present in current audit', () => {
    const r = evaluateAudit([], ledgerRsa, today);
    expect(r.exitCode).toBe(0);
    expect(r.warnings.some(w => /stale|no longer/i.test(w))).toBe(true);
  });
});

describe('evaluateAudit — rule 3-bis override hatch', () => {
  it('allows medium+blocking-category advisory with valid override fields', () => {
    const adv = advisory({ severity: 'medium', categories: ['crypto-failure'], id: 'RUSTSEC-2023-0071', name: 'rsa' });
    const r = evaluateAudit([adv], ledgerRsa, today);
    expect(r.exitCode).toBe(0);
    expect(r.warnings.some(w => /allowlisted.*unreachable/i.test(w))).toBe(true);
  });

  it('rejects override attempt with empty unreachable_justification', () => {
    const adv = advisory({ severity: 'medium', categories: ['crypto-failure'], id: 'RUSTSEC-2023-0071', name: 'rsa' });
    const r = evaluateAudit([adv], ledgerRsaNoOverride, today);
    expect(r.exitCode).toBe(1);
    expect(r.blocking[0].reason).toMatch(/lacks unreachable justification/i);
  });
});

describe('evaluateAudit — warn-only kinds (rule 5)', () => {
  it('does not block on unmaintained kind regardless of severity', () => {
    const r = evaluateAudit([
      advisory({ kind: 'unmaintained', severity: null, name: 'backoff' }),
      advisory({ kind: 'unsound', severity: null, name: 'rand' }),
      advisory({ kind: 'notice', severity: null, name: 'paste' }),
      advisory({ kind: 'yanked', severity: null, name: 'foo' }),
    ], ledgerEmpty, today);
    expect(r.exitCode).toBe(0);
    expect(r.warnings.length).toBeGreaterThanOrEqual(4);
  });

  it('does not block low severity vulnerabilities', () => {
    const r = evaluateAudit([advisory({ severity: 'low', categories: ['crypto-failure'] })], ledgerEmpty, today);
    expect(r.exitCode).toBe(0);
  });

  // Sub-spec AC-2 rule 5: a ledger entry for a warn-only advisory has NO
  // effect. The warning still fires; the entry's suppression intent is
  // silently ignored. Pinned by this test so the behavior cannot drift to
  // "ledger suppresses warning" without an explicit spec change.
  it('ignores ledger entry for warn-only kind: warning still fires, exit still 0', () => {
    const ledgerForUnmaintained = [{
      rustsec: 'RUSTSEC-2025-0012',
      package: 'backoff',
      via: 'neo4rs',
      severity: null,
      categories: [],
      kind: 'unmaintained',
      category_label: 'transitive-unmaintained',
      rationale: 'upstream replacement pending',
      mitigation: 'none',
      accepted_by: 'tonyhu',
      accepted_on: '2026-04-28',
      expires: '2026-10-28',
      review_action: 'revisit on neo4rs minor release',
    }];
    const adv = advisory({ kind: 'unmaintained', severity: null, name: 'backoff', id: 'RUSTSEC-2025-0012' });
    const r = evaluateAudit([adv], ledgerForUnmaintained, today);
    expect(r.exitCode).toBe(0);
    // Rule 5 short-circuit fires: the warn-only message is emitted and the
    // ledger lookup is bypassed. Crucially, the warning is NOT suppressed.
    expect(r.warnings.some(w => /unmaintained.*RUSTSEC-2025-0012/.test(w))).toBe(true);
    // Stale-entry warning does NOT fire (the advisory IS present in the
    // audit output), so the warnings list contains only the rule-5 line.
    expect(r.warnings.some(w => /stale|no longer/i.test(w))).toBe(false);
  });
});

describe('flattenCargoAuditJson — direct tests', () => {
  it('parses per-kind warning buckets into kind-tagged advisories', () => {
    const json = {
      vulnerabilities: { list: [] },
      warnings: {
        unmaintained: [{ advisory: { id: 'RUSTSEC-2025-0012', package: 'backoff', url: 'u', title: 't' } }],
        unsound: [{ advisory: { id: 'RUSTSEC-2026-0097', package: 'rand', url: 'u', title: 't' } }],
        notice: [{ advisory: { id: 'RUSTSEC-2024-0436', package: 'paste', url: 'u', title: 't' } }],
        yanked: [{ advisory: { id: 'RUSTSEC-9999-9999', package: 'foo', url: 'u', title: 't' } }],
      },
    };
    const { advisories } = flattenCargoAuditJson(json);
    expect(advisories).toHaveLength(4);
    const byKind = Object.fromEntries(advisories.map(a => [a.kind, a.id]));
    expect(byKind.unmaintained).toBe('RUSTSEC-2025-0012');
    expect(byKind.unsound).toBe('RUSTSEC-2026-0097');
    expect(byKind.notice).toBe('RUSTSEC-2024-0436');
    expect(byKind.yanked).toBe('RUSTSEC-9999-9999');
  });

  it('defaults missing severity on a vulnerability to medium and reports it', () => {
    const json = {
      vulnerabilities: { list: [{ advisory: { id: 'RUSTSEC-2026-0001', package: 'foo', url: 'u', title: 't' } }] },
      warnings: {},
    };
    const { advisories, defaultedSeverity } = flattenCargoAuditJson(json);
    expect(advisories[0].severity).toBe('medium');
    expect(defaultedSeverity).toContain('RUSTSEC-2026-0001');
  });

  it('dedupes advisories that appear under both vulnerability and a warning bucket via id+kind key', () => {
    // Same id appearing in two different kinds should produce two records
    // (the dedup key is id+kind, not id alone).
    const json = {
      vulnerabilities: { list: [{ advisory: { id: 'RUSTSEC-2026-0001', package: 'foo', severity: 'medium', url: 'u', title: 't' } }] },
      warnings: {
        notice: [{ advisory: { id: 'RUSTSEC-2026-0001', package: 'foo', url: 'u', title: 't' } }],
      },
    };
    const { advisories } = flattenCargoAuditJson(json);
    expect(advisories).toHaveLength(2);
    // Same id appearing twice in the SAME bucket should dedupe to one.
    const json2 = {
      vulnerabilities: { list: [
        { advisory: { id: 'RUSTSEC-2026-0001', package: 'foo', severity: 'medium', url: 'u', title: 't' } },
        { advisory: { id: 'RUSTSEC-2026-0001', package: 'foo', severity: 'medium', url: 'u', title: 't' } },
      ] },
      warnings: {},
    };
    const { advisories: a2 } = flattenCargoAuditJson(json2);
    expect(a2).toHaveLength(1);
  });
});

describe('evaluateAudit — clean run', () => {
  it('exits 0 with no advisories and no ledger', () => {
    const r = evaluateAudit([], [], today);
    expect(r.exitCode).toBe(0);
    expect(r.blocking).toHaveLength(0);
  });
});
