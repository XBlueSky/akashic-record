/**
 * Path A — anonymous read journey.
 *
 * Drives the unauthenticated user flow: landing page → click the seeded
 * `akashic-record` repository card → repo detail view mounts with the
 * 3-tab navigation (Timeline / Graph / Modules) → the default route lands
 * on Timeline (the notes list) where the seeded note card is visible →
 * switch to the Graph tab → macro graph SVG renders with the repository's
 * center node.
 *
 * Task 2b amendment: this spec originally targeted an older UI — a
 * Code Map / Notes mode toggle (`[data-slot=toggle-group-item]`) with an
 * inner Graph/Modules tab bar shown under Code Map. That toggle no longer
 * exists. The shipped UI (`src/routes/r/[repo]/+layout.svelte`) now uses a
 * flat 3-tab `<nav aria-label="Repository views">`, and `/r/[repo]`
 * redirects to `/r/[repo]/timeline` by default — so the note-list view
 * (what used to be "Notes mode") is what a visitor sees first, and Graph
 * is reached via an explicit tab click instead of being the initial view.
 * Every original assertion has a direct counterpart below (see
 * task-2b-report.md for the old→new mapping); order changed only because
 * the default tab changed.
 *
 * Known pre-existing UI bug (reported, not fixed here — app code is out of
 * scope for this spec-only task): the Timeline tab's label is driven by
 * `$t('repo.timeline')`, but `repo.timeline` has no entry in
 * `src/lib/i18n/en.json` (only `repo.graphTab`/`repo.modulesTab` exist).
 * svelte-i18n's missing-key fallback returns the key path itself, so the
 * tab renders the literal text "repo.timeline" instead of "Timeline" — the
 * `|| "Timeline"` fallback in the template never fires because the
 * returned string is truthy. Confirmed via live DOM inspection against
 * this stack. This spec deliberately locates the Timeline tab by its
 * `href` (a stable, user-facing navigation target) rather than by name, so
 * it is unaffected by that bug either way.
 *
 * Auth surface assertions:
 *  - `/api/v1/auth/me` MAY 401 (frontend probes it to decide whether to
 *    render signed-in UI; an anonymous user is expected to see 401).
 *  - Every other request must succeed (no 401 or 403) — the anonymous
 *    read journey must not hit any auth-gated endpoint by accident.
 *
 * Why the assertion stops at "note card visible" and not "open the note
 * detail panel and read the body": the globalSetup seed uses the
 * `/test/fixtures/note` endpoint, which writes `summary = NULL`. The
 * NoteCard's expand-to-read row is gated on `note.summary`, so a
 * summary-less note has no clickable expand affordance. Asserting the
 * card itself is visible (via its stable `id="note-{uuid}"` anchor prefix
 * — an app-authored, semantically stable attribute used for
 * scroll-to-highlight, not a UI-library internal) exercises the entire
 * read journey through the data-loading boundary (notes list endpoint,
 * NoteCard render).
 */
import { test, expect } from "@playwright/test";

test.describe("Path A — anonymous read journey", () => {
	test("landing → repo → tab nav mounts → notes visible → graph mounts", async ({ page }) => {
		// Track unexpected auth failures. `/api/v1/auth/me` is allowed to 401
		// — that's how the frontend detects "no session" for anonymous users.
		const authFailures: string[] = [];
		page.on("response", (resp) => {
			if (resp.status() !== 401 && resp.status() !== 403) return;
			if (resp.url().endsWith("/api/v1/auth/me")) return;
			authFailures.push(`${resp.status()} ${resp.url()}`);
		});

		// 1. Landing renders. Use `domcontentloaded` instead of `networkidle`
		//    because the landing's WebGL canvas keeps the network non-idle.
		await page.goto("/", { waitUntil: "domcontentloaded" });

		// 2. Repo card visible + clickable.
		const repoCard = page.locator(".repo-card", { hasText: "akashic-record" }).first();
		await expect(repoCard).toBeVisible({ timeout: 10_000 });
		await repoCard.click();

		// 3. Repo detail mounts: the 3-tab nav (Timeline / Graph / Modules)
		//    replaces the old Code Map/Notes mode toggle. `/r/[repo]`
		//    redirects to `/r/[repo]/timeline` by default, so Timeline is the
		//    active tab immediately (aria-current="page").
		const repoNav = page.getByRole("navigation", { name: "Repository views" });
		await expect(repoNav).toBeVisible({ timeout: 10_000 });
		await expect(repoNav.locator('a[href*="/timeline"]')).toHaveAttribute("aria-current", "page");
		await expect(repoNav.getByRole("link", { name: "Graph" })).toBeVisible();
		await expect(repoNav.getByRole("link", { name: "Modules" })).toBeVisible();

		// 4. Timeline (the Notes-mode equivalent) is the default landing tab
		//    — assert at least one seeded NoteCard renders. NoteCard's
		//    Card.Root carries a stable `id="note-{uuid}"` anchor (used for
		//    scroll-to-highlight), so matching the id prefix identifies a
		//    rendered note without depending on internal UI-library markup.
		await expect(page.locator('[id^="note-"]').first()).toBeVisible({ timeout: 10_000 });

		// 5. Switch to the Graph tab → macro graph SVG mounts with at least
		//    the repo center node. The seed has zero modules, so the macro
		//    layer renders just the repo node — that's enough to prove the
		//    graph endpoint resolved.
		await repoNav.getByRole("link", { name: "Graph" }).click();
		await expect(page).toHaveURL(/\/r\/akashic-record\/graph/);
		await expect(page.locator("svg .node-group").first()).toBeVisible({ timeout: 10_000 });

		// 6. Zero unexpected auth failures throughout the journey.
		expect(
			authFailures,
			`unexpected auth failures during anonymous read: ${authFailures.join(", ")}`,
		).toEqual([]);
	});
});
