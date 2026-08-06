import type MarkdownIt from "markdown-it";

const MARKER = /^\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]\s*/;

/** GitHub-style alerts: a blockquote whose first inline content starts
 * with `[!NOTE]` (etc.) becomes `<div class="docs-alert docs-alert-note">`
 * with an injected title paragraph. In-house (~50 lines) instead of a
 * dependency — plan-level decision #3. */
export function docsAlerts(md: MarkdownIt): void {
	md.core.ruler.after("block", "docs_alerts", (state) => {
		const tokens = state.tokens;
		for (let i = 0; i < tokens.length; i++) {
			if (tokens[i].type !== "blockquote_open") continue;
			let j = i + 1;
			while (
				j < tokens.length &&
				tokens[j].type !== "inline" &&
				tokens[j].type !== "blockquote_close"
			)
				j++;
			if (j >= tokens.length || tokens[j].type !== "inline") continue;
			const inline = tokens[j];
			const m = MARKER.exec(inline.content);
			if (!m) continue;
			const kind = m[1];
			inline.content = inline.content.slice(m[0].length);
			const first = inline.children?.[0];
			if (first && first.type === "text") first.content = first.content.replace(MARKER, "");
			let depth = 0;
			let k = i;
			for (; k < tokens.length; k++) {
				if (tokens[k].type === "blockquote_open") depth++;
				else if (tokens[k].type === "blockquote_close") {
					depth--;
					if (depth === 0) break;
				}
			}
			tokens[i].tag = "div";
			tokens[i].attrJoin("class", `docs-alert docs-alert-${kind.toLowerCase()}`);
			tokens[k].tag = "div";
			const title = new state.Token("html_block", "", 0);
			title.content = `<p class="docs-alert-title">${kind}</p>\n`;
			tokens.splice(i + 1, 0, title);
			i = k + 1;
		}
		return true;
	});
}
