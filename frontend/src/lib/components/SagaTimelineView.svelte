<script lang="ts">
  import { goto } from '$app/navigation';
  import { fetchSagaDetail, isAuthError } from '$lib/api';
  import type { SagaDetail, SagaTimelineEntry, Category } from '$lib/types/index.js';
  import { Button } from '$lib/components/ui/button/index.js';
  import { fade } from 'svelte/transition';

  let {
    repoName,
    sagaId,
  }: {
    repoName: string;
    sagaId: string;
  } = $props();

  let detail = $state<SagaDetail | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Stale-fetch generation guard
  let fetchGen = 0;

  const CATEGORY_COLORS: Record<Category, string> = {
    ARCHITECTURE: 'bg-blue-500/20 text-blue-400 border-blue-500/40',
    BUG_FIX: 'bg-red-500/20 text-red-400 border-red-500/40',
    CONFIG: 'bg-amber-500/20 text-amber-400 border-amber-500/40',
    ONBOARDING: 'bg-green-500/20 text-green-400 border-green-500/40',
    DECISION: 'bg-purple-500/20 text-purple-400 border-purple-500/40',
  };

  const DOT_COLORS: Record<Category, string> = {
    ARCHITECTURE: 'bg-blue-400',
    BUG_FIX: 'bg-red-400',
    CONFIG: 'bg-amber-400',
    ONBOARDING: 'bg-green-400',
    DECISION: 'bg-purple-400',
  };

  const STATUS_BADGES: Record<string, string> = {
    active: 'bg-green-500/20 text-green-400',
    resolved: 'bg-blue-500/20 text-blue-400',
    archived: 'bg-zinc-500/20 text-zinc-400',
    open: 'bg-amber-500/20 text-amber-400',
  };

  function formatDate(dateStr: string | null): string {
    if (!dateStr) return 'unknown';
    return dateStr.slice(0, 10);
  }

  function isSuperseded(entry: SagaTimelineEntry): boolean {
    return entry.superseded_by !== null;
  }

  async function loadSaga(): Promise<void> {
    const gen = ++fetchGen;
    loading = true;
    error = null;
    try {
      const result = await fetchSagaDetail(repoName, sagaId);
      if (gen !== fetchGen) return; // stale — discard
      detail = result;
    } catch (e: unknown) {
      if (gen !== fetchGen) return;
      if (!isAuthError(e)) {
        error = e instanceof Error ? e.message : String(e);
        detail = null;
      }
    } finally {
      if (gen === fetchGen) loading = false;
    }
  }

  // Re-fetch whenever repoName or sagaId changes
  $effect(() => {
    // Track both as dependencies
    const _repo = repoName;
    const _id = sagaId;
    void _repo;
    void _id;
    loadSaga();
  });

  function goBack(): void {
    goto('/r/' + encodeURIComponent(repoName) + '/sagas');
  }
</script>

<div class="mx-auto w-full max-w-3xl">
  <Button
    variant="ghost"
    size="sm"
    class="mb-4 font-mono text-xs text-muted-foreground"
    onclick={goBack}
  >
    &larr; Back
  </Button>

  {#if loading}
    <p class="py-12 text-center font-mono text-sm text-muted-foreground" transition:fade={{ duration: 150 }}>
      Loading saga...
    </p>
  {:else if error}
    <p class="py-12 text-center font-mono text-sm text-destructive" transition:fade={{ duration: 150 }}>
      {error}
    </p>
  {:else if detail}
    <div transition:fade={{ duration: 150 }}>
      <!-- Saga Header -->
      <div class="mb-8 rounded-md border border-zinc-800 bg-zinc-900/50 p-5">
        <div class="flex items-start justify-between gap-4">
          <div class="min-w-0 flex-1">
            <h2 class="font-mono text-lg font-semibold text-zinc-100">
              {detail.saga.name ?? 'Untitled Saga'}
            </h2>
            {#if detail.saga.source_ref}
              <p class="mt-1 font-mono text-xs text-muted-foreground">
                ref: {detail.saga.source_ref}
              </p>
            {/if}
          </div>
          <span class="shrink-0 rounded border px-2 py-0.5 font-mono text-xs {STATUS_BADGES[detail.saga.status] ?? 'bg-zinc-500/20 text-zinc-400'}">
            {detail.saga.status}
          </span>
        </div>
        {#if detail.saga.summary}
          <p class="mt-3 text-sm leading-relaxed text-zinc-400">{detail.saga.summary}</p>
        {/if}
        <p class="mt-2 font-mono text-xs text-zinc-600">
          {detail.timeline.length} note{detail.timeline.length === 1 ? '' : 's'}
        </p>
      </div>

      <!-- Timeline -->
      {#if detail.timeline.length === 0}
        <p class="py-8 text-center font-mono text-sm text-muted-foreground">
          No notes in this saga yet.
        </p>
      {:else}
        <div class="relative ml-4 border-l border-zinc-700 pl-6">
          {#each detail.timeline as entry (entry.uuid)}
            <div class="relative mb-6 last:mb-0 {isSuperseded(entry) ? 'opacity-50' : ''}">
              <!-- Dot -->
              <div
                class="absolute -left-[calc(1.5rem+0.3125rem)] top-1 h-2.5 w-2.5 rounded-full {isSuperseded(entry) ? 'bg-zinc-600' : (DOT_COLORS[entry.category] ?? 'bg-blue-400')}"
              ></div>

              <!-- Date -->
              <p class="font-mono text-xs text-zinc-500">
                {formatDate(entry.valid_at ?? entry.created_at)}
              </p>

              <!-- Category badge -->
              <span
                class="mt-1 inline-block rounded border px-1.5 py-0.5 font-mono text-[10px] uppercase {CATEGORY_COLORS[entry.category] ?? 'bg-zinc-500/20 text-zinc-400 border-zinc-500/40'}"
              >
                {entry.category}
              </span>

              <!-- Title -->
              {#if entry.title}
                <h3
                  class="mt-1.5 text-sm font-medium {isSuperseded(entry) ? 'text-zinc-500 line-through' : 'text-zinc-200'}"
                >
                  {entry.title}
                </h3>
              {/if}

              <!-- Summary -->
              {#if entry.summary}
                <p class="mt-1 text-sm leading-relaxed text-zinc-400">{entry.summary}</p>
              {/if}

              <!-- Superseded indicator -->
              {#if entry.superseded_by}
                <p class="mt-1 font-mono text-xs text-zinc-600">
                  superseded &rarr; {entry.superseded_by}
                </p>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/if}
</div>
