<script lang="ts">
  import { fly } from "svelte/transition";
  import { tick, untrack } from "svelte";
  import { SvelteMap, SvelteSet } from "svelte/reactivity";
  import { X } from "@lucide/svelte";
  import { fetchModuleDetail, isAuthError } from "$lib/api";
  import type { ChunkItem, ModuleDetailData, DocumentSectionItem, Category, ExplainsTarget } from "$lib/types";
  import { t } from "svelte-i18n";
  import { goto } from "$app/navigation";

  interface Props {
    moduleId: string;
    moduleName: string;
    onclose: () => void;
    onselectnote?: (noteId: string) => void;
    focusChunkId?: string;
    selectedSection?: DocumentSectionItem | null;
    sectionMode?: boolean;
    sourceRepo?: string;
    documentTitle?: string;
  }

  let {
    moduleId,
    moduleName,
    onclose,
    onselectnote,
    focusChunkId,
    selectedSection,
    sectionMode,
    sourceRepo,
    documentTitle,
  }: Props = $props();

  let loading = $state(true);
  let error = $state("");
  let data: ModuleDetailData | null = $state(null);
  let expandedChunk: string | null = $state(null);
  const collapsedGroups = new SvelteSet<string>();

  // Chunk type grouping
  const CHUNK_GROUPS: Record<string, string[]> = {
    "Functions": ["function", "macro"],
    "Types & Structs": ["class", "struct", "trait", "enum", "type", "interface"],
    "Components": ["component"],
    "Constants & Config": ["constant", "config"],
    "Documentation": ["readme", "section", "module"],
  };

  function getGroupLabel(chunkType: string): string {
    for (const [label, types] of Object.entries(CHUNK_GROUPS)) {
      if (types.includes(chunkType)) return label;
    }
    return "Other";
  }

  interface ChunkGroup {
    label: string;
    chunks: ChunkItem[];
  }

  let groupedChunks: ChunkGroup[] = $derived.by(() => {
    if (!data) return [];
    const groups = new SvelteMap<string, ChunkItem[]>();
    for (const chunk of data.chunks) {
      const label = getGroupLabel(chunk.chunk_type);
      if (!groups.has(label)) groups.set(label, []);
      groups.get(label)!.push(chunk);
    }
    // Sort groups by predefined order
    const order = [...Object.keys(CHUNK_GROUPS), "Other"];
    return order
      .filter((label) => groups.has(label))
      .map((label) => ({ label, chunks: groups.get(label)! }));
  });

  function toggleGroup(label: string) {
    if (collapsedGroups.has(label)) {
      collapsedGroups.delete(label);
    } else {
      collapsedGroups.add(label);
    }
  }

  function toggleChunk(id: string) {
    expandedChunk = expandedChunk === id ? null : id;
  }

  // Category badge color — aligned to the canonical Category set
  // (ARCHITECTURE, BUG_FIX, CONFIG, ONBOARDING, DECISION) and reusing the
  // same --cat-* CSS tokens so badges match across views.
  // NoteItem.category is typed `string`, so we keep a string param + fallback.
  function categoryColor(category: string): string {
    const colors: Record<Category, string> = {
      ARCHITECTURE: "var(--cat-architecture)",
      BUG_FIX: "var(--cat-bug-fix)",
      CONFIG: "var(--cat-config)",
      ONBOARDING: "var(--cat-onboarding)",
      DECISION: "var(--cat-decision)",
    };
    return colors[category as Category] || "#64748B";
  }

  // Out-of-order guard: rapidly selecting modules can let an earlier fetch
  // resolve last. We bump a plain (non-reactive) token per request and bail
  // after the await if a newer load has superseded this one, so stale
  // chunks/notes never overwrite the current module's data.
  let loadToken = 0;

  async function loadDetail(id: string) {
    const token = ++loadToken;
    loading = true;
    error = "";
    data = null;
    expandedChunk = null;

    try {
      // Strip "mod:" prefix if present
      const cleanId = id.startsWith("mod:") ? id.slice(4) : id;
      const result = await fetchModuleDetail(cleanId);
      // Superseded by a newer selection — discard this stale response.
      if (token !== loadToken) return;
      data = result;
    } catch (e: unknown) {
      if (token !== loadToken) return; // ignore errors from superseded requests
      if (!isAuthError(e)) error = e instanceof Error ? e.message : "Failed to load module details";
    } finally {
      // Only the latest request may clear the loading flag.
      if (token === loadToken) loading = false;
    }
  }

  $effect(() => {
    if (moduleId) loadDetail(moduleId);
  });

  // Navigate to the target repo's graph, pre-drilling to the explained module/chunk.
  // The ?from= param carries a human-readable label for the back-banner in GraphModuleView.
  function gotoExplains(target: ExplainsTarget) {
    const fromLabel = [sourceRepo, documentTitle ?? selectedSection?.heading ?? ""]
      .filter(Boolean)
      .join(" · ");
    goto(
      `/r/${encodeURIComponent(target.repo_name)}/graph` +
      `?module=${encodeURIComponent(target.module_path)}` +
      `&chunk=${encodeURIComponent(target.chunk_id)}` +
      `&from=${encodeURIComponent(fromLabel)}`
    );
  }

  // Auto-expand and scroll to focused chunk
  // untrack writes to collapsedGroups/expandedChunk to prevent read-write loop
  $effect(() => {
    const targetId = focusChunkId;
    const groups = groupedChunks;
    if (!targetId || !data) return;
    untrack(() => {
      for (const group of groups) {
        const match = group.chunks.find((c) => c.id === targetId);
        if (match) {
          collapsedGroups.delete(group.label);
          expandedChunk = targetId;
          tick().then(() => {
            const el = document.querySelector(`[data-chunk-id="${targetId}"]`);
            el?.scrollIntoView({ behavior: "smooth", block: "center" });
          });
          break;
        }
      }
    });
  });
