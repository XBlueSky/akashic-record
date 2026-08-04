<script lang="ts">
  import { t, locale } from "svelte-i18n";
  import type { DocsRepoEntry } from "$lib/api";

  interface Props {
    entry: DocsRepoEntry;
  }
  let { entry }: Props = $props();

  const updated = $derived(
    new Date(entry.ingested_at).toLocaleDateString($locale ?? "en", {
      year: "numeric",
      month: "short",
      day: "numeric",
    }),
  );
</script>

<a
  href={`/docs/${encodeURIComponent(entry.repo)}/latest`}
  data-testid="repo-card"
  class="block rounded-lg border border-border bg-card p-4 transition-[box-shadow,border-color] duration-150 hover:border-primary/40 hover:shadow-[0_0_24px_-8px_var(--color-primary)] motion-reduce:transition-none"
>
  <div class="flex items-baseline gap-2">
    <span class="font-mono text-sm text-foreground">{entry.repo}</span>
    <span class="rounded border border-border px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground">{entry.version}</span>
    {#if entry.derive_status === "pending" || entry.derive_status === "running"}
      <span class="rounded bg-secondary px-1.5 py-0.5 text-[10px] text-muted-foreground">{$t("docs.badge.indexing")}</span>
    {:else if entry.derive_status === "failed"}
      <span class="rounded bg-secondary px-1.5 py-0.5 text-[10px] text-muted-foreground">{$t("docs.badge.failed")}</span>
    {/if}
  </div>
  {#if entry.description}
    <p class="mt-2 line-clamp-2 text-[13px] leading-5 text-muted-foreground">{entry.description}</p>
  {/if}
  <div class="mt-3 flex gap-3 font-mono text-[11px] text-muted-foreground">
    {#if entry.page_count != null}
      <span>{$t("docs.hub.pages", { values: { count: entry.page_count } })}</span>
    {/if}
    <span>{$t("docs.hub.updated", { values: { date: updated } })}</span>
  </div>
</a>
