/**
 * Path B — authenticated edit journey.
 *
 * Drives the signed-in user flow end-to-end against the live test stack:
 *  1. Seed a session via /test/fixtures/session → install ak_session cookie
 *     in the Playwright context (so the SPA boots with auth already valid).
 *  2. Seed a fresh note via /test/fixtures/note (unique title per run so
 *     concurrent or repeated runs don't collide).
 *  3. Navigate to the repo detail view — which now redirects straight to
 *     the Timeline tab (the Notes-mode equivalent) — locate the seeded
 *     note by its stable DOM id (`#note-{uuid}`), click the "Edit note"
 *     pencil button (only visible when `canEdit=true` from
 *     /repos/{name}/permissions — proves gitlab-mock issued access_level
 *     >= 30).
 *  4. NoteEditor mounts; rewrite `#note-title` and pick a different
 *     `#note-category`; click Save.
 *  5. Assert the PUT request to /api/v1/repos/{name}/notes/{uuid} returns
 *     204 (the backend's success status — *not* 200; the handler returns
 *     `StatusCode::NO_CONTENT`).
 *  6. Reload the page (state is lost on full reload); re-open the same
 *     note and assert the title input + category select reflect the
 *     edited values — proving the write was persisted, not just
 *     reflected in transient client state.
 *
 * Task 2b amendment: this spec originally drove an older UI where entering
 * a repo landed on a Code Map / Notes mode toggle (`[data-slot=
 * toggle-group-item]`) that had to be clicked to reach the note list, and
 * a full page reload dropped the client back to `/` (an SPA-style flow
 * requiring re-clicking the repo card + toggle after reload). The shipped
 * UI now routes `/r/[repo]` → `/r/[repo]/timeline` by default (Timeline
 * IS the note list — no toggle click needed to reach it), and edit/close
 * actions manage state via the `?edit=<uuid>` query param on that same
 * route, so a full reload lands back on `/r/[repo]/timeline` directly
 * (verified live: reload preserves the URL, not just `/`). The mode-toggle
 * click is dropped from both the initial and reload flows below because
 * it has no current-UI equivalent to click — Timeline being the default
 * tab *is* the direct replacement for "switch to Notes mode". Every
 * other behavioral assertion (session cookie honored, canEdit-gated edit
 * button, editor round trip, 204 on save, persistence across reload) is
 * unchanged. See task-2b-report.md for the full old→new assertion
 * mapping.
 *
 * Auth surface assertion: throughout the journey the only allowed 401 is
 * /api/v1/auth/me (which can fire BEFORE the cookie takes effect on the
 * first navigation). All other authenticated endpoints (permissions,
 * notes list, notes get, notes PUT) must succeed — any 401/403 elsewhere
 * indicates the cookie was not honored.
 */
import { test, expect } from '@playwright/test';
import { seedSession, seedNote } from '../fixtures/seed';

