<script module lang="ts">
  import type { DocsPageResponse } from "$lib/api";

  // D5: per-page SWR-lite. Freshness is the browser's job (backend sends
  // no-cache + ETag → conditional revalidation); this map only avoids
  // re-render flicker between page navs. FIFO-capped.
  const pageCache = new Map<string, DocsPageResponse>();
  const CACHE_CAP = 50;
</script>

<script lang="ts">
  import { page } from "$app/state";
  import { t } from "svelte-i18n";
  import { fetchDocsPage } from "$lib/api";
  import { ApiError } from "$lib/api/http.js";
  import { urlPathToFullKey } from "$lib/docs/paths.js";
  import { prevNext } from "$lib/docs/nav.js";
  import { renderDocsMarkdown, type DocsRenderCtx } from "$lib/docs/markdown.js";
  import { extractLanguageLine } from "$lib/docs/language.js";
  import { commandPalette } from "$lib/state/command-palette.svelte.js";
  import MarkdownView from "$lib/components/docs/MarkdownView.svelte";
  import TocRail from "$lib/components/docs/TocRail.svelte";
  import LanguageChips from "$lib/components/docs/LanguageChips.svelte";
  import StalenessBanner from "$lib/components/docs/StalenessBanner.svelte";
  import PageFooter from "$lib/components/docs/PageFooter.svelte";
  import RelatedCode from "$lib/components/docs/RelatedCode.svelte";
  import { Skeleton } from "$lib/components/ui/skeleton";
  import type { PageData } from "./$types.js";

  let { data }: { data: PageData } = $props();

  const urlPath = $derived(page.params.page ?? "");
  const fullKey = $derived(urlPathToFullKey(urlPath, data.indexDir, data.indexPath));
  const base = $derived(`/docs/${encodeURIComponent(data.repo)}/${encodeURIComponent(data.selector)}`);
  const proseLang = $derived(fullKey.split("/").includes("zh-TW") ? "zh-TW" : "en");

  let pageData = $state<DocsPageResponse | null>(null);
  let status = $state<"loading" | "ready" | "notfound" | "error">("loading");
  let container = $state<HTMLElement | null>(null);

  $effect(() => {
    const key = `${data.repo}@${data.selector}@${fullKey}`;
    const cached = pageCache.get(key);
    if (cached) {
      pageData = cached;
      status = "ready";
    } else {
      pageData = null;
      status = "loading";
    }
    let cancelled = false;
    fetchDocsPage(data.repo, data.selector, fullKey)
      .then((p) => {
        if (cancelled) return;
        pageCache.set(key, p);
        if (pageCache.size > CACHE_CAP) {
          const oldest = pageCache.keys().next().value;
          if (oldest !== undefined) pageCache.delete(oldest);
        }
        pageData = p;
        status = "ready";
      })
      .catch((e) => {
        if (cancelled) return;
        if (pageCache.has(key)) return; // stale-but-shown beats an error flash
        status = e instanceof ApiError && e.status === 404 ? "notfound" : "error";
      });
    return () => {
      cancelled = true;
    };
  });

  const ctx = $derived<DocsRenderCtx>({
    repo: data.repo,
    version: data.selector,
    indexDir: data.indexDir,
    indexPath: data.indexPath,
    pageFullKey: fullKey,
  });
  const extracted = $derived(pageData ? extractLanguageLine(pageData.markdown) : null);
  const rendered = $derived(extracted ? renderDocsMarkdown(extracted.markdown, ctx) : null);
  const nearby = $derived(prevNext(data.resolvedNav.flat, fullKey, data.indexPath));

  // Anchor deep-link positioning (D3): after content is in the DOM.
  $effect(() => {
    if (status !== "ready" || !rendered || !container) return;
    const hash = location.hash;
    if (!hash) return;
    const el = container.querySelector(`#${CSS.escape(decodeURIComponent(hash.slice(1)))}`);
    el?.scrollIntoView({ block: "start" });
  });
</script>

<svelte:head>
  <title>{pageData ? `${pageData.title} — ${data.repo}` : data.repo} — Akashic Record</title>
</svelte:head>

{#if status === "loading"}
  <div class="space-y-3 py-2" data-testid="docs-skeleton">
    <Skeleton class="h-8 w-2/5" />
    <Skeleton class="h-4 w-4/5" />
    <Skeleton class="h-4 w-3/5" />
    <Skeleton class="h-64 w-full" />
  </div>
{:else if status === "notfound"}
  <div class="py-16 text-center" data-testid="docs-404">
    <p class="font-mono text-[32px] text-muted-foreground">404</p>
    <p class="mt-2 text-sm text-foreground">{$t("docs.notFound.title")}</p>
    <p class="mt-1 font-mono text-[12px] text-muted-foreground">
      {$t("docs.notFound.body", { values: { repo: data.repo, version: data.entry.version } })}
    </p>
    <div class="mt-6 flex justify-center gap-3">
      <button
        class="rounded border border-border px-3 py-1.5 text-[13px] text-muted-foreground transition-colors duration-150 hover:text-foreground"
        onclick={() => commandPalette.show()}
      >{$t("docs.notFound.search")}</button>
      <a href={base} class="rounded border border-border px-3 py-1.5 text-[13px] text-primary hover:underline">
        {$t("docs.notFound.backToIndex")}
      </a>
    </div>
  </div>
{:else if status === "error"}
  <p class="py-16 text-center text-sm text-muted-foreground">{$t("docs.hub.loadFailed")}</p>
{:else if pageData && rendered}
  <div class="flex gap-8">
    <article class="min-w-0 flex-1">
      <StalenessBanner deriveStatus={data.entry.derive_status} />
      {#if extracted?.chips}
        <LanguageChips chips={extracted.chips} {ctx} />
      {/if}
      {#if rendered.toc.length > 0}
        <details class="mb-4 rounded border border-border px-3 py-2 xl:hidden">
          <summary class="cursor-pointer text-[11px] uppercase tracking-wider text-muted-foreground">{$t("docs.toc")}</summary>
          <ul class="mt-2 space-y-1">
            {#each rendered.toc as e (e.id)}
              <li class={e.level === 3 ? "pl-4" : ""}>
                <a href={`#${e.id}`} class="text-[12px] text-muted-foreground hover:text-foreground">{e.text}</a>
              </li>
            {/each}
          </ul>
        </details>
      {/if}
      <MarkdownView html={rendered.html} lang={proseLang} bind:el={container} />
      <RelatedCode documentId={pageData.document_id} repo={data.repo} />
      <PageFooter prev={nearby.prev} next={nearby.next} {base} stamp={pageData.stamp} {fullKey} />
    </article>
    <TocRail toc={rendered.toc} {container} />
  </div>
{/if}
