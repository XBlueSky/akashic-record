/**
 * Path C — login → logout cookie attribute parity.
 *
 * Pure REST spec, no UI interaction. Asserts the Set-Cookie header
 * returned by POST /api/v1/auth/logout matches the attribute set
 * (HttpOnly, SameSite, Path, Domain, Secure-presence) of the
 * Set-Cookie issued by POST /test/fixtures/session — both call the
 * same `build_session_cookie` helper, so any drift between login
 * and logout serialization fails here.
 *
 * Logout cookie additionally carries Max-Age=0.
 *
 * Closes the D8 P2-3 verification loop end-to-end: D8 had unit-test
 * parity coverage only; this exercises the real /api/v1/auth/logout
 * handler against a real seeded session.
 *
 * Parameterized on E2E_EXPECTED_SECURE. Default branch tests
 * cookie_secure=false (compose.test.yml: COOKIE_SECURE=false). Task 19
 * exercises the cookie_secure=true branch via a temporary backend
 * restart.
 */
import { test, expect } from "@playwright/test";
import { seedSession } from "../fixtures/seed";
import { assertCookieAttributeParity } from "../fixtures/parity";

const BACKEND = process.env.PLAYWRIGHT_BACKEND_URL ?? "http://localhost:13001";
const EXPECTED_SECURE = (process.env.E2E_EXPECTED_SECURE ?? "false") === "true";

test.describe(`Path C — login → logout cookie parity (expectedSecure=${EXPECTED_SECURE})`, () => {
	test("logout Set-Cookie attributes match login Set-Cookie", async ({ request }) => {
		// 1. Seed a session — capture login Set-Cookie verbatim.
		const { apiKey, setCookieHeader: loginSetCookie } = await seedSession(request);

		// 2. Call the REAL logout handler with the cookie attached.
		const logoutResp = await request.post(`${BACKEND}/api/v1/auth/logout`, {
			headers: { Cookie: `ak_session=${apiKey}` },
		});
		expect(logoutResp.status()).toBeLessThan(400);

		const logoutSetCookie = logoutResp.headers()["set-cookie"];
		expect(logoutSetCookie, "logout response must include Set-Cookie").toBeTruthy();

		// 3. Parity check.
		assertCookieAttributeParity(loginSetCookie, logoutSetCookie!, EXPECTED_SECURE);
	});
});
