import { describe, it, expect } from "vitest";
import {
	renderDocsMarkdown,
	rewriteHref,
	stripFrontmatter,
	type DocsRenderCtx,
} from "../markdown.js";

const push: DocsRenderCtx = {
	repo: "acme",
	version: "latest",
	indexDir: "",
	indexPath: "index.md",
	pageFullKey: "guide/setup.md",
};
const pull: DocsRenderCtx = {
	...push,
	indexDir: "docs",
	indexPath: "docs/README.md",
	pageFullKey: "docs/guide/setup.md",
};

describe("rewriteHref rule table", () => {
	const cases: Array<[DocsRenderCtx, string, string, string]> = [
		// ctx, input href, expected kind, expected href
		[push, "advanced.md#tuning", "page", "/docs/acme/latest/guide/advanced#tuning"],
		[push, "../index.md", "page", "/docs/acme/latest"],
		[push, "../api.md", "page", "/docs/acme/latest/api"],
		[push, "images/arch.png", "asset", "/api/v1/docs/acme/latest/raw/guide/images/arch.png"],
		[push, "https://example.com/x", "external", "https://example.com/x"],
		[push, "mailto:dev@example.com", "external", "mailto:dev@example.com"],
		[push, "#local", "anchor", "#local"],
		[push, "../../etc/passwd.md", "dead", "../../etc/passwd.md"],
		[push, "/rooted/path.md", "dead", "/rooted/path.md"],
		[pull, "advanced.md", "page", "/docs/acme/latest/guide/advanced"],
		[pull, "../README.md", "page", "/docs/acme/latest"],
		[pull, "../../outside.md", "dead", "../../outside.md"],
	];
	for (const [ctx, href, kind, out] of cases) {
		it(`${href} → ${kind}`, () => {
			const r = rewriteHref(href, ctx);
			expect(r.kind).toBe(kind);
			expect(r.href).toBe(out);
		});
	}
});

describe("stripFrontmatter", () => {
	it("removes yaml frontmatter", () =>
		expect(stripFrontmatter("---\ntitle: x\n---\n# H")).toBe("# H"));
	it("keeps non-frontmatter content", () =>
		expect(stripFrontmatter("# H\n---\nrule")).toBe("# H\n---\nrule"));
});

describe("renderDocsMarkdown", () => {
	it("renders headings with github-slugger ids and collects h2/h3 toc", () => {
		const { html, toc } = renderDocsMarkdown(
			"# Top\n\n## Quick Start\n\n### Sub Step\n\n#### deep",
			push,
		);
		expect(html).toContain('id="quick-start"');
		expect(html).toContain('class="header-anchor"');
		expect(toc).toEqual([
			{ level: 2, id: "quick-start", text: "Quick Start" },
			{ level: 3, id: "sub-step", text: "Sub Step" },
		]);
	});
	it("dedups repeated headings like the server SlugCounter", () => {
		const { html } = renderDocsMarkdown("## Setup\n\n## Setup", push);
		expect(html).toContain('id="setup"');
		expect(html).toContain('id="setup-1"');
	});
	it("rewrites links and images", () => {
		const { html } = renderDocsMarkdown("[a](advanced.md)\n\n![d](images/x.png)", push);
		expect(html).toContain('href="/docs/acme/latest/guide/advanced"');
		expect(html).toContain('src="/api/v1/docs/acme/latest/raw/guide/images/x.png"');
		expect(html).toContain('loading="lazy"');
	});
	it("external links open in new tab with noopener", () => {
		const { html } = renderDocsMarkdown("[x](https://example.com)", push);
		expect(html).toContain('target="_blank"');
		expect(html).toContain('rel="noopener noreferrer"');
		expect(html).toContain("docs-external");
	});
	// markdown-it runs with html:true so corpus HTML reaches DOMPurify, which
	// means a hand-written anchor never passes through the link_open renderer
	// that adds rel. DOMPurify re-admits `target` via ADD_ATTR and never adds
	// `rel` on its own, so without the sanitize hook this leaks window.opener.
	it("forces rel on raw-HTML anchors that carry target", () => {
		const { html } = renderDocsMarkdown(
			'<a href="https://evil.example" target="_blank">click</a>',
			push,
		);
		expect(html).toContain('target="_blank"');
		expect(html).toContain('rel="noopener noreferrer"');
	});
	it("forces rel on raw-HTML anchors inside a block", () => {
		const { html } = renderDocsMarkdown(
			'<div>\n<a href="https://evil.example" target="_blank">click</a>\n</div>',
			push,
		);
		expect(html).toContain('rel="noopener noreferrer"');
	});
	it("emits data-attr fences for hydration", () => {
		const { html } = renderDocsMarkdown("```cpp\nint x;\n```", push);
		expect(html).toContain('class="docs-code"');
		expect(html).toContain("data-code");
		expect(html).toContain('data-lang="cpp"');
		expect(html).toContain("int x;");
	});
	it("mermaid fence gets its own marker", () => {
		const { html } = renderDocsMarkdown("```mermaid\ngraph TD;\n```", push);
		expect(html).toContain("data-mermaid");
		expect(html).not.toContain("data-code");
	});
	it("sanitizes: no inline svg, no script", () => {
		const { html } = renderDocsMarkdown(
			'<svg onload="x"></svg>\n\n<script>a()</script>\n\ntext',
			push,
		);
		expect(html).not.toContain("<svg");
		expect(html).not.toContain("<script");
		expect(html).toContain("text");
	});
	it("hides frontmatter", () => {
		const { html } = renderDocsMarkdown("---\ntitle: hidden\n---\n\nvisible", push);
		expect(html).not.toContain("hidden");
		expect(html).toContain("visible");
	});
	it("renders footnotes", () => {
		const { html } = renderDocsMarkdown("text[^1]\n\n[^1]: note body", push);
		expect(html).toContain("footnote");
		expect(html).toContain("note body");
	});
	it("renders GFM tables and strikethrough", () => {
		const { html } = renderDocsMarkdown("| a | b |\n|---|---|\n| 1 | 2 |\n\n~~gone~~", push);
		expect(html).toContain("<table>");
		expect(html).toContain("<s>gone</s>");
	});
	it("renders GFM task lists as disabled checkboxes", () => {
		const { html } = renderDocsMarkdown("- [x] done\n- [ ] todo", push);
		expect(html).toContain('type="checkbox"');
		expect(html).toContain("disabled");
	});
	it("autolinks bare URLs (GFM)", () => {
		const { html } = renderDocsMarkdown("see https://example.com/x now", push);
		expect(html).toContain('href="https://example.com/x"');
		expect(html).toContain("docs-external");
	});
});
