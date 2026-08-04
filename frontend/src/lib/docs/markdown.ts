import MarkdownIt from "markdown-it";
import anchor from "markdown-it-anchor";
import footnote from "markdown-it-footnote";
import taskLists from "markdown-it-task-lists";
import GithubSlugger from "github-slugger";
import DOMPurify from "dompurify";
import { docsAlerts } from "./alerts.js";
import { fileDir, fullKeyToUrlPath, isInCorpus, resolveRelative, splitAnchor, encodeUrlPath } from "./paths.js";
import { docsRawUrl } from "$lib/api/docs.js";

export interface DocsRenderCtx {
  repo: string;
  /** The URL's version selector (usually "latest") — reused verbatim in
   * rewritten links so pinned URLs stay pinned. */
  version: string;
  indexDir: string;
  indexPath: string;
  pageFullKey: string;
}

export interface TocEntry {
  level: 2 | 3;
  id: string;
  text: string;
}

export interface RenderedDoc {
  html: string;
  toc: TocEntry[];
}

export type HrefKind = "page" | "asset" | "external" | "anchor" | "dead";

export function stripFrontmatter(src: string): string {
  if (!src.startsWith("---")) return src;
  const m = /^---\r?\n[\s\S]*?\r?\n---\r?\n?/.exec(src);
  return m ? src.slice(m[0].length) : src;
}

export function rewriteHref(href: string, ctx: DocsRenderCtx): { href: string; kind: HrefKind } {
  if (href.startsWith("#")) return { href, kind: "anchor" };
  if (/^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith("//")) return { href, kind: "external" };
  if (href.startsWith("/")) return { href, kind: "dead" };
  const { path, anchor: frag } = splitAnchor(href);
  if (path === "") return { href, kind: "anchor" };
  const { fullKey, escaped } = resolveRelative(fileDir(ctx.pageFullKey), path);
  if (escaped || !isInCorpus(fullKey, ctx.indexDir)) return { href, kind: "dead" };
  if (path.toLowerCase().endsWith(".md")) {
    const urlPath = fullKeyToUrlPath(fullKey, ctx.indexDir, ctx.indexPath);
    const base = `/docs/${encodeURIComponent(ctx.repo)}/${encodeURIComponent(ctx.version)}`;
    return {
      href: urlPath === "" ? `${base}${frag}` : `${base}/${encodeUrlPath(urlPath)}${frag}`,
      kind: "page",
    };
  }
  return { href: docsRawUrl(ctx.repo, ctx.version, fullKey) + frag, kind: "asset" };
}

export function renderDocsMarkdown(source: string, ctx: DocsRenderCtx): RenderedDoc {
  const slugger = new GithubSlugger();
  const toc: TocEntry[] = [];
  const md = new MarkdownIt({ html: true, linkify: true, typographer: false });
  // html:true lets corpus HTML *reach* DOMPurify, which owns the security
  // boundary (allowlist below); markdown-it's own escaping would instead
  // show raw tags as text. Do NOT weaken the sanitize config.
  // linkify:true = GFM autolinks; rewritten by link_open like any link.
  md.use(docsAlerts);
  md.use(footnote);
  md.use(taskLists, { enabled: false }); // read-only checkboxes (GFM)
  md.use(anchor, {
    slugify: (s: string) => slugger.slug(s),
    permalink: anchor.permalink.linkInsideHeader({
      symbol: "#",
      placement: "after",
      class: "header-anchor",
      ariaHidden: true,
    }),
  });

  md.renderer.rules.fence = (tokens, idx) => {
    const t = tokens[idx];
    const lang = (t.info || "").trim().split(/\s+/)[0] ?? "";
    const escaped = md.utils.escapeHtml(t.content);
    if (lang === "mermaid") return `<pre class="docs-mermaid" data-mermaid="">${escaped}</pre>\n`;
    return `<pre class="docs-code" data-code="" data-lang="${md.utils.escapeHtml(lang)}"><code>${escaped}</code></pre>\n`;
  };

  const defaultLink =
    md.renderer.rules.link_open ??
    ((tokens, idx, opts, _env, self) => self.renderToken(tokens, idx, opts));
  md.renderer.rules.link_open = (tokens, idx, opts, env, self) => {
    const t = tokens[idx];
    const href = t.attrGet("href");
    if (href != null) {
      const r = rewriteHref(href, ctx);
      if (r.kind === "dead") {
        t.attrSet("href", "#");
        t.attrJoin("class", "docs-deadlink");
      } else {
        t.attrSet("href", r.href);
      }
      if (r.kind === "external") {
        t.attrSet("target", "_blank");
        t.attrSet("rel", "noopener noreferrer");
        t.attrJoin("class", "docs-external");
      }
    }
    return defaultLink(tokens, idx, opts, env, self);
  };

  const defaultImage = md.renderer.rules.image;
  md.renderer.rules.image = (tokens, idx, opts, env, self) => {
    const t = tokens[idx];
    const src = t.attrGet("src");
    if (src != null) {
      const r = rewriteHref(src, ctx);
      if (r.kind === "asset" || r.kind === "external") t.attrSet("src", r.href);
      else t.attrSet("src", "");
      t.attrSet("loading", "lazy");
    }
    return defaultImage
      ? defaultImage(tokens, idx, opts, env, self)
      : self.renderToken(tokens, idx, opts);
  };

  md.core.ruler.push("docs_toc", (state) => {
    const tokens = state.tokens;
    for (let i = 0; i < tokens.length; i++) {
      const t = tokens[i];
      if (t.type !== "heading_open" || (t.tag !== "h2" && t.tag !== "h3")) continue;
      const id = t.attrGet("id");
      const inline = tokens[i + 1];
      if (!id || inline?.type !== "inline") continue;
      const text = (
        inline.children
          ?.filter((c) => c.type === "text" || c.type === "code_inline")
          .map((c) => c.content)
          .join("") ?? inline.content
      ).trim();
      toc.push({ level: t.tag === "h2" ? 2 : 3, id, text });
    }
    return true;
  });

  const raw = md.render(stripFrontmatter(source));
  const html = DOMPurify.sanitize(raw, {
    USE_PROFILES: { html: true },
    ADD_ATTR: ["target"],
    FORBID_TAGS: ["style", "svg", "math"],
  });
  return { html, toc };
}

// `target` is NOT in DOMPurify's default allowlist — upstream excludes it
// precisely because `target="_blank"` without `rel` hands the opened page a
// live `window.opener` handle (reverse tabnabbing), and DOMPurify never adds
// `rel` itself. `ADD_ATTR: ["target"]` above re-admits it, so the `rel`
// guarantee has to be re-established HERE rather than in the `link_open`
// renderer: with `html: true`, an anchor authored as raw HTML in the corpus
// (`<a href=".." target="_blank">`) is an opaque `html_inline`/`html_block`
// token that never reaches `link_open`. The sanitizer is the one point every
// anchor — tokenized or raw — passes through.
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.hasAttribute?.("target")) {
    node.setAttribute("rel", "noopener noreferrer");
  }
});
