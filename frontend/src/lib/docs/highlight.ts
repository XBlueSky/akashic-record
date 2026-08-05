/** Lazy shiki singleton (D6): the docs initial chunk must not include
 * shiki — everything here goes through dynamic import on first use. */

// D6 language subset — do not extend without a plan change.
const LANGS = [
	"c",
	"cpp",
	"rust",
	"python",
	"typescript",
	"javascript",
	"json",
	"toml",
	"yaml",
	"bash",
	"cmake",
	"make",
	"sql",
] as const;

const ALIASES: Record<string, string> = {
	ts: "typescript",
	js: "javascript",
	sh: "bash",
	shell: "bash",
	console: "bash",
	"c++": "cpp",
	yml: "yaml",
	mk: "make",
};

export function normalizeLang(lang: string): string | null {
	const l = lang.toLowerCase();
	const mapped = ALIASES[l] ?? l;
	return (LANGS as readonly string[]).includes(mapped) ? mapped : null;
}

type Shiki = typeof import("shiki");
type Highlighter = Awaited<ReturnType<Shiki["createHighlighter"]>>;

let highlighterPromise: Promise<Highlighter> | null = null;

function getHighlighter(): Promise<Highlighter> {
	highlighterPromise ??= import("shiki").then((m) =>
		m.createHighlighter({
			themes: ["github-dark-default"],
			langs: [...LANGS],
		}),
	);
	return highlighterPromise;
}

/** Highlight `code`; resolves to shiki HTML, or null when the language is
 * outside the D6 subset (caller keeps the plain <pre>). */
export async function highlightCode(code: string, lang: string): Promise<string | null> {
	const l = normalizeLang(lang);
	if (l === null) return null;
	const h = await getHighlighter();
	return h.codeToHtml(code, { lang: l, theme: "github-dark-default" });
}
