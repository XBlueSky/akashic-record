<script lang="ts">
  import { tick } from "svelte";
  import { fly } from "svelte/transition";
  import { fetchNotes, deleteNote, isAuthError } from "$lib/api";
  import NoteCard from "$lib/components/NoteCard.svelte";
  import { Button } from "$lib/components/ui/button";
  import type { Note, Category } from "$lib/types/index.js";
  import { t } from "svelte-i18n";
  import { toast } from "$lib/state/toast.svelte.js";

  let { repoName, branch = null, category = null, highlightNoteId = null, canEdit = false, canDelete = false, onedit, ondeleted }: {
    repoName: string;
    branch: string | null;
    category: Category | null;
    highlightNoteId?: string | null;
    canEdit?: boolean;
    canDelete?: boolean;
    onedit?: (uuid: string) => void;
    ondeleted?: () => void;
  } = $props();

  let notes: Note[] = $state([]);
  let total = $state(0);
  let page = $state(1);
  let limit = $state(20);
  let loading = $state(false);
  let error: string | null = $state(null);

  let totalPages = $derived(Math.max(1, Math.ceil(total / limit)));
  let hasNext = $derived(page < totalPages);
  let hasPrev = $derived(page > 1);

  $effect(() => {
    // Track reactive deps — reset page on filter change
    repoName; branch; category;
    page = 1;
    fetchPage();
  });

  async function fetchPage() {
    loading = true;
    error = null;
    try {
      const result = await fetchNotes(repoName, {
        branch: branch ?? undefined,
        category: category ?? undefined,
        page,
        limit,
      });
      notes = result.items;
      total = result.total;
    } catch (e: unknown) {
      if (!isAuthError(e)) {
        error = e instanceof Error ? e.message : String(e);
        notes = [];
        total = 0;
      }
    } finally {
      loading = false;
    }

    // Page-underflow recovery: deleting the last note on page N>1 returns an
    // empty page while `page` stays at N, stranding the user (the empty branch
    // hides pagination). Step back toward the last page that still has data.
    // `totalPages` derives from the freshly-updated `total`, so this converges
    // downward (never increases `page`) and stops once we land on data or page 1.
    if (notes.length === 0 && page > 1 && !error) {
      page = Math.max(1, totalPages);
      await fetchPage();
      return;
    }

    // Auto-scroll to highlighted note if present
    if (notes.length > 0 && highlightNoteId) {
      await tick();
      const el = document.getElementById(`note-${highlightNoteId}`);
      if (el) {
        el.scrollIntoView({ behavior: "smooth", block: "center" });
      }
    }
  }

  function nextPage() {
    if (hasNext) {
      page++;
      fetchPage();
    }
  }

  function prevPage() {
    if (hasPrev) {
      page--;
      fetchPage();
    }
  }
</script>

<div class="relative mx-auto w-full max-w-4xl">
  {#if loading && notes.length === 0}
    <p class="py-12 text-center font-mono text-sm text-muted-foreground">{$t("timeline.loading")}</p>
  {:else if error}
    <p class="py-12 text-center font-mono text-sm text-destructive">{error}</p>
  {:else if notes.length === 0}
    <p class="py-12 text-center font-mono text-sm text-muted-foreground">{$t("timeline.noNotes")}</p>
  {:else}
    <div class="mb-4 flex items-center justify-between font-mono text-xs text-muted-foreground">
      <span>{$t("timeline.noteCount", { values: { count: total } })}</span>
      <span>{$t("timeline.page", { values: { page, total: totalPages } })}</span>
    </div>
    <div class="flex flex-col gap-2">
      {#each notes as n, i (n.uuid)}
        <div in:fly={{ y: 20, duration: 300, delay: i * 60 }}>
          <NoteCard
            note={n}
            highlight={highlightNoteId === n.uuid}
            {canEdit}
            {canDelete}
            {onedit}
            ondelete={async (uuid) => {
              if (!confirm("Delete this note? This cannot be undone.")) return;
              try {
                await deleteNote(repoName, uuid);
                ondeleted?.();
                fetchPage();
              } catch (e) {
                if (isAuthError(e)) return;
                toast.push(e instanceof Error ? e.message : "Delete failed", "error");
              }
            }}
          />
        </div>
      {/each}
    </div>
    {#if totalPages > 1}
      <div class="flex items-center justify-center gap-4 py-6">
        <Button variant="outline" size="sm" disabled={!hasPrev} onclick={prevPage}>
          {$t("timeline.previous")}
        </Button>
        <span class="font-mono text-sm text-secondary-foreground">{page} / {totalPages}</span>
        <Button variant="outline" size="sm" disabled={!hasNext} onclick={nextPage}>
          {$t("timeline.next")}
        </Button>
      </div>
    {/if}
  {/if}

  {#if loading && notes.length > 0}
    <div class="absolute inset-0 flex items-center justify-center rounded-md bg-background/80 font-mono text-sm text-muted-foreground backdrop-blur-sm">
      {$t("common.loading")}
    </div>
  {/if}
</div>
