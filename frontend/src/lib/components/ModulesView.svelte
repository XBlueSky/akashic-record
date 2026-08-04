<script lang="ts">
  import { SvelteSet } from "svelte/reactivity";
  import { fetchModules, fetchModuleChunks, isAuthError } from "$lib/api";
  import type { Module, Chunk } from "$lib/types/index.js";
  import { chunkTypeBucket } from "$lib/graph/types";
  import * as Card from "$lib/components/ui/card";
  import { Badge } from "$lib/components/ui/badge";
  import * as Collapsible from "$lib/components/ui/collapsible";
  import { t } from "svelte-i18n";

  let { repoName }: { repoName: string } = $props();

  let modules: Module[] = $state([]);
  let selectedModule: Module | null = $state(null);
  let chunks: Chunk[] = $state([]);
  let loadingModules = $state(true);
  let loadingChunks = $state(false);
  let error: string | null = $state(null);
  let expandedChunks = new SvelteSet<string>();

  // Generation counters guard against stale async responses overwriting newer
  // state when the user navigates/clicks faster than the network responds
  // (mirrors GraphModuleView / ChunkSidebar / TimelineView). Plain `let` — read
  // only by the async callbacks below, never by markup.
  let loadGen = 0;
  let selectGen = 0;

  $effect(() => {
    // Reactive on repoName — re-fetch when the repo changes.
    repoName;
    loadModules();
  });

  async function loadModules() {
    const gen = ++loadGen;
    // Invalidate any in-flight chunk fetch from the previous repo so its late
    // response cannot repopulate `chunks` after we cleared the selection.
    selectGen++;
    loadingModules = true;
    error = null;
    modules = [];
    selectedModule = null;
    chunks = [];
    try {
      const result = await fetchModules(repoName);
      if (gen !== loadGen) return; // a newer load started; discard stale result
      modules = result;
    } catch (e: unknown) {
      if (gen !== loadGen) return;
      if (!isAuthError(e)) error = e instanceof Error ? e.message : String(e);
    } finally {
      if (gen === loadGen) loadingModules = false;
    }
  }

  async function selectModule(mod: Module) {
    // Re-clicking the already-selected module is a no-op: avoid the redundant
    // fetchModuleChunks round-trip and preserve the user's expanded-chunk state.
    if (selectedModule?.id === mod.id) return;
    const gen = ++selectGen;
    selectedModule = mod;
    loadingChunks = true;
    expandedChunks.clear();
    try {
      const result = await fetchModuleChunks(repoName, mod.path);
      if (gen !== selectGen) return; // a newer selection started; discard stale result
      chunks = result;
    } catch (e: unknown) {
      if (gen !== selectGen) return;
      chunks = [];
      if (!isAuthError(e)) error = e instanceof Error ? e.message : String(e);
    } finally {
      if (gen === selectGen) loadingChunks = false;
    }
  }

  function toggleChunk(id: string) {
    if (expandedChunks.has(id)) {
      expandedChunks.delete(id);
    } else {
      expandedChunks.add(id);
    }
  }

  function isExpanded(id: string): boolean {
    return expandedChunks.has(id);
  }

  // Badge rendering: derive bucket via the shared graph/types bucketing function
  // so new chunk types only need one update (CHUNK_TYPE_TO_BUCKET in types.ts).
  const bucketBadgeClasses: Record<string, string> = {
    function: "bg-blue-500/15 text-blue-400 border-blue-500/25",
    class:    "bg-purple-500/15 text-purple-400 border-purple-500/25",
    enum:     "bg-yellow-500/15 text-yellow-400 border-yellow-500/25",
    ghost:    "bg-slate-500/15 text-slate-400 border-slate-500/25",
    section:  "bg-cyan-500/15 text-cyan-400 border-cyan-500/25",
  };

  function chunkTypeClass(ct: string): string {
    const bucket = chunkTypeBucket(ct.toLowerCase());
    return bucketBadgeClasses[bucket] ?? "bg-muted text-muted-foreground";
  }
</script>

