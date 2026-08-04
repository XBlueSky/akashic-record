<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { RefreshCw, Play, X, GitBranch, Globe, Loader, AlertTriangle, XCircle } from "@lucide/svelte";
  import { fetchSourcesOverview, reingest, resumeIngest, deleteRepo, isAuthError } from "$lib/api";
  import { connectEvents, onEvent } from "$lib/events";
  import type { SourcesOverviewResponse, SourceOverview } from "$lib/types";
  import * as AlertDialog from "$lib/components/ui/alert-dialog";
  import { toast } from "$lib/state/toast.svelte";
  import { t } from "svelte-i18n";
  import { goto } from "$app/navigation";

  let data = $state<SourcesOverviewResponse | null>(null);
  let loading = $state(true);

  // Stale-fetch generation guard — each load() call increments the generation;
  // the callback only writes back if the generation hasn't changed by the time
  // it resolves, discarding racing responses from prior fetches.
  let generation = 0;

  // Delete confirmation state
  let deleteTarget = $state<SourceOverview | null>(null);
  let deleteConfirmText = $state("");
  let deleteLoading = $state(false);
  let showDeleteDialog = $state(false);

  // Action loading state (keyed by source name)
  let actionLoading = $state<Record<string, boolean>>({});

  // SSE unsub handles
  let unsubJobUpdate: (() => void) | null = null;
  let unsubReposChanged: (() => void) | null = null;

  async function load() {
    const gen = ++generation;
    loading = true;
    try {
      const result = await fetchSourcesOverview();
      if (gen === generation) {
        data = result;
      }
    } catch (e: unknown) {
      if (gen === generation) {
        if (!isAuthError(e)) {
          toast.push(
            `Failed to load sources: ${e instanceof Error ? e.message : String(e)}`,
            "error",
          );
        }
      }
    } finally {
      if (gen === generation) {
        loading = false;
      }
    }
  }

  onMount(() => {
    load();

    connectEvents();

    unsubJobUpdate = onEvent("job_update", (raw) => {
      const evt = raw as {
        repo_name: string;
        status: string;
        processed_files: number | null;
        total_files: number | null;
      };
      if (!data) return;
      const idx = data.sources.findIndex((s) => s.name === evt.repo_name);
      if (idx >= 0) {
        const done = ["done", "completed", "failed"].includes(evt.status);
        // Clear action loading when job starts or finishes
        actionLoading = { ...actionLoading, [evt.repo_name]: false };
        if (done) {
          load();
        } else {
          data.sources[idx] = {
            ...data.sources[idx],
            status: "ingesting",
            active_job: {
              job_id: "",
              status: evt.status,
              processed_files: evt.processed_files,
              total_files: evt.total_files,
            },
          };
          data = { ...data };
        }
      }
    });

    unsubReposChanged = onEvent("repos_changed", () => load());
  });

  onDestroy(() => {
    unsubJobUpdate?.();
    unsubReposChanged?.();
    // Do NOT call disconnectEvents() — the Sidebar owns the SSE connection lifecycle.
  });

  async function handleSync(name: string) {
    actionLoading = { ...actionLoading, [name]: true };
    try {
      await reingest(name);
    } catch (e: unknown) {
      toast.push(
        `Sync failed for ${name}: ${e instanceof Error ? e.message : String(e)}`,
        "error",
      );
    } finally {
      // Clear the spinner deterministically once the trigger request settles.
      // The row flips to its real "ingesting" state via the SSE `job_update`
      // backstop; but if that event never arrives (SSE drop, the job failing
      // before its first update), relying on it alone would leave the spinner
      // stuck forever — so we always clear here.
      actionLoading = { ...actionLoading, [name]: false };
    }
  }

  async function handleResume(name: string) {
    actionLoading = { ...actionLoading, [name]: true };
    try {
      await resumeIngest(name);
    } catch (e: unknown) {
      toast.push(
        `Resume failed for ${name}: ${e instanceof Error ? e.message : String(e)}`,
        "error",
      );
    } finally {
      // Same as handleSync: clear deterministically rather than depending on a
      // future `job_update` SSE event that may never come.
      actionLoading = { ...actionLoading, [name]: false };
    }
  }

  function openDeleteDialog(source: SourceOverview) {
    deleteTarget = source;
    deleteConfirmText = "";
    deleteLoading = false;
    showDeleteDialog = true;
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    deleteLoading = true;
    try {
      await deleteRepo(deleteTarget.name);
      showDeleteDialog = false;
      deleteTarget = null;
      load();
    } catch (e: unknown) {
      if (!isAuthError(e)) {
        toast.push(e instanceof Error ? e.message : "Delete failed", "error");
      }
    } finally {
      deleteLoading = false;
    }
  }

  function timeAgo(iso: string): string {
    const diff = Date.now() - new Date(iso).getTime();
    const mins = Math.floor(diff / 60000);
    if (mins < 60) return `${mins}m ago`;
    const hrs = Math.floor(mins / 60);
    if (hrs < 24) return `${hrs}h ago`;
    const days = Math.floor(hrs / 24);
    return `${days}d ago`;
  }
