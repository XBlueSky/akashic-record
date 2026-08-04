<script lang="ts">
  import { onMount } from "svelte";
  import { fetchAllSources } from "$lib/api";
  import type { Source, SourceType } from "$lib/types/index.js";
  import { t, locale } from "svelte-i18n";
  import * as Card from "$lib/components/ui/card";
  import { Badge } from "$lib/components/ui/badge";
  import { Separator } from "$lib/components/ui/separator";
  import { GitBranch, Globe, Check, X, RefreshCw } from "@lucide/svelte";

  let { onPick }: { onPick: (type: SourceType) => void } = $props();

  let sources = $state<Source[]>([]);
  let loadingSources = $state(true);

  onMount(async () => {
    try {
      sources = await fetchAllSources();
    } catch {
      sources = [];
    } finally {
      loadingSources = false;
    }
  });

  function formatDate(iso: string): string {
    return new Date(iso).toLocaleString($locale === "zh-TW" ? "zh-TW" : "en-US", {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }
</script>

<div class="mx-auto w-full max-w-[640px]">
  <!-- Header -->
  <div class="mb-8 text-center">
    <span class="font-mono text-[10px] font-medium tracking-[0.12em] uppercase text-primary/35">
      {$t("ingest.source.label")}
    </span>
    <h2 class="mt-2 text-2xl font-bold tracking-[0.2em] text-primary drop-shadow-[0_0_20px_rgba(59,130,246,0.4)]">
      {$t("ingest.source.title")}
    </h2>
    <p class="font-mono text-xs tracking-[0.06em] text-muted-foreground">
      {$t("ingest.source.subtitle")}
    </p>
  </div>

  <!-- Source cards -->
  <div class="mb-9 grid grid-cols-2 gap-4">
    <Card.Root
      class="group cursor-pointer border-border/50 bg-card/80 backdrop-blur-xl transition-all duration-250 hover:-translate-y-0.5 hover:border-primary/25 hover:shadow-[0_0_24px_rgba(59,130,246,0.12)]"
    >
      <button class="w-full" onclick={() => onPick("gitlab")}>
        <Card.Content class="flex flex-col items-center gap-2.5 py-8">
          <GitBranch size={32} strokeWidth={1.5} class="text-muted-foreground transition-colors group-hover:text-primary" />
          <span class="text-base font-semibold tracking-[0.08em] text-secondary-foreground">
            {$t("ingest.source.gitlab")}
          </span>
          <span class="text-center font-mono text-xs tracking-[0.02em] text-muted-foreground">
            {$t("ingest.source.gitlabDesc")}
          </span>
        </Card.Content>
      </button>
    </Card.Root>

    <Card.Root
      class="group cursor-pointer border-border/50 bg-card/80 backdrop-blur-xl transition-all duration-250 hover:-translate-y-0.5 hover:border-primary/25 hover:shadow-[0_0_24px_rgba(59,130,246,0.12)]"
    >
      <button class="w-full" onclick={() => onPick("website")}>
        <Card.Content class="flex flex-col items-center gap-2.5 py-8">
          <Globe size={32} strokeWidth={1.5} class="text-muted-foreground transition-colors group-hover:text-primary" />
          <span class="text-base font-semibold tracking-[0.08em] text-secondary-foreground">
            {$t("ingest.source.website")}
          </span>
          <span class="text-center font-mono text-xs tracking-[0.02em] text-muted-foreground">
            {$t("ingest.source.websiteDesc")}
          </span>
        </Card.Content>
      </button>
    </Card.Root>
  </div>

  <!-- Recent Sources -->
  {#if !loadingSources && sources.length > 0}
    <Separator class="mb-5" />
    <h3 class="mb-3 font-mono text-[10px] font-semibold tracking-[0.14em] uppercase text-muted-foreground">
      {$t("ingest.source.recentSources")}
    </h3>
    <div class="flex flex-col gap-1.5">
      {#each sources as src (src.name)}
        <div class="flex items-center gap-2.5 rounded-md bg-muted/20 px-3 py-2 text-sm text-secondary-foreground">
          <span class="shrink-0">
            {#if src.source_type === "gitlab"}
              <GitBranch size={14} strokeWidth={1.5} />
            {:else}
              <Globe size={14} strokeWidth={1.5} />
            {/if}
          </span>
          <span class="flex-1 font-mono text-xs">{src.name}</span>
          <Badge
            variant={src.status === "healthy" || src.status === "stale"
              ? "secondary"
              : src.status === "failed"
                ? "destructive"
                : "outline"}
            class="font-mono text-[10px] font-semibold uppercase"
          >
            {#if src.status === "healthy" || src.status === "stale"}
              <Check size={10} class="mr-0.5" />
            {:else if src.status === "failed"}
              <X size={10} class="mr-0.5" />
            {:else}
              <RefreshCw size={10} class="mr-0.5 animate-spin" />
            {/if}
            {src.status}
          </Badge>
          <span class="font-mono text-xs text-muted-foreground">
            {$t("ingest.source.chunks", { values: { count: src.chunk_count } })}
          </span>
          {#if src.last_ingested_at}
            <span class="text-xs text-muted-foreground">{formatDate(src.last_ingested_at)}</span>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>
