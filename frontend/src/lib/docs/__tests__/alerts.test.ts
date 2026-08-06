import { describe, it, expect } from "vitest";
import MarkdownIt from "markdown-it";
import { docsAlerts } from "../alerts.js";

const md = new MarkdownIt({ html: false }).use(docsAlerts);

describe("docsAlerts", () => {
	for (const kind of ["NOTE", "TIP", "IMPORTANT", "WARNING", "CAUTION"]) {
		it(`converts [!${kind}]`, () => {
			const html = md.render(`> [!${kind}]\n> body text`);
			expect(html).toContain(`docs-alert docs-alert-${kind.toLowerCase()}`);
			expect(html).toContain(`<p class="docs-alert-title">${kind}</p>`);
			expect(html).toContain("body text");
			expect(html).not.toContain(`[!${kind}]`);
			expect(html).not.toContain("<blockquote>");
		});
	}
	it("plain blockquote untouched", () => {
		const html = md.render("> just a quote");
		expect(html).toContain("<blockquote>");
		expect(html).not.toContain("docs-alert");
	});
	it("marker mid-text is not an alert", () => {
		const html = md.render("> see [!NOTE] docs");
		expect(html).toContain("<blockquote>");
	});
	it("single-line alert with trailing text", () => {
		const html = md.render("> [!TIP] quick hint");
		expect(html).toContain("quick hint");
		expect(html).not.toContain("[!TIP]");
	});
});
