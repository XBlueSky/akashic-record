<script lang="ts">
  import * as Card from "$lib/components/ui/card";
  import { Input } from "$lib/components/ui/input";
  import { Label } from "$lib/components/ui/label";
  import { Button } from "$lib/components/ui/button";
  import { ToggleGroup, ToggleGroupItem } from "$lib/components/ui/toggle-group";
  import { ArrowLeft, Info } from "@lucide/svelte";
  import { t } from "svelte-i18n";
  import { get } from "svelte/store";

  let {
    onBack,
    onSubmit,
  }: {
    onBack: () => void;
    onSubmit: (data: { url: string; crawl_depth: number; url_pattern: string }) => void;
  } = $props();

  let url = $state("");
  // toggle-group value is bindable (fixed in SP4c Unit A)
  let crawlDepth = $state("2");
  let urlPattern = $state("");
  let error = $state<string | null>(null);

  function handleSubmit() {
    error = null;
    let trimmed = url.trim();
    if (!trimmed) { error = get(t)("ingest.website.urlRequired"); return; }
    // Auto-prepend https:// if user typed a bare hostname.
    if (!trimmed.startsWith("http://") && !trimmed.startsWith("https://")) {
      trimmed = "https://" + trimmed;
      url = trimmed;
    }
    try { new URL(trimmed); } catch { error = get(t)("ingest.website.urlInvalid"); return; }
    onSubmit({
      url: trimmed,
      crawl_depth: parseInt(crawlDepth, 10),
      url_pattern: urlPattern.trim(),
    });
  }

  let siteName = $derived(extractSiteName(url));

  function extractSiteName(u: string): string {
    let trimmedUrl = u.trim();
    if (trimmedUrl && !trimmedUrl.startsWith("http://") && !trimmedUrl.startsWith("https://")) {
      trimmedUrl = "https://" + trimmedUrl;
    }
    try {
      const parsed = new URL(trimmedUrl);
      const host = parsed.hostname;
      if (!host) return "—";
      const firstSeg = parsed.pathname.split("/").find((s) => s.length > 0);
      return firstSeg ? `${host}/${firstSeg}` : host;
    } catch {
      return "—";
    }
  }
</script>

<div class="mx-auto w-full max-w-[480px]">
  <Button variant="ghost" size="sm" onclick={onBack} class="mb-4 gap-1 font-mono text-xs text-muted-foreground">
    <ArrowLeft size={14} /> {$t("common.back")}
  </Button>

  <Card.Root class="border-border/50 bg-card/80 backdrop-blur-xl">
    <Card.Header>
      <span class="font-mono text-[10px] font-medium tracking-[0.12em] uppercase text-primary/35">
        {$t("ingest.website.label")}
      </span>
      <Card.Title class="font-mono text-lg font-bold tracking-[0.15em]">
        {$t("ingest.website.title")}
      </Card.Title>
    </Card.Header>

    <Card.Content class="space-y-4">
      <!-- URL field -->
      <div class="space-y-1.5">
        <Label class="font-mono text-xs font-medium tracking-[0.06em] uppercase text-secondary-foreground">
          {$t("ingest.website.urlLabel")}
        </Label>
        <Input
          type="text"
          bind:value={url}
          placeholder={$t("ingest.website.urlPlaceholder")}
          class="font-mono"
        />
      </div>

      <!-- Crawl scope preview -->
      {#if siteName !== "—"}
        <div class="space-y-1.5">
          <div class="rounded-md border border-primary/12 bg-primary/6 px-2.5 py-2 text-xs">
            <span class="text-muted-foreground">{$t("ingest.website.crawlScope")}</span>
            <span class="ml-1.5 inline-block rounded bg-white/5 px-2 py-0.5 font-mono text-xs text-blue-400">{siteName}</span>
          </div>
          <p class="flex items-center gap-1.5 text-xs text-slate-500">
            <Info class="h-3.5 w-3.5 shrink-0" />
            {$t("ingest.website.crawlScopeHint")}
          </p>
        </div>
      {/if}

      <!-- Crawl depth -->
      <div class="space-y-1.5">
        <Label class="font-mono text-xs font-medium tracking-[0.06em] uppercase text-secondary-foreground">
          {$t("ingest.website.depthLabel")}
        </Label>
        <ToggleGroup type="single" bind:value={crawlDepth} variant="outline" class="w-full">
          <ToggleGroupItem value="1" class="flex-1 font-mono text-xs">{$t("ingest.website.depth1")}</ToggleGroupItem>
          <ToggleGroupItem value="2" class="flex-1 font-mono text-xs">{$t("ingest.website.depth2")}</ToggleGroupItem>
          <ToggleGroupItem value="3" class="flex-1 font-mono text-xs">{$t("ingest.website.depth3")}</ToggleGroupItem>
        </ToggleGroup>
      </div>

      <!-- URL Pattern -->
      <div class="space-y-1.5">
        <Label class="font-mono text-xs font-medium tracking-[0.06em] uppercase text-secondary-foreground">
          {$t("ingest.website.patternLabel")}
        </Label>
        <Input
          type="text"
          bind:value={urlPattern}
          placeholder={$t("ingest.website.patternPlaceholder")}
          class="font-mono"
        />
      </div>

      <!-- Error message -->
      {#if error}
        <p class="text-sm text-destructive">{error}</p>
      {/if}
    </Card.Content>

    <Card.Footer>
      <Button
        class="w-full font-mono text-sm font-bold tracking-[0.1em]"
        onclick={handleSubmit}
        disabled={!url.trim()}
      >
        {$t("ingest.website.submit")}
      </Button>
    </Card.Footer>
  </Card.Root>
</div>