</script>

<div class="p-6 max-w-7xl mx-auto">
  <!-- Header -->
  <div class="flex items-center justify-between mb-6">
    <h2 class="font-mono text-sm font-medium text-slate-200">{$t('sources.title')}</h2>
    {#if data}
      <div class="flex items-center gap-4 font-mono text-[10px]">
        <span class="text-slate-500">{$t('sources.total', { values: { count: data.total } })}</span>
        <span class="text-green-400/70">{$t('sources.healthy', { values: { count: data.healthy } })}</span>
        {#if data.stale > 0}<span class="text-amber-400/70">{$t('sources.stale', { values: { count: data.stale } })}</span>{/if}
        {#if data.failed > 0}<span class="text-red-400/70">{$t('sources.failed', { values: { count: data.failed } })}</span>{/if}
        {#if data.ingesting > 0}<span class="text-blue-400/70">{$t('sources.active', { values: { count: data.ingesting } })}</span>{/if}
      </div>
    {/if}
    <button
      class="font-mono text-[11px] text-blue-400 bg-blue-500/10 border border-blue-500/20 px-3 py-1.5 rounded-md hover:bg-blue-500/20 transition-colors"
      onclick={() => goto('/ingest')}
    >{$t('sources.addSource')}</button>
  </div>

  <!-- Table -->
  {#if loading}
    <!-- Skeleton table -->
    <table class="w-full font-mono text-[11px]">
      <thead>
        <tr class="border-b border-white/5">
          <th class="text-left text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3">{$t('sources.colSource')}</th>
          <th class="text-left text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3">{$t('sources.colStatus')}</th>
          <th class="text-left text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3">{$t('sources.colBranch')}</th>
          <th class="text-right text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3">{$t('sources.colChunks')}</th>
          <th class="text-right text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3">{$t('sources.colNotes')}</th>
          <th class="text-left text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3">{$t('sources.colSynced')}</th>
          <th class="text-right text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3 w-24"></th>
        </tr>
      </thead>
      <tbody>
        {#each Array(4) as _, i (i)}
          <tr class="border-b border-white/[0.03]">
            <td class="px-3 py-2.5"><div class="h-3 w-32 bg-white/[0.04] rounded animate-pulse"></div></td>
            <td class="px-3 py-2.5"><div class="h-3 w-14 bg-white/[0.04] rounded animate-pulse"></div></td>
            <td class="px-3 py-2.5"><div class="h-3 w-12 bg-white/[0.04] rounded animate-pulse"></div></td>
            <td class="px-3 py-2.5 text-right"><div class="h-3 w-8 bg-white/[0.04] rounded animate-pulse ml-auto"></div></td>
            <td class="px-3 py-2.5 text-right"><div class="h-3 w-6 bg-white/[0.04] rounded animate-pulse ml-auto"></div></td>
            <td class="px-3 py-2.5"><div class="h-3 w-14 bg-white/[0.04] rounded animate-pulse"></div></td>
            <td class="px-3 py-2.5"></td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else if data && data.sources.length > 0}
    <table class="w-full font-mono text-[11px]">
      <thead>
        <tr class="border-b border-white/5">
          <th class="text-left text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3">{$t('sources.colSource')}</th>
          <th class="text-left text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3">{$t('sources.colStatus')}</th>
          <th class="text-left text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3">{$t('sources.colBranch')}</th>
          <th class="text-right text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3">{$t('sources.colChunks')}</th>
          <th class="text-right text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3">{$t('sources.colNotes')}</th>
          <th class="text-left text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3">{$t('sources.colSynced')}</th>
          <th class="text-right text-[10px] text-slate-600 uppercase tracking-wider pb-2 px-3 w-24"></th>
        </tr>
      </thead>
      <tbody>
        {#each data.sources as source (source.name)}
          <tr
            class="border-b border-white/[0.03] hover:bg-white/[0.02] transition-colors group {source.status === 'failed' ? 'bg-red-500/[0.03]' : ''}"
          >
            <!-- Source Name -->
            <td class="px-3 py-2.5">
              <button
                class="flex items-center gap-1.5 text-blue-300 hover:text-blue-200 transition-colors bg-transparent border-none cursor-pointer p-0 text-left font-mono text-[11px]"
                onclick={() => goto('/r/' + encodeURIComponent(source.name))}
              >
                {#if source.source_type === "website"}
                  <Globe size={12} strokeWidth={1.5} class="shrink-0 opacity-50" />
                {:else}
                  <GitBranch size={12} strokeWidth={1.5} class="shrink-0 opacity-50" />
                {/if}
                {source.name}
              </button>
            </td>

            <!-- Status -->
            <td class="px-3 py-2.5">
              <div class="flex items-center gap-1.5">
                {#if source.status === "ingesting"}
                  <span class="inline-flex h-3 w-3 items-center justify-center text-blue-400">
                    <Loader size={10} strokeWidth={2} class="animate-spin" />
                  </span>
                  <span class="text-blue-400">{source.active_job?.status ?? $t('sources.statusIngesting')}</span>
                {:else if source.status === "failed"}
                  <XCircle size={12} strokeWidth={1.5} class="shrink-0 text-red-400" />
                  <span class="text-red-400">{$t('sources.statusFailed')}</span>
                {:else if source.status === "stale"}
                  <AlertTriangle size={12} strokeWidth={1.5} class="shrink-0 text-amber-400" />
                  <span class="text-amber-400">{$t('sources.statusStale')}</span>
                {:else}
                  <span class="inline-block w-[6px] h-[6px] rounded-full bg-green-400 shrink-0"></span>
                  <span class="text-slate-500">{$t('sources.statusHealthy')}</span>
                {/if}
              </div>
              {#if source.status === "ingesting" && source.active_job?.total_files}
                <div class="mt-1.5 h-[2px] w-full bg-white/5 rounded-full overflow-hidden">
                  <div
                    class="h-full bg-blue-500 rounded-full transition-all"
                    style="width: {((source.active_job.processed_files ?? 0) / source.active_job.total_files) * 100}%"
                  ></div>
                </div>
              {/if}
            </td>

            <!-- Branch -->
            <td class="px-3 py-2.5 text-slate-600">{source.branch ?? "—"}</td>

            <!-- Chunks -->
            <td class="px-3 py-2.5 text-right">{source.chunk_count.toLocaleString()}</td>

            <!-- Notes -->
            <td class="px-3 py-2.5 text-right">{source.note_count || "—"}</td>

            <!-- Synced / Error -->
            <td class="px-3 py-2.5 text-slate-600">
              {#if source.status === "failed"}
                <span class="text-red-400/70 text-[10px]">{source.last_error?.slice(0, 40) ?? "failed"}</span>
              {:else}
                {source.last_synced_at ? timeAgo(source.last_synced_at) : "—"}
              {/if}
            </td>

            <!-- Actions -->
            <td class="px-3 py-2.5 text-right">
              <div class="flex items-center justify-end gap-2 transition-opacity {source.status === 'failed' || source.status === 'stale' || source.status === 'ingesting' ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'}">
                {#if actionLoading[source.name]}
                  <Loader size={12} strokeWidth={2} class="animate-spin text-blue-400" />
                {:else if source.status === "ingesting"}
                  <!-- no actions during ingest -->
                {:else if source.status === "failed"}
                  {#if source.can_resume}
                    <button class="text-blue-400 hover:text-blue-300 bg-transparent border-none cursor-pointer p-0" title={$t('sources.actionResume')}
                      onclick={() => handleResume(source.name)}><Play size={12} /></button>
                  {/if}
                  <button class="text-slate-500 hover:text-blue-400 bg-transparent border-none cursor-pointer p-0" title={$t('sources.actionRetry')}
                    onclick={() => handleSync(source.name)}><RefreshCw size={12} /></button>
                  <button class="text-slate-500 hover:text-red-400 bg-transparent border-none cursor-pointer p-0" title={$t('sources.actionDelete')}
                    onclick={() => openDeleteDialog(source)}><X size={12} /></button>
                {:else}
                  <button class="text-slate-500 hover:text-blue-400 bg-transparent border-none cursor-pointer p-0" title={$t('sources.actionSync')}
                    onclick={() => handleSync(source.name)}><RefreshCw size={12} /></button>
                  <button class="text-slate-500 hover:text-red-400 bg-transparent border-none cursor-pointer p-0" title={$t('sources.actionDelete')}
                    onclick={() => openDeleteDialog(source)}><X size={12} /></button>
                {/if}
              </div>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else}
    <!-- Empty state -->
    <div class="text-center py-16 space-y-3">
      <p class="text-slate-400 font-mono text-xs">{$t('sources.emptyTitle')}</p>
      <div class="flex items-center justify-center gap-6 pt-2">
        <button
          class="flex items-center gap-2 font-mono text-[11px] text-slate-400 hover:text-blue-300 bg-white/[0.03] border border-white/5 px-4 py-2.5 rounded-md hover:bg-white/[0.05] transition-colors"
          onclick={() => goto('/ingest')}
        >
          <GitBranch size={14} strokeWidth={1.5} class="opacity-60" />
          {$t('sources.emptyGitlab')}
        </button>
        <button
          class="flex items-center gap-2 font-mono text-[11px] text-slate-400 hover:text-blue-300 bg-white/[0.03] border border-white/5 px-4 py-2.5 rounded-md hover:bg-white/[0.05] transition-colors"
          onclick={() => goto('/ingest')}
        >
          <Globe size={14} strokeWidth={1.5} class="opacity-60" />
          {$t('sources.emptyWebsite')}
        </button>
      </div>
    </div>
  {/if}
</div>

<!-- Delete Confirmation Dialog -->
<AlertDialog.Root bind:open={showDeleteDialog}>
  <AlertDialog.Content class="bg-[#0f1729]/80 backdrop-blur-xl border border-white/10 shadow-[0_8px_40px_-10px_rgba(0,0,0,0.6)]">
    <AlertDialog.Header>
      <AlertDialog.Title class="font-mono text-sm font-bold tracking-widest text-destructive">
        {$t('sources.deleteTitle')}
      </AlertDialog.Title>
      <AlertDialog.Description class="text-slate-400">
        {#if deleteTarget}
          {$t('sources.deleteDesc', { values: { name: deleteTarget.name, chunks: deleteTarget.chunk_count, notes: deleteTarget.note_count } })}
        {/if}
      </AlertDialog.Description>
    </AlertDialog.Header>
    <div class="space-y-2 py-2">
      <label for="delete-confirm-input" class="font-mono text-[10px] font-semibold tracking-widest uppercase text-slate-500">
        {$t('sources.deleteConfirmLabel')}
      </label>
      <input
        id="delete-confirm-input"
        bind:value={deleteConfirmText}
        placeholder={deleteTarget?.name ?? ""}
        class="w-full font-mono text-sm bg-white/5 border border-white/10 rounded-md px-3 py-1.5 text-slate-200 placeholder:text-slate-600 focus:border-red-500/50 focus:outline-none"
      />
    </div>
    <AlertDialog.Footer>
      <AlertDialog.Cancel
        style="background: transparent; border: none; color: rgb(148,163,184); box-shadow: none;"
      >{$t('common.cancel')}</AlertDialog.Cancel>
      <AlertDialog.Action
        onclick={confirmDelete}
        disabled={deleteConfirmText !== deleteTarget?.name || deleteLoading}
        class="bg-destructive/15 border border-destructive/30 text-destructive hover:bg-destructive/25 hover:shadow-[0_0_20px_rgba(248,81,73,0.3)] font-mono text-xs font-bold tracking-wide disabled:opacity-30"
      >
        {#if deleteLoading}
          <Loader size={12} class="animate-spin" />
        {/if}
        {$t('sources.deleteConfirm')}
      </AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
