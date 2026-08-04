<script lang="ts">
  import { goto } from "$app/navigation";
  import { addSource, triggerIngest, isAuthError } from "$lib/api";
  import { websiteSubmissionOutcome } from "$lib/api/ingest";
  import type { SourceType } from "$lib/types/index.js";
  import { auth } from "$lib/state/auth.svelte";
  import { toast } from "$lib/state/toast.svelte";
  import { t } from "svelte-i18n";
  import * as Card from "$lib/components/ui/card";
  import { Button } from "$lib/components/ui/button";
  import { GitBranch } from "@lucide/svelte";
  import SourcePicker from "$lib/components/ingest/SourcePicker.svelte";
  import GitLabForm from "$lib/components/ingest/GitLabForm.svelte";
  import WebsiteForm from "$lib/components/ingest/WebsiteForm.svelte";
  import IngestProgress from "$lib/components/ingest/IngestProgress.svelte";

  type Step = "pick" | "gitlab" | "website" | "progress" | "submitted";
  let step = $state<Step>("pick");
  let userSetRepoName = $state<string | null>(null);
  let submitError = $state<string | null>(null);
  let submittedOutcome = $state<"unsupported" | "review" | null>(null);

  function handlePick(type: SourceType) {
    step = type as Step;
  }

  async function handleGitLabSubmit(data: { url: string; git_ref: string }) {
    submitError = null;
    const { url, git_ref } = data;
    try {
      const path = new URL(url).pathname.replace(/^\//, "").replace(/\.git$/, "");
      userSetRepoName = path;
      try {
        await addSource("gitlab", url, { git_ref });
      } catch {
        // Source already exists — re-trigger ingest instead.
        await triggerIngest(path, git_ref, "gitlab");
      }
      step = "progress";
    } catch (err: unknown) {
      if (!isAuthError(err)) {
        const msg = err instanceof Error ? err.message : String(err);
        submitError = msg;
        toast.push(msg, "error");
      }
    }
  }

  async function handleWebsiteSubmit(data: { url: string; crawl_depth: number; url_pattern: string }) {
    submitError = null;
    try {
      const { url, crawl_depth, url_pattern } = data;
      const result = await addSource("website", url, { crawl_depth, url_pattern });
      userSetRepoName = result.repo_name;
      // A website submission is gated: the backend starts no crawl and returns
      // an empty job_id, so there is no job to poll — show the confirmation card
      // instead of the (forever-"connecting") progress view.
      submittedOutcome = websiteSubmissionOutcome(result.status);
      step = "submitted";
    } catch (err: unknown) {
      if (!isAuthError(err)) {
        const msg = err instanceof Error ? err.message : String(err);
        submitError = msg;
        toast.push(msg, "error");
      }
    }
  }

  function handleProgressDone(name: string) {
    goto("/r/" + encodeURIComponent(name) + "/graph");
  }
</script>

<div class="flex h-full min-h-[400px] w-full flex-col items-center justify-center">
  {#if submitError}
    <div class="mb-4 max-w-[480px] rounded-md border border-destructive/25 bg-destructive/10 px-3.5 py-2.5 text-sm text-destructive">
      {submitError}
    </div>
  {/if}

  {#if !auth.checked}
    <p class="font-mono text-xs text-muted-foreground">{$t("ingest.auth.checking")}</p>
  {:else if !auth.user}
    <Card.Root class="max-w-[420px] border-border/50 bg-card/80 text-center backdrop-blur-xl">
      <Card.Content class="space-y-6 px-10 py-12">
        <span class="font-mono text-[10px] font-medium tracking-[0.12em] uppercase text-primary/35">
          {$t("ingest.auth.label")}
        </span>
        <h2 class="text-2xl font-bold tracking-[0.18em] text-primary drop-shadow-[0_0_20px_rgba(59,130,246,0.4)]">
          {$t("ingest.auth.title")}
        </h2>
        <p class="font-mono text-xs tracking-[0.06em] text-muted-foreground">
          {$t("ingest.auth.subtitle")}
        </p>
        <Button size="lg" onclick={() => auth.login()} class="gap-2.5 px-8 text-sm font-semibold">
          <GitBranch size={20} /> {$t("ingest.auth.button")}
        </Button>
        <p class="text-xs leading-relaxed text-muted-foreground">
          {$t("ingest.auth.hint")}
        </p>
      </Card.Content>
    </Card.Root>
  {:else if step === "pick"}
    <SourcePicker onPick={handlePick} />
  {:else if step === "gitlab"}
    <GitLabForm onBack={() => (step = "pick")} onSubmit={handleGitLabSubmit} />
  {:else if step === "website"}
    <WebsiteForm onBack={() => (step = "pick")} onSubmit={handleWebsiteSubmit} />
  {:else if step === "submitted" && submittedOutcome}
    {@const isUnsupported = submittedOutcome === "unsupported"}
    <Card.Root class="max-w-[420px] border-border/50 bg-card/80 text-center backdrop-blur-xl">
      <Card.Content class="space-y-6 px-10 py-12">
        <span class="font-mono text-[10px] font-medium tracking-[0.12em] uppercase text-primary/35">
          {$t(isUnsupported ? "ingest.submitted.unsupportedLabel" : "ingest.submitted.reviewLabel")}
        </span>
        <h2 class="text-2xl font-bold tracking-[0.18em] text-primary drop-shadow-[0_0_20px_rgba(59,130,246,0.4)]">
          {$t(isUnsupported ? "ingest.submitted.unsupportedTitle" : "ingest.submitted.reviewTitle")}
        </h2>
        <p class="font-mono text-xs leading-relaxed tracking-[0.06em] text-muted-foreground">
          {$t(isUnsupported ? "ingest.submitted.unsupportedBody" : "ingest.submitted.reviewBody")}
        </p>
        <Button size="lg" onclick={() => (step = "pick")} class="px-8 text-sm font-semibold">
          {$t("ingest.submitted.addAnother")}
        </Button>
      </Card.Content>
    </Card.Root>
  {:else if step === "progress" && userSetRepoName}
    <IngestProgress
      repoName={userSetRepoName}
      onBack={() => (step = "pick")}
      onDone={handleProgressDone}
    />
  {/if}
</div>