<div class="flex h-[calc(100vh-120px)]">
  <!-- Module Sidebar -->
  <div class="w-70 min-w-70 overflow-y-auto border-r border-border/50 bg-card/50 backdrop-blur-md">
    <h3 class="px-3.5 pt-3 pb-2 font-mono text-xs font-semibold uppercase tracking-wider text-muted-foreground">
      {$t("modules.title")}
    </h3>
    {#if loadingModules}
      <p class="px-3.5 py-3 font-mono text-sm text-muted-foreground">{$t("modules.loading")}</p>
    {:else if error && modules.length === 0}
      <p class="px-3.5 py-3 font-mono text-sm text-destructive">{error}</p>
    {:else if modules.length === 0}
      <p class="px-3.5 py-3 font-mono text-sm text-muted-foreground">{$t("modules.noModules")}</p>
    {:else}
      <ul class="list-none">
        {#each modules as mod (mod.id)}
          <li>
            <button
              class="flex w-full cursor-pointer flex-col gap-0.5 border-l-2 px-3.5 py-2 text-left text-sm transition-all
                {selectedModule?.id === mod.id
                  ? 'border-l-blue-500 bg-blue-500/10 text-blue-400 shadow-[inset_3px_0_8px_rgba(59,130,246,0.15)]'
                  : 'border-l-transparent text-secondary-foreground hover:bg-accent hover:text-foreground'}"
              onclick={() => selectModule(mod)}
            >
              <span class="break-all font-mono text-xs">{mod.path}</span>
              <span class="flex items-center gap-1.5 text-xs text-muted-foreground">
                {#if mod.language}
                  <Badge variant="secondary" class="px-1.5 py-0 text-[10px] uppercase">
                    {mod.language}
                  </Badge>
                {/if}
                <!-- file_count is nullable on the wire (Option<i32>); coalesce to 0 so
                     null never flows into the i18n count pluralizer. -->
                <span>{$t("modules.files", { values: { count: mod.file_count ?? 0 } })}</span>
              </span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </div>

  <!-- Chunk Panel -->
  <div class="flex-1 overflow-y-auto p-3 px-5">
    {#if !selectedModule}
      <div class="flex h-1/2 items-center justify-center">
        <p class="font-mono text-sm text-muted-foreground">{$t("modules.selectPrompt")}</p>
      </div>
    {:else if loadingChunks}
      <div class="flex h-1/2 items-center justify-center">
        <p class="font-mono text-sm text-muted-foreground">{$t("modules.loadingChunks")}</p>
      </div>
    {:else}
      <div class="mb-4">
        <h3 class="px-3.5 pb-1 font-mono text-xs font-semibold uppercase tracking-wider text-muted-foreground">
          {selectedModule.path}
        </h3>
        {#if selectedModule.summary}
          <p class="px-3.5 font-mono text-sm leading-relaxed text-secondary-foreground">
            {selectedModule.summary}
          </p>
        {/if}
        <p class="px-3.5 pt-0.5 font-mono text-xs text-muted-foreground">
          {$t("modules.chunkCount", { values: { count: chunks.length } })}
        </p>
      </div>

      {#if chunks.length === 0}
        <p class="px-3.5 font-mono text-sm text-muted-foreground">{$t("modules.noChunks")}</p>
      {:else}
        <div class="flex flex-col gap-2">
          {#each chunks as chunk (chunk.id)}
            <Collapsible.Root open={isExpanded(chunk.id)} onOpenChange={() => toggleChunk(chunk.id)}>
              <Card.Root class="overflow-hidden border-border/50 bg-card/50">
                <Collapsible.Trigger
                  class="flex w-full cursor-pointer items-center justify-between px-3 py-2.5 text-sm text-foreground transition-colors hover:bg-accent"
                >
                  <div class="flex items-center gap-2">
                    <Badge
                      variant="outline"
                      class="text-[10px] font-bold uppercase tracking-wide {chunkTypeClass(chunk.chunk_type)}"
                    >
                      {chunk.chunk_type}
                    </Badge>
                    <span class="font-mono text-sm">{chunk.name}</span>
                  </div>
                  <svg
                    class="h-3 w-3 text-muted-foreground transition-transform duration-200 {isExpanded(chunk.id) ? 'rotate-180' : ''}"
                    fill="none"
                    viewBox="0 0 24 24"
                    stroke="currentColor"
                    stroke-width="2"
                  >
                    <path stroke-linecap="round" stroke-linejoin="round" d="M19 9l-7 7-7-7" />
                  </svg>
                </Collapsible.Trigger>

                {#if chunk.signature}
                  <div class="border-t border-border/30 px-3 py-1.5 font-mono text-xs text-secondary-foreground">
                    {chunk.signature}
                  </div>
                {/if}

                <Collapsible.Content>
                  <pre class="m-0 overflow-x-auto bg-zinc-950 p-3 font-mono text-xs leading-relaxed text-foreground"><code>{chunk.content}</code></pre>
                </Collapsible.Content>
              </Card.Root>
            </Collapsible.Root>
          {/each}
        </div>
      {/if}
    {/if}
  </div>
</div>
