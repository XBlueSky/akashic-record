import { expect, test } from "@playwright/test";

test.describe("docs reading journey", () => {
	test("hub → repo → nav → page → anchor → language → 404-in-place", async ({ page }) => {
		await page.goto("/docs");
		await expect(page.getByTestId("docs-topbar")).toBeVisible();

		const card = page.getByTestId("repo-card").filter({ hasText: "e2e-docs" });
		await expect(card).toBeVisible();
		await card.click();
		await expect(page).toHaveURL(/\/docs\/e2e-docs\/latest$/);

		// Index renders with language chips + nav groups.
		await expect(page.getByTestId("docs-content")).toContainText("E2E Docs Corpus");
		await expect(page.getByTestId("language-chip")).toBeVisible();
		const nav = page.getByTestId("docs-nav").first();
		await expect(nav).toContainText("Guide");

		// Nav → Setup page: alert, code fence, image, stamp footer.
		await nav.getByRole("link", { name: "Setup" }).click();
		await expect(page).toHaveURL(/\/docs\/e2e-docs\/latest\/guide\/setup$/);
		const content = page.getByTestId("docs-content");
		await expect(content).toContainText("This corpus exists only for e2e tests");
		await expect(content.locator(".docs-alert-note")).toBeVisible();
		await expect(page.locator(".docs-codeblock, pre.docs-code").first()).toBeVisible();
		await expect(page.getByTestId("page-footer")).toContainText(
			"documents e2e-docs v0.0.1-e2e @ 0123456",
		);

		// In-corpus link with anchor.
		await content.getByRole("link", { name: "Advanced" }).click();
		await expect(page).toHaveURL(/\/docs\/e2e-docs\/latest\/guide\/advanced#tuning$/);
		await expect(content.locator("#tuning")).toBeVisible();

		// Language chip → zh-TW page, prose lang attr flips.
		await page.goto("/docs/e2e-docs/latest");
		await page.getByTestId("language-chip").getByRole("link", { name: "繁體中文" }).click();
		await expect(page).toHaveURL(/\/docs\/e2e-docs\/latest\/zh-TW\/index$/);
		await expect(page.getByTestId("docs-content")).toHaveAttribute("lang", "zh-TW");

		// 404-in-place: nav survives, page shows guidance.
		await page.goto("/docs/e2e-docs/latest/does/not/exist");
		await expect(page.getByTestId("docs-404")).toBeVisible();
		await expect(page.getByTestId("docs-nav").first()).toBeVisible();
	});

	test("AI-surface files reach the backend through nginx", async ({ request }) => {
		const llms = await request.get("/llms.txt");
		expect(llms.status()).toBe(200);
		expect(llms.headers()["content-type"]).toContain("text/plain");
		expect(await llms.text()).toContain("e2e-docs");

		const repoLlms = await request.get("/docs/e2e-docs/llms.txt");
		expect(repoLlms.status()).toBe(200);
		expect(await repoLlms.text()).toContain("documents e2e-docs");

		const skill = await request.get("/docs/e2e-docs/skill.md");
		expect(skill.status()).toBe(200);
		expect(skill.headers()["content-type"]).toContain("text/markdown");
	});
});
