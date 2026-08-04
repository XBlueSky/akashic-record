/**
 * Path A — anonymous read journey.
 *
 * Drives the unauthenticated user flow: landing page → click the seeded
 * `akashic-record` repository card → repo detail view mounts with mode
 * toggle (Code Map / Notes) and tab bar (Graph / Modules) → macro graph
 * SVG renders with the repository's center node → switch to Notes mode
 * → the seeded note card is visible.
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
 * card itself is visible exercises the entire read journey through the
 * data-loading boundary (notes list endpoint, NoteCard render).
 */
import { test, expect } from '@playwright/test';

test.describe('Path A — anonymous read journey', () => {
  test('landing → repo → graph mounts → notes list renders', async ({ page }) => {
    // Track unexpected auth failures. `/api/v1/auth/me` is allowed to 401
    // — that's how the frontend detects "no session" for anonymous users.
    const authFailures: string[] = [];
    page.on('response', (resp) => {
      if (resp.status() !== 401 && resp.status() !== 403) return;
      if (resp.url().endsWith('/api/v1/auth/me')) return;
      authFailures.push(`${resp.status()} ${resp.url()}`);
    });

    // 1. Landing renders. Use `domcontentloaded` instead of `networkidle`
    //    because the landing's WebGL canvas keeps the network non-idle.
    await page.goto('/', { waitUntil: 'domcontentloaded' });

    // 2. Repo card visible + clickable.
    const repoCard = page.locator('.repo-card', { hasText: 'akashic-record' }).first();
    await expect(repoCard).toBeVisible({ timeout: 10_000 });
    await repoCard.click();

    // 3. Repo detail mounts. Both the mode toggle (Code Map / Notes) and
    //    the inner Graph/Modules tab bar should render once `load()`
    //    settles in RepoDetailView.
    await expect(
      page.locator('[data-slot=toggle-group-item]', { hasText: 'Code Map' }),
    ).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('[role=tab]', { hasText: /graph/i })).toBeVisible();

    // 4. Macro graph SVG mounts with at least the repo center node. The
    //    seed has zero modules, so the macro layer renders just the repo
    //    node — that's enough to prove the graph endpoint resolved.
    await expect(page.locator('svg .node-group').first()).toBeVisible({ timeout: 10_000 });

    // 5. Switch to Notes mode + assert at least one NoteCard renders
    //    (TimelineView wraps each card in `.note-card-wrapper`).
    await page.locator('[data-slot=toggle-group-item]', { hasText: 'Notes' }).click();
    await expect(page.locator('.note-card-wrapper').first()).toBeVisible({ timeout: 10_000 });

    // 6. Zero unexpected auth failures throughout the journey.
    expect(
      authFailures,
      `unexpected auth failures during anonymous read: ${authFailures.join(', ')}`,
    ).toEqual([]);
  });
});
