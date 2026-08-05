import { request, type FullConfig } from "@playwright/test";

const BACKEND = process.env.PLAYWRIGHT_BACKEND_URL ?? "http://localhost:13001";

export default async function globalSetup(_config: FullConfig) {
	const ctx = await request.newContext({ baseURL: BACKEND });

	// 1) Sanity: backend reachable. Backend exposes /health and /ready
	// at the root (not under /api/v1), so use the canonical path here.
	const health = await ctx.get("/health");
	if (!health.ok()) {
		throw new Error(
			`Backend health check failed at ${BACKEND}/health (status ${health.status()}). ` +
				`Bring the test stack up with: docker compose -f docker-compose.test.yml up -d --wait`,
		);
	}

	// 2) Seed the akashic-record repo (idempotent).
	const repoResp = await ctx.post("/test/fixtures/repo", {
		data: { name: "akashic-record", source_type: "gitlab" },
	});
	if (!repoResp.ok()) {
		throw new Error(
			`Failed to seed akashic-record repo: ${repoResp.status()} ${await repoResp.text()}. ` +
				`Is the backend built with --features test-fixtures?`,
		);
	}

	// 3) Seed at least one note so the graph view has a node to click.
	const noteResp = await ctx.post("/test/fixtures/note", {
		data: {
			repo_name: "akashic-record",
			title: "D3 globalSetup seed note",
			content: "Anonymous read journey lands here.",
			category: "ARCHITECTURE",
		},
	});
	if (!noteResp.ok()) {
		throw new Error(`Failed to seed setup note: ${noteResp.status()} ${await noteResp.text()}`);
	}

	// 4) Seed the e2e-docs corpus for docs reading journey e2e.
	const docsResp = await ctx.post("/test/fixtures/docs-corpus", {
		data: { repo: "e2e-docs" },
	});
	if (!docsResp.ok()) {
		throw new Error(
			`Failed to seed e2e-docs corpus: ${docsResp.status()} ${await docsResp.text()}`,
		);
	}

	await ctx.dispose();
}
