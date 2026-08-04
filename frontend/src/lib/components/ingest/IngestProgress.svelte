<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { fetchIngestStatus, isAuthError } from "$lib/api";
  import { connectEvents, onEvent } from "$lib/events";
  import type { IngestionJob } from "$lib/types/index.js";
  import { t } from "svelte-i18n";
  import * as Card from "$lib/components/ui/card";
  import { Button } from "$lib/components/ui/button";
  import { Progress } from "$lib/components/ui/progress";
  import { ArrowLeft, ArrowRight, Check, X, Loader, Circle } from "@lucide/svelte";

  let {
    repoName,
    onBack,
    onDone,
  }: {
    repoName: string;
    onBack: () => void;
    onDone: (repoName: string) => void;
  } = $props();

  let job = $state<IngestionJob | null>(null);
  let error = $state<string | null>(null);
  let unsubJobUpdate: (() => void) | null = null;

  let STAGES = $derived([
    { key: "cloning",   label: $t("ingest.progress.stageCloning") },
    { key: "crawling",  label: $t("ingest.progress.stageCrawling") },
    { key: "analyzing", label: $t("ingest.progress.stageAnalyzing") },
    { key: "parsing",   label: $t("ingest.progress.stageParsing") },
    { key: "storing",   label: $t("ingest.progress.stageStoring") },
    { key: "done",      label: $t("ingest.progress.stageComplete") },
  ]);

  const STATUS_ORDER = ["pending", "cloning", "crawling", "analyzing", "parsing", "storing", "done", "failed"];

  function stageState(stageKey: string, currentStatus: string): "done" | "active" | "pending" | "failed" {
    // When failed: don't mark prior stages as "done" — the backend doesn't
    // record the last-good stage, so rendering them green would be misleading.
    if (currentStatus === "failed") return "failed";
    const ci = STATUS_ORDER.indexOf(currentStatus);
    const si = STATUS_ORDER.indexOf(stageKey);
    if (si < ci) return "done";
    if (si === ci) return "active";
    return "pending";
  }

  let progressPercent = $derived(
    job && job.total_files > 0
      ? Math.round((job.processed_files / job.total_files) * 100)
      : 0
  );

  onMount(async () => {
    // Initial fetch for current state.
    try {
      const status = await fetchIngestStatus(repoName);
      if (status) job = status;
    } catch (e: unknown) {
      if (!isAuthError(e)) error = e instanceof Error ? e.message : String(e);
    }

    // Subscribe to SSE job updates — only call unsub() on destroy, never disconnectEvents().
    connectEvents();
    unsubJobUpdate = onEvent("job_update", (raw) => {
      const data = raw as {
        repo_name: string;
        status: string;
        processed_files?: number;
        total_files?: number;
        total_chunks?: number;
      };
      if (data.repo_name !== repoName) return;
      job = {
        ...(job ?? {
          id: "",
          repo_name: repoName,
          git_ref: "",
          error_message: null,
          started_at: "",
          completed_at: null,
        }),
        status: data.status,
        processed_files: data.processed_files ?? job?.processed_files ?? 0,
        total_files: data.total_files ?? job?.total_files ?? 0,
        total_chunks: data.total_chunks ?? job?.total_chunks ?? 0,
      };
    });
  });

  onDestroy(() => {
    // Only unsubscribe the listener — do NOT call disconnectEvents().
    unsubJobUpdate?.();
  });
</script>

<div class="mx-auto w-full max-w-[520px]">
  <Button variant="ghost" size="sm" onclick={onBack} class="mb-4 gap-1 font-mono text-xs text-muted-foreground">
    <ArrowLeft size={14} /> {$t("common.back")}
  </Button>

  <Card.Root class="border-border/50 bg-card/80 backdrop-blur-xl">
    <Card.Header>
      <span class="font-mono text-[10px] font-medium tracking-[0.12em] uppercase text-primary/35">
        {$t("ingest.progress.label")}
      </span>
      <Card.Title class="font-mono text-base font-bold tracking-[0.12em]">
        {$t("ingest.progress.title", { values: { repoName } })}
      </Card.Title>
    </Card.Header>

    <Card.Content class="space-y-5">
      {#if error}
        <p class="font-mono text-sm text-destructive">{error}</p>
      {/if}

      {#if job}
        <!-- Pipeline stages -->
        <div class="flex flex-col gap-2.5">
          {#each STAGES as stage (stage.key)}
            {@const state = stageState(stage.key, job.status)}
            <div class="flex items-center gap-2.5 font-mono text-xs tracking-[0.04em] transition-colors {
              state === 'done'    ? 'text-emerald-500' :
              state === 'active'  ? 'text-primary' :
              state === 'failed'  ? 'text-destructive' :
              'text-muted-foreground'
            }">
              <span class="flex w-5 shrink-0 items-center justify-center">
                {#if state === "done"}
                  <Check size={13} />
                {:else if state === "active"}
                  <Loader size={13} class="animate-spin" />
                {:else if state === "failed"}
                  <X size={13} />
                {:else}
                  <Circle size={13} strokeWidth={1} />
                {/if}
              </span>
              <span class="flex-1">{stage.label}</span>
              {#if state === "active" && job.total_files > 0}
                <span class="text-secondary-foreground">
                  {$t("ingest.progress.files", { values: { processed: job.processed_files, total: job.total_files } })}
                </span>
              {/if}
            </div>
          {/each}
        </div>

        <!-- Progress bar -->
        <div class="space-y-1.5">
          <div class="flex items-center justify-between font-mono text-xs text-muted-foreground">
            <span>
              {$t("ingest.progress.files", { values: { processed: job.processed_files, total: job.total_files } })}
            </span>
            <span>{progressPercent}%</span>
          </div>
          <Progress value={progressPercent} max={100} class="h-1.5" />
        </div>

        <!-- Done banner -->
        {#if job.status === "done"}
          <div class="flex items-center gap-2 rounded-md border border-emerald-500/20 bg-emerald-500/8 p-2.5 font-mono text-xs text-emerald-500">
            <Check size={14} />
            {$t("ingest.progress.complete", { values: { chunks: job.total_chunks } })}
          </div>
        {/if}

        <!-- Failed banner — shown even when error_message is null -->
        {#if job.status === "failed"}
          <div class="flex items-center gap-2 rounded-md border border-destructive/20 bg-destructive/8 p-2.5 font-mono text-xs text-destructive">
            <X size={14} />
            {job.error_message ?? $t("ingest.progress.failed")}
          </div>
        {/if}
      {:else}
        <p class="text-center font-mono text-xs text-muted-foreground">{$t("ingest.progress.connecting")}</p>
      {/if}
    </Card.Content>

    {#if job?.status === "done"}
      <Card.Footer>
        <Button
          variant="outline"
          class="w-full gap-2 font-mono text-xs font-bold tracking-[0.1em] text-primary hover:bg-primary/8 hover:shadow-[0_0_16px_rgba(59,130,246,0.15)]"
          onclick={() => onDone(repoName)}
        >
          {$t("ingest.progress.viewGraph")} <ArrowRight size={14} />
        </Button>
      </Card.Footer>
    {/if}
  </Card.Root>
</div>
