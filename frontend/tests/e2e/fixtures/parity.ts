import { expect } from '@playwright/test';

/**
 * Parse a Set-Cookie header into an attribute map.
 *
 * Playwright collapses multiple Set-Cookie response headers into one
 * newline-separated string. We select the `ak_session=*` line if multiple
 * cookies are present; if none match the prefix, fall back to the first
 * line so the caller sees a useful parse error rather than a silent skip.
 */
export function parseSetCookie(header: string): {
  value: string;
  attrs: Map<string, string>;
} {
  const lines = header.split(/\n/).map((l) => l.trim()).filter(Boolean);
  const akLine = lines.find((l) => l.startsWith('ak_session=')) ?? lines[0] ?? header;
  const parts = akLine.split(';').map((p) => p.trim());
  const firstEq = parts[0].indexOf('=');
  const value = firstEq >= 0 ? parts[0].slice(firstEq + 1) : '';
  const attrs = new Map<string, string>();
  for (const p of parts.slice(1)) {
    const eq = p.indexOf('=');
    if (eq < 0) {
      attrs.set(p.toLowerCase(), ''); // flag attr like "HttpOnly"
    } else {
      attrs.set(p.slice(0, eq).toLowerCase(), p.slice(eq + 1));
    }
  }
  return { value, attrs };
}

/**
 * Assert that login and logout Set-Cookie attribute sets match on
 * HttpOnly, SameSite, Path, Domain, and Secure-presence. The logout
 * cookie additionally must have Max-Age=0 (or an Expires in the past).
 */
export function assertCookieAttributeParity(
  loginSetCookie: string,
  logoutSetCookie: string,
  expectedSecure: boolean,
): void {
  const login = parseSetCookie(loginSetCookie);
  const logout = parseSetCookie(logoutSetCookie);

  // Same attribute set, modulo Max-Age (which is expected to differ).
  const compareAttrs = ['httponly', 'samesite', 'path', 'domain'];
  for (const a of compareAttrs) {
    expect(
      logout.attrs.get(a),
      `attribute "${a}" mismatch: login=${login.attrs.get(a)} vs logout=${logout.attrs.get(a)}`,
    ).toBe(login.attrs.get(a));
  }

  // Secure presence parity.
  expect(login.attrs.has('secure'), `login Secure present`).toBe(expectedSecure);
  expect(logout.attrs.has('secure'), `logout Secure present`).toBe(expectedSecure);

  // Logout must clear: Max-Age=0.
  expect(logout.attrs.get('max-age')).toBe('0');

  // Sanity: login Max-Age is positive.
  const loginMaxAge = parseInt(login.attrs.get('max-age') ?? '0', 10);
  expect(loginMaxAge).toBeGreaterThan(0);
}
