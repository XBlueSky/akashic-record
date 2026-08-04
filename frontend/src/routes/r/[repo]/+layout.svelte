<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import {
    RefreshCw, GitBranch, Globe, ChevronDown, Trash2,
    Loader, Check, Database, FolderTree, FileText, List,
  } from "@lucide/svelte";
  import { t } from "svelte-i18n";
  import { fetchRepoDetail, deleteRepo, reingest, fetchBranches, isAuthError } from "$lib/api";
  import type { RepoDetail, Branch, IngestionJob } from "$lib/api";
  import { connectEvents, onEvent } from "$lib/events";
  import { toast } from "$lib/state/toast.svelte";
  import ChunkSidebar from "$lib/components/ChunkSidebar.svelte";
  import NoteEditor from "$lib/components/NoteEditor.svelte";
  import * as Popover from "$lib/components/ui/popover/index.js";
  import * as Command from "$lib/components/ui/command/index.js";
  import * as AlertDialog from "$lib/components/ui/alert-dialog/index.js";

  let { children, data } = $props();

  // ── Repo name from URL ────────────────────────────────────────────────────
  // page.params.repo is string | undefined in LayoutParams (SvelteKit merges
  // all sibling route params as optional). The actual /r/[repo]/* routes always
  // provide it, so we assert non-null with a fallback empty string for safety.
  let repoName = $derived(page.params.repo ?? "");

  // ── Server data ───────────────────────────────────────────────────────────
  // `detail` is mutable local state (the SSE job_update handler refreshes it on
  // job completion), but its source of truth is the layout load data
  // (+layout.ts), which fetches repo detail once per [repo] navigation — so we
  // do NOT re-fetch it here (no double-fetch). The $effect below seeds and
  // re-syncs it from `data.detail`; it is intentionally NOT a $derived because
  // SSE mutates it independently.
  let detail = $state<RepoDetail | null>(null);
  let branches = $state<Branch[]>([]);
  let selectedBranch = $state<string | null>(null);
  let loading = $state(false);
  let headerError = $state<string | null>(null);

  // ── Action state ──────────────────────────────────────────────────────────
  let reingestLoading = $state(false);
  let deleteLoading = $state(false);
  let showDeleteModal = $state(false);
  let deleteConfirmText = $state("");
  let branchOpen = $state(false);
  let branchFilter = $state("");

  // ── Active job (lightweight polling from header) ──────────────────────────
  let activeJob = $state<IngestionJob | null>(null);
  let isJobActive = $derived(
    activeJob != null && !["done", "completed", "failed"].includes(activeJob.status)
  );

  // ── Load generation guard ─────────────────────────────────────────────────
  // Plain let — guard token is never read by markup, only by async callbacks.
  let loadGen = 0;

  // Branch list is the only header data still fetched component-side: it is
  // not part of repo detail (and gitlab-only). `detail` itself comes from
  // +layout.ts load data, so there is no second fetchRepoDetail here.
  async function loadBranches(repo: string, d: RepoDetail | null) {
    const gen = ++loadGen;
    const superseded = () => loadGen !== gen;

    // Seed the active job from the load-data detail (a still-running ingest).
    if (d?.latest_job && !["done", "completed", "failed"].includes(d.latest_job.status)) {
      activeJob = d.latest_job;
    }

    if (d?.source_type !== "gitlab") return;
    try {
      const br = await fetchBranches(repo);
      if (superseded()) return;
      branches = br;
      if (!selectedBranch && d.latest_job?.git_ref) {
        selectedBranch = d.latest_job.git_ref;
      }
    } catch {
      if (!superseded()) branches = [];
    }
  }

  // React when the load data (and thus repo) changes. SvelteKit re-runs the
  // layout load on every [repo] navigation, so `data.detail` is fresh here.
  $effect(() => {
    const repo = repoName;
    const d = data.detail;
    if (!repo) return;
    detail = d;
    branches = [];
    selectedBranch = null;
    branchOpen = false;
    branchFilter = "";
    activeJob = null;
    headerError = null;
    loadBranches(repo, d);
  });

  // ── Live job progress via SSE ───────────────────────────────────────────
  // The legacy RepoDetailView advanced the reingest progress bar in real time
  // by subscribing to `job_update`. Mirror Sidebar.svelte's pattern.
  //
  // SINGLETON TRAP: connectEvents() shares ONE EventSource across the whole app
  // and disconnectEvents() closes it for EVERYONE (incl. the always-mounted
  // Sidebar). This layout unmounts on every navigation away from /r/[repo], so
  // it must NEVER call disconnectEvents() — only remove its own listener via
  // unsub(). The root app + Sidebar own the connection lifecycle.
  let unsubJobUpdate: (() => void) | null = null;

  onMount(() => {
    connectEvents();
    unsubJobUpdate = onEvent("job_update", (raw) => {
      const data = raw as {
        repo_name: string;
        status: string;
        processed_files: number | null;
        total_files: number | null;
        total_chunks?: number | null;
      };
      // Only react to events for the repo this layout is currently showing.
      if (data.repo_name !== page.params.repo) return;

      if (["done", "completed", "failed"].includes(data.status)) {
        // Job finished — drop the progress bar and refresh stats (chunk/module
        // counts, latest_job) from the backend. Guard isAuthError so a 401 does
        // not surface a toast (handled globally by the app shell).
        activeJob = null;
        fetchRepoDetail(data.repo_name)
          .then((d) => {
            // Ignore if the user navigated to a different repo meanwhile.
            if (data.repo_name === page.params.repo) detail = d;
          })
          .catch((e: unknown) => {
            if (!isAuthError(e)) {
              console.warn("Failed to refresh repo detail after job:", e);
            }
          });
      } else {
        // In-progress — advance the progress bar in place.
        activeJob = {
          ...(activeJob ?? {
            id: "",
            repo_name: data.repo_name,
            git_ref: selectedBranch ?? "",
            error_message: null,
            started_at: new Date().toISOString(),
            completed_at: null,
          }),
          status: data.status,
          processed_files: data.processed_files ?? activeJob?.processed_files ?? 0,
          total_files: data.total_files ?? activeJob?.total_files ?? 0,
          total_chunks: data.total_chunks ?? activeJob?.total_chunks ?? 0,
        };
      }
    });
  });

  onDestroy(() => {
    // Remove ONLY our listener. Do NOT disconnectEvents() — see SINGLETON TRAP.
    unsubJobUpdate?.();
  });

  // ── Reingest ──────────────────────────────────────────────────────────────
  async function handleReingest() {
    reingestLoading = true;
    try {
      await reingest(repoName, selectedBranch ?? undefined);
      activeJob = {
        id: "",
        repo_name: repoName,
        git_ref: selectedBranch ?? "",
        status: "pending",
        total_files: 0,
        processed_files: 0,
        total_chunks: 0,
        error_message: null,
        started_at: new Date().toISOString(),
        completed_at: null,
      };
      toast.push(String($t("repo.reingestStarted") ?? "Re-ingest started."), "info");
    } catch (e: unknown) {
      if (!isAuthError(e)) {
        const msg = e instanceof Error ? e.message : String(e);
        toast.push(msg, "error");
      }
    } finally {
      reingestLoading = false;
    }
  }

  // ── Delete ────────────────────────────────────────────────────────────────
  async function handleDelete() {
    deleteLoading = true;
    try {
      await deleteRepo(repoName);
      showDeleteModal = false;
      toast.push(String($t("repo.deleted") ?? `Repository "${repoName}" deleted.`), "info");
      goto("/");
    } catch (e: unknown) {
      if (!isAuthError(e)) {
        const msg = e instanceof Error ? e.message : String(e);
        toast.push(msg, "error");
      }
      showDeleteModal = false;
    } finally {
      deleteLoading = false;
    }
  }

  // ── Branch selector helpers ───────────────────────────────────────────────
  let filteredBranches = $derived(
    branchFilter
      ? branches.filter((b) => b.name.toLowerCase().includes(branchFilter.toLowerCase()))
      : branches
  );

  function selectBranch(name: string) {
    selectedBranch = name;
    branchOpen = false;
    branchFilter = "";
  }

  // ── View-switcher ─────────────────────────────────────────────────────────
  // Preserve ?branch= query param when switching tabs.
  function tabHref(sub: "timeline" | "graph" | "modules"): string {
    const branch = page.url.searchParams.get("branch");
    const params = branch ? `?branch=${encodeURIComponent(branch)}` : "";
    return `/r/${repoName}/${sub}${params}`;
  }

  function isTabActive(sub: string): boolean {
    return page.url.pathname.endsWith(`/${sub}`);
  }

  // ── ChunkSidebar: URL-driven panel (?chunk=, ?module=, ?doc=, ?smode=) ──
  let chunkId = $derived(page.url.searchParams.get("chunk"));
  // moduleId: graph page sets ?module= when opening chunk panel (Unit C)
  // TODO: graph page sets this in Unit C
  let panelModuleId = $derived(page.url.searchParams.get("module") ?? "");
  // moduleName: graph page sets ?modname= for the sidebar header (Unit C)
  // TODO: graph page sets this in Unit C
  let panelModuleName = $derived(page.url.searchParams.get("modname") ?? panelModuleId);
  // sectionMode: graph page sets ?smode=1 when a doc section is selected (Unit C)
  // TODO: graph page sets this in Unit C
  let panelSectionMode = $derived(page.url.searchParams.get("smode") === "1");
  // documentTitle: graph page sets ?doc= (Unit C)
  // TODO: graph page sets this in Unit C
  let panelDocTitle = $derived(page.url.searchParams.get("doc") ?? "");

  function closeChunkPanel() {
    const u = new URL(page.url.href);
    u.searchParams.delete("chunk");
    u.searchParams.delete("module");
    u.searchParams.delete("modname");
    u.searchParams.delete("smode");
    u.searchParams.delete("doc");
    // URLSearchParams.delete() never leaves a bare "?" — u.search is "" or "?k=v".
    goto(u.pathname + u.search);
  }

  // ── NoteEditor: URL-driven panel (?edit=<uuid>) ───────────────────────────
  // Mirrors the ?chunk= ChunkSidebar pattern above.
  let editUuid = $derived(page.url.searchParams.get("edit"));

  function closeEditPanel() {
    const u = new URL(page.url.href);
    u.searchParams.delete("edit");
    goto(u.pathname + u.search);
  }

  // ── Stats helpers ─────────────────────────────────────────────────────────
  function timeAgo(dateStr: string | null): string {
    if (!dateStr) return $t("repo.never") || "never";
    const diff = Date.now() - new Date(dateStr).getTime();
    const mins = Math.floor(diff / 60000);
    if (mins < 1) return $t("repo.justNow") || "just now";
    if (mins < 60) return $t("repo.minsAgo", { values: { mins } }) || `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return $t("repo.hoursAgo", { values: { hours } }) || `${hours}h ago`;
    return $t("repo.daysAgo", { values: { days: Math.floor(hours / 24) } }) || `${Math.floor(hours / 24)}d ago`;
  }

  function progressPercent(job: IngestionJob): number {
    if (!job.total_files || job.total_files === 0) return 0;
    return Math.round((job.processed_files / job.total_files) * 100);
  }
</script>

<div class="flex h-full w-full flex-col overflow-hidden">
  <!-- ── Repo Header ────────────────────────────────────────────────────── -->
  <div class="shrink-0 border-b border-border/50 bg-card/80 backdrop-blur-xl px-4 py-3">
    <!-- Top row: icon + name + branch + actions -->
    <div class="flex items-center gap-3 min-w-0">
      {#if detail?.source_type === "website"}
        <Globe size={16} strokeWidth={1.5} class="text-primary shrink-0" />
      {:else}
        <GitBranch size={16} strokeWidth={1.5} class="text-primary shrink-0" />
      {/if}

      <h1 class="font-mono text-base font-bold tracking-wide truncate min-w-0">
        {repoName}
      </h1>

      {#if detail?.source_type === "website"}
        <span class="font-mono text-[10px] font-semibold tracking-widest border border-border rounded px-2 py-0.5 text-muted-foreground shrink-0">
          {$t("repo.website") || "website"}
        </span>
      {:else if detail?.source_type === "gitlab" && branches.length > 0}
        <!-- Branch dropdown -->
        <Popover.Root bind:open={branchOpen}>
          <Popover.Trigger>
            <button
              class="inline-flex items-center gap-1.5 rounded-md border border-emerald-500/20 bg-emerald-500/8 px-2.5 py-1 font-mono text-xs font-medium text-emerald-500 transition-all hover:bg-emerald-500/14 hover:shadow-[0_0_12px_rgba(16,185,129,0.15)] shrink-0"
            >
              <GitBranch size={12} strokeWidth={1.5} />
              {selectedBranch || "main"}
              <ChevronDown size={12} />
            </button>
          </Popover.Trigger>
          <Popover.Content class="w-56 p-0" align="start">
            <Command.Root shouldFilter={false}>
              <Command.Input
                placeholder={$t("repo.branchFilter") || "Filter branches…"}
                bind:value={branchFilter}
              />
              <Command.List>
                <Command.Empty>{$t("repo.noBranches") || "No branches found."}</Command.Empty>
                <Command.Group>
                  {#each filteredBranches as b (b.name)}
                    <Command.Item
                      value={b.name}
                      onSelect={() => selectBranch(b.name)}
                      class={selectedBranch === b.name ? "text-emerald-500 bg-emerald-500/8" : ""}
                    >
                      <GitBranch size={12} strokeWidth={1.5} />
                      {b.name}
                      {#if selectedBranch === b.name}
                        <Check size={12} class="ml-auto" />
                      {/if}
                    </Command.Item>
                  {/each}
                </Command.Group>
              </Command.List>
            </Command.Root>
          </Popover.Content>
        </Popover.Root>
      {/if}

      <!-- Spacer -->
      <div class="flex-1"></div>

      <!-- Action buttons -->
      <div class="flex items-center gap-1.5 shrink-0">
        <!-- Reingest -->
        <button
          onclick={handleReingest}
          disabled={reingestLoading || isJobActive}
          class="inline-flex items-center gap-1.5 rounded-md border border-blue-500/30 bg-transparent px-3 py-1.5 font-mono text-xs font-bold tracking-wide text-blue-400 transition-all hover:bg-blue-500/15 hover:border-blue-500/50 hover:text-blue-300 hover:shadow-[0_0_12px_rgba(59,130,246,0.25)] disabled:opacity-40 disabled:cursor-not-allowed"
        >
          <RefreshCw size={14} strokeWidth={2} class={reingestLoading ? "animate-spin" : ""} />
          {$t("repo.reingest") || "Re-ingest"}
        </button>

        <!-- Delete -->
        <button
          onclick={() => { deleteConfirmText = ""; showDeleteModal = true; }}
          disabled={isJobActive}
          class="inline-flex items-center justify-center rounded-md border-0 bg-transparent p-1.5 text-muted-foreground/50 transition-all hover:text-destructive hover:shadow-[0_0_16px_rgba(248,81,73,0.3)] disabled:opacity-40 disabled:cursor-not-allowed"
          title={$t("repo.purgeTitle") || "Delete repository"}
        >
          <Trash2 size={14} strokeWidth={1.5} />
        </button>
      </div>
    </div>

    <!-- Stats row -->
    {#if detail}
      <div class="mt-2 flex items-center gap-2 font-mono text-xs tracking-wider text-slate-400 flex-wrap">
        {#if detail.source_type === "website"}
          <span class="inline-flex items-center gap-1">
            <FileText size={12} strokeWidth={1.5} />
            {detail.total_documents} {$t("repo.documents") || "documents"}
          </span>
          <span class="h-[3px] w-[3px] rounded-full bg-muted-foreground/40"></span>
          <span class="inline-flex items-center gap-1">
            <List size={12} strokeWidth={1.5} />
            {detail.total_sections} {$t("repo.sections") || "sections"}
          </span>
        {:else}
          <span class="inline-flex items-center gap-1">
            <Database size={12} strokeWidth={1.5} />
            {$t("repo.chunks", { values: { count: detail.total_chunks } }) || `${detail.total_chunks} chunks`}
          </span>
          <span class="h-[3px] w-[3px] rounded-full bg-muted-foreground/40"></span>
          <span class="inline-flex items-center gap-1">
            <FolderTree size={12} strokeWidth={1.5} />
            {$t("repo.modules", { values: { count: detail.total_modules } }) || `${detail.total_modules} modules`}
          </span>
        {/if}
        {#if detail.latest_job}
          <span class="h-[3px] w-[3px] rounded-full bg-muted-foreground/40"></span>
          <span>{$t("repo.lastIndexed", { values: { time: timeAgo(detail.latest_job.completed_at || detail.latest_job.started_at) } }) || `indexed ${timeAgo(detail.latest_job.completed_at || detail.latest_job.started_at)}`}</span>
        {/if}
      </div>
    {:else if loading}
      <!-- Skeleton stats -->
      <div class="mt-2 flex items-center gap-2">
        <div class="h-3 w-24 bg-white/[0.04] rounded animate-pulse"></div>
        <div class="h-3 w-20 bg-white/[0.04] rounded animate-pulse"></div>
      </div>
    {:else if headerError}
      <p class="mt-1.5 font-mono text-xs text-destructive/70">{headerError}</p>
    {/if}

    <!-- Active job progress bar -->
    {#if isJobActive && activeJob}
      <div class="mt-2 space-y-1">
        <div class="flex items-center justify-between font-mono text-xs">
          <span class="inline-flex items-center gap-1.5 font-semibold text-primary">
            <Loader size={12} class="animate-spin" />
            {activeJob.status.toUpperCase()}
          </span>
          {#if activeJob.total_files > 0}
            <span class="text-muted-foreground">
              {activeJob.processed_files}/{activeJob.total_files} files
              ({progressPercent(activeJob)}%)
            </span>
          {/if}
        </div>
        <div class="h-1 w-full overflow-hidden rounded-full bg-white/10">
          <div
            class="h-full bg-primary transition-all duration-300"
            style="width: {progressPercent(activeJob)}%"
          ></div>
        </div>
      </div>
    {/if}

    <!-- View-switcher tabs -->
    <nav class="mt-3 flex items-center gap-1" aria-label="Repository views">
      {#each [
        { id: "timeline", label: $t("repo.timeline") || "Timeline" },
        { id: "graph",    label: $t("repo.graphTab") || "Graph" },
        { id: "modules",  label: $t("repo.modulesTab") || "Modules" },
      ] as tab (tab.id)}
        <a
          href={tabHref(tab.id as "timeline" | "graph" | "modules")}
          class="rounded-md px-3 py-1 font-mono text-xs font-semibold tracking-wide transition-all {isTabActive(tab.id)
            ? 'bg-blue-500/10 text-blue-400 border border-blue-500/30 shadow-[0_0_10px_rgba(59,130,246,0.1)]'
            : 'text-slate-400 hover:text-slate-200 hover:bg-white/5 border border-transparent'}"
          aria-current={isTabActive(tab.id) ? "page" : undefined}
        >
          {tab.label}
        </a>
      {/each}
    </nav>
  </div>

  <!-- ── Content area (relative so ChunkSidebar can position absolutely) ── -->
  <div class="relative flex-1 overflow-hidden">
    {@render children()}

    <!-- ── Right-hand URL-driven panel: NoteEditor (?edit=) takes precedence
         over ChunkSidebar (?chunk=). Both are absolute top-0 right-0 z-10, so
         only ONE may render at a time. Precedence (not URL juggling) keeps the
         ?chunk= context in the URL while the editor is open, so closing the
         editor (closeEditPanel removes only ?edit=) makes the ChunkSidebar
         reappear with its prior selection intact. ──────────────────────── -->
    {#if editUuid}
      <NoteEditor
        noteUuid={editUuid}
        repoName={repoName}
        onclose={closeEditPanel}
        onsaved={closeEditPanel}
      />
    {:else if chunkId && panelModuleId}
      <ChunkSidebar
        moduleId={panelModuleId}
        moduleName={panelModuleName}
        focusChunkId={chunkId}
        sectionMode={panelSectionMode}
        selectedSection={null}
        sourceRepo={repoName}
        documentTitle={panelDocTitle}
        onclose={closeChunkPanel}
        onselectnote={(uuid) => {
          const u = new URL(page.url.href);
          u.searchParams.set("edit", uuid);
          goto(u.pathname + u.search);
        }}
      />
    {/if}
  </div>
</div>

<!-- ── Delete confirmation dialog ────────────────────────────────────────── -->
<!-- Real alert-dialog: bits-ui AlertDialog blocks Escape + outside-click
     dismissal by design — correct for a destructive irreversible action. The
     repo-name confirmation gate (deleteConfirmText !== repoName) is preserved.
     Custom buttons in Footer (not AlertDialog.Action) so the gated/disabled
     Delete button does not auto-close before the gate passes. -->
<AlertDialog.Root bind:open={showDeleteModal}>
  <AlertDialog.Content class="bg-[#0f1729]/80 backdrop-blur-xl border border-white/10 shadow-[0_8px_40px_-10px_rgba(0,0,0,0.6)]">
    <AlertDialog.Header>
      <AlertDialog.Title class="font-mono text-sm font-bold tracking-widest text-destructive">
        {$t("repo.purgeTitle") || "Delete repository"}
      </AlertDialog.Title>
      <AlertDialog.Description class="text-slate-400">
        {#if detail}
          {$t("repo.purgeDesc", { values: { name: detail.name, chunks: detail.total_chunks, modules: detail.total_modules } }) || `Permanently delete "${detail.name}"? This will remove ${detail.total_chunks} chunks across ${detail.total_modules} modules.`}
        {:else}
          {$t("repo.purgeDescShort") || `Permanently delete "${repoName}"? This cannot be undone.`}
        {/if}
      </AlertDialog.Description>
    </AlertDialog.Header>
    <div class="space-y-2 py-2">
      <label for="delete-confirm-input" class="font-mono text-[10px] font-semibold tracking-widest uppercase text-muted-foreground">
        {$t("repo.purgeConfirmLabel") || `Type "${repoName}" to confirm`}
      </label>
      <input
        id="delete-confirm-input"
        type="text"
        placeholder={repoName}
        bind:value={deleteConfirmText}
        autocomplete="off"
        spellcheck={false}
        class="w-full rounded-md border border-white/10 bg-white/5 px-3 py-2 font-mono text-sm text-foreground placeholder-muted-foreground focus:border-destructive/50 focus:outline-none"
      />
    </div>
    <AlertDialog.Footer>
      <button
        onclick={() => (showDeleteModal = false)}
        class="rounded-md bg-transparent px-4 py-2 font-mono text-sm text-slate-400 hover:text-foreground"
      >
        {$t("common.cancel") || "Cancel"}
      </button>
      <button
        onclick={handleDelete}
        disabled={deleteConfirmText !== repoName || deleteLoading}
        class="inline-flex items-center gap-2 rounded-md border border-destructive/30 bg-destructive/15 px-4 py-2 font-mono text-xs font-bold tracking-wide text-destructive hover:bg-destructive/25 hover:shadow-[0_0_20px_rgba(248,81,73,0.3)] disabled:opacity-30 disabled:cursor-not-allowed"
      >
        {#if deleteLoading}
          <Loader size={12} class="animate-spin" />
        {/if}
        {$t("repo.purgeConfirm") || "Delete"}
      </button>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