test.describe('Path B — authenticated edit journey', () => {
  test('seed session + note, edit title and category, persist across reload', async ({
    page,
    context,
    request,
  }) => {
    // ── 1. Seed an authenticated session ───────────────────────────
    const session = await seedSession(request);

    // Install the ak_session cookie on the Playwright browser context.
    // Backend issues the cookie with Domain unset (host-only) for
    // localhost:18080; we mirror that here so the SPA's same-origin
    // /api/v1/* fetches pick it up.
    await context.addCookies([
      {
        name: 'ak_session',
        value: session.apiKey,
        domain: 'localhost',
        path: '/',
        httpOnly: true,
        secure: false,
        sameSite: 'Lax',
      },
    ]);

    // ── 2. Seed a unique note for this run ─────────────────────────
    const editTag = crypto.randomUUID().slice(0, 8);
    const origTitle = `e2e-orig-${editTag}`;
    const editedTitle = `e2e-edited-${editTag}`;
    const note = await seedNote(request, {
      title: origTitle,
      content: 'original body for path B edit test',
      category: 'ARCHITECTURE',
    });

    // ── 3. Track unexpected auth failures ──────────────────────────
    const authFailures: string[] = [];
    page.on('response', (resp) => {
      const status = resp.status();
      if (status !== 401 && status !== 403) return;
      // /api/v1/auth/me may legitimately 401 transiently while the
      // session cookie is being installed; everything else under
      // /api/v1 must succeed for an authenticated edit journey.
      if (resp.url().endsWith('/api/v1/auth/me')) return;
      authFailures.push(`${status} ${resp.request().method()} ${resp.url()}`);
    });

    // ── 4. Open landing + click into the repo ──────────────────────
    // Use `domcontentloaded` — the landing's WebGL canvas keeps the
    // network non-idle indefinitely.
    await page.goto('/', { waitUntil: 'domcontentloaded' });

    const repoCard = page.locator('.repo-card', { hasText: 'akashic-record' }).first();
    await expect(repoCard).toBeVisible({ timeout: 10_000 });
    await repoCard.click();

    // ── 5. Repo detail mounts on Timeline (Notes-mode equivalent) ──
    // `/r/[repo]` redirects to `/r/[repo]/timeline` by default — no
    // manual mode-toggle click is needed to reach the note list. The
    // 3-tab nav mounting is the current-UI signal that the repo detail
    // view (incl. permissions-gated note list) is ready.
    const repoNav = page.getByRole('navigation', { name: 'Repository views' });
    await expect(repoNav).toBeVisible({ timeout: 10_000 });
    await expect(page).toHaveURL(/\/r\/akashic-record\/timeline/);

    // ── 6. Locate the seeded note + open the editor ────────────────
    // NoteCard renders the Card.Root with `id="note-{uuid}"`, which
    // gives us a deterministic per-note anchor regardless of title
    // rendering decisions (NoteCard doesn't render `note.title`
    // directly — only `summary` — so we cannot match by title text).
    const noteSelector = `#note-${note.uuid}`;
    const noteCard = page.locator(noteSelector);
    await expect(noteCard).toBeVisible({ timeout: 10_000 });

    // The pencil edit button is gated on `canEdit` which is set from
    // /repos/{name}/permissions. If permission resolution against
    // gitlab-mock fails, this button won't appear and the test fails
    // here with a clear locator timeout.
    const editButton = noteCard.locator('button[title="Edit note"]');
    await expect(editButton).toBeVisible({ timeout: 5_000 });
    await editButton.click();

    // ── 7. Drive the editor: rewrite title + change category ───────
    const titleInput = page.locator('input#note-title');
    await expect(titleInput).toBeVisible({ timeout: 10_000 });
    // Wait for loadNote() to populate (the input is empty during the
    // `loading` state — the editor doesn't render the form fields
    // until loading=false, but $effect can fire before fetch resolves).
    await expect(titleInput).toHaveValue(origTitle, { timeout: 5_000 });

    await titleInput.fill(editedTitle);

    const categorySelect = page.locator('select#note-category');
    await expect(categorySelect).toBeVisible();
    await categorySelect.selectOption('BUG_FIX');

    // ── 8. Save + wait for the PUT response ────────────────────────
    // NoteEditor uses fetch('PUT', credentials: 'include'). The backend
    // handler returns 204 No Content on success.
    const putUrlFragment = `/api/v1/repos/akashic-record/notes/${note.uuid}`;
    const [putResp] = await Promise.all([
      page.waitForResponse(
        (r) => r.request().method() === 'PUT' && r.url().includes(putUrlFragment),
        { timeout: 10_000 },
      ),
      page.getByRole('button', { name: 'Save', exact: true }).click(),
    ]);
    expect(putResp.status(), 'PUT to notes endpoint must succeed').toBe(204);

    // ── 9. Reload + verify persistence ─────────────────────────────
    // Full reload — discards all client state. onsaved (Save's success
    // callback) already closed the editor and removed `?edit=` from the
    // URL, so we're back on `/r/akashic-record/timeline` before reload.
    // Unlike the old SPA-style flow, this route reload does NOT bounce
    // back to `/` — SvelteKit re-runs the layout + Timeline page loads
    // directly against `/r/akashic-record/timeline`, so no re-navigation
    // through the landing page or a mode toggle is needed to get back to
    // the note list.
    await page.reload({ waitUntil: 'domcontentloaded' });
    await expect(page).toHaveURL(/\/r\/akashic-record\/timeline/);

    const reloadedCard = page.locator(`#note-${note.uuid}`);
    await expect(reloadedCard).toBeVisible({ timeout: 10_000 });
    await reloadedCard.locator('button[title="Edit note"]').click();

    const reloadedTitle = page.locator('input#note-title');
    await expect(reloadedTitle).toBeVisible({ timeout: 10_000 });
    await expect(reloadedTitle).toHaveValue(editedTitle, { timeout: 5_000 });

    const reloadedCategory = page.locator('select#note-category');
    await expect(reloadedCategory).toHaveValue('BUG_FIX');

    // ── 10. No unexpected auth failures ────────────────────────────
    expect(
      authFailures,
      `unexpected auth failures during authenticated edit: ${authFailures.join(', ')}`,
    ).toEqual([]);
  });
});