</script>

<div
  class="chunk-sidebar custom-scrollbar absolute top-0 right-0 z-10 h-full w-[380px] glass border-l border-border overflow-y-auto"
  transition:fly={{ x: 380, duration: 250 }}
>
  <!-- Header -->
  <div class="sticky top-0 z-10 glass border-b border-border px-4 py-3">
    <div class="flex items-center justify-between mb-1">
      <h2 class="text-sm font-semibold text-foreground font-mono truncate pr-2" title={moduleName}>
        {moduleName}
      </h2>
      <button
        class="bg-transparent border border-border rounded-md text-muted-foreground w-[26px] h-[26px] flex items-center justify-center cursor-pointer hover:border-white/15 hover:text-foreground transition-colors shrink-0"
        onclick={onclose}
        title="Close"
      >
        <X size={14} />
      </button>
    </div>
    {#if data}
      <p class="text-xs text-muted-foreground font-mono">
        {data.module.chunk_count} chunks · {data.module.note_count} notes
      </p>
    {/if}
  </div>

  {#if sectionMode && selectedSection}
    <div class="space-y-3 p-3">
      <!-- Section heading -->
      <h3 class="font-mono text-sm font-semibold text-slate-200">
        {selectedSection.heading}
      </h3>

      <!-- Tags -->
      {#if selectedSection.tags && selectedSection.tags.length > 0}
        <div class="flex flex-wrap gap-1">
          {#each selectedSection.tags as tag (tag)}
            <span class="rounded-full bg-cyan-500/10 border border-cyan-500/20 px-2 py-0.5 text-[10px] font-mono text-cyan-400">
              {tag}
            </span>
          {/each}
        </div>
      {/if}

      <!-- Content -->
      <div class="rounded-md bg-black/20 p-3 font-mono text-xs text-slate-300 leading-relaxed max-h-[400px] overflow-y-auto whitespace-pre-wrap">
        {selectedSection.content}
      </div>

      <!-- Linked Code section -->
      {#if selectedSection.explains.length > 0}
        <div class="border-t border-amber-500/20 pt-3">
          <h4 class="mb-2 font-mono text-xs font-semibold uppercase tracking-wider text-amber-400">
            Linked Code
          </h4>
          {#each selectedSection.explains as target (target.chunk_id)}
            <button
              class="flex items-center gap-2 rounded-md px-2 py-1.5 hover:bg-white/5 transition-colors w-full text-left cursor-pointer"
              onclick={() => gotoExplains(target)}
            >
              <span class="h-2 w-2 shrink-0 rounded-full {target.confidence >= 1.0 ? 'bg-amber-400' : 'border border-amber-400'}"></span>
              <div class="flex-1 min-w-0">
                <span class="block truncate font-mono text-sm text-slate-200">
                  {target.chunk_name} <span class="text-slate-500">({target.chunk_type})</span>
                </span>
                <span class="block truncate text-xs text-slate-500">
                  {target.module_path} — {target.repo_name}
                </span>
              </div>
              <span class="text-amber-400/50 text-xs shrink-0">→</span>
            </button>
          {/each}
        </div>
      {/if}
    </div>
  {:else}
    {#if loading}
      <div class="flex items-center justify-center py-12">
        <p class="text-sm text-muted-foreground font-mono">Loading...</p>
      </div>
    {:else if error}
      <div class="flex items-center justify-center py-12">
        <p class="text-sm text-red-500 font-mono">{error}</p>
      </div>
    {:else if data}
      <div class="px-4 py-3 space-y-3">
        <!-- Notes section -->
        {#if data.notes.length > 0}
          <div class="space-y-2">
            {#each data.notes as note (note.id)}
              <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
              <div
                class="rounded-md border px-3 py-2 transition-colors {onselectnote ? 'cursor-pointer hover:bg-white/5' : ''}"
                style="border-color: {categoryColor(note.category)}40; background: {categoryColor(note.category)}08;"
                role={onselectnote ? "button" : "region"}
                tabindex={onselectnote ? 0 : undefined}
                onclick={() => onselectnote?.(note.id)}
                onkeydown={(e) => e.key === "Enter" && onselectnote?.(note.id)}
              >
                <div class="flex items-center gap-2 mb-1">
                  <span
                    class="inline-block w-2 h-2 rounded-full shrink-0"
                    style="background: {categoryColor(note.category)};"
                  ></span>
                  <span class="text-[10px] font-bold tracking-wide uppercase font-mono" style="color: {categoryColor(note.category)};">
                    {note.category}
                  </span>
                </div>
                <p class="text-xs text-muted-foreground font-mono leading-relaxed">
                  {note.summary || note.title}
                </p>
              </div>
            {/each}
          </div>
          <div class="h-px bg-border"></div>
        {/if}

        <!-- Chunk groups -->
        {#each groupedChunks as group (group.label)}
          <div>
            <button
              class="w-full flex items-center gap-2 py-1.5 cursor-pointer bg-transparent border-none text-left"
              onclick={() => toggleGroup(group.label)}
            >
              <span class="text-xs text-muted-foreground">
                {collapsedGroups.has(group.label) ? "▶" : "▼"}
              </span>
              <span class="text-xs font-semibold text-foreground font-mono">
                {group.label}
              </span>
              <span class="text-xs text-muted-foreground font-mono">
                ({group.chunks.length})
              </span>
            </button>

            {#if !collapsedGroups.has(group.label)}
              <div class="ml-4 space-y-0.5">
                {#each group.chunks as chunk (chunk.id)}
                  <div data-chunk-id={chunk.id}>
                    <button
                      class="w-full flex items-center gap-2 py-1 px-2 rounded hover:bg-white/5 cursor-pointer bg-transparent border-none text-left transition-colors {focusChunkId === chunk.id ? 'ring-1 ring-primary/50 bg-primary/5' : ''}"
                      onclick={() => toggleChunk(chunk.id)}
                    >
                      <span class="text-[10px] text-teal-400">●</span>
                      <span class="text-xs text-foreground font-mono truncate">
                        {chunk.name}
                      </span>
                      {#if chunk.notes.length > 0}
                        <span class="w-1.5 h-1.5 rounded-full bg-amber-500 shrink-0 ml-auto"></span>
                        {#if chunk.notes.length > 1}
                          <span class="text-[9px] text-amber-400 font-mono ml-0.5">{chunk.notes.length}</span>
                        {/if}
                      {/if}
                    </button>

                    {#if expandedChunk === chunk.id}
                      <div class="ml-5 mt-1 mb-2 space-y-2">
                        <!-- Notes (above code) -->
                        {#if chunk.notes.length > 0}
                          <div class="space-y-1">
                            {#each chunk.notes as note (note.id)}
                              <button
                                class="flex items-center gap-2 w-full text-left rounded px-2 py-1 hover:bg-white/5 transition-colors cursor-pointer bg-transparent border-none"
                                onclick={() => onselectnote?.(note.id)}
                              >
                                <span
                                  class="text-[9px] px-1.5 py-0.5 rounded font-mono font-semibold uppercase tracking-wider shrink-0"
                                  style="color: {categoryColor(note.category)}; background: {categoryColor(note.category)}15; border: 1px solid {categoryColor(note.category)}30;"
                                >
                                  {note.category.replace("_", " ")}
                                </span>
                                <span class="text-xs text-slate-300 truncate">{note.title}</span>
                                <span class="text-slate-500 text-[10px] ml-auto shrink-0">→</span>
                              </button>
                            {/each}
                          </div>
                        {/if}
                        <!-- Code -->
                        {#if chunk.signature}
                          <p class="text-xs text-muted-foreground font-mono mb-1 italic">
                            {chunk.signature}
                          </p>
                        {/if}
                        <pre class="custom-scrollbar text-xs font-mono text-foreground/80 bg-black/30 rounded-md p-3 max-h-96 overflow-y-auto whitespace-pre-wrap break-words border border-border">{chunk.content}</pre>
                      </div>
                    {/if}
                  </div>
                {/each}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  /* Dark-theme scrollbar */
  .custom-scrollbar::-webkit-scrollbar {
    width: 6px;
  }
  .custom-scrollbar::-webkit-scrollbar-track {
    background: transparent;
  }
  .custom-scrollbar::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.08);
    border-radius: 3px;
  }
  .custom-scrollbar::-webkit-scrollbar-thumb:hover {
    background: rgba(255, 255, 255, 0.15);
  }

  /* Firefox */
  .custom-scrollbar {
    scrollbar-width: thin;
    scrollbar-color: rgba(255, 255, 255, 0.08) transparent;
  }

  /* Responsive: bottom sheet on narrow viewports */
  @media (max-width: 1023px) {
    .chunk-sidebar {
      position: fixed;
      top: auto;
      bottom: 0;
      left: 0;
      right: 0;
      width: 100%;
      max-height: 60vh;
      border-left: none;
      border-top: 1px solid hsl(var(--border));
      border-radius: 12px 12px 0 0;
    }
  }
</style>
