<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "svelte-i18n";
  import { fetchDocsRepos, type DocsRepoEntry } from "$lib/api";
  import RepoCard from "$lib/components/docs/RepoCard.svelte";
  import { Skeleton } from "$lib/components/ui/skeleton";

  let repos = $state<DocsRepoEntry[] | null>(null);
  let failed = $state(false);

  onMount(async () => {
    try {
      repos = (await fetchDocsRepos()).repos;
    } catch {
      failed = true;
    }
  });
</script>

<svelte:head><title>{$t("docs.title")} — Akashic Record</title></svelte:head>

<div class="mx-auto w-full max-w-[1200px] px-6 py-8">
  <h1 class="mb-6 text-lg font-medium text-foreground">{$t("docs.title")}</h1>

  {#if failed}
    <p class="text-sm text-muted-foreground">{$t("docs.hub.loadFailed")}</p>
  {:else if repos === null}
    <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
      {#each Array.from({ length: 6 }) as _, i (i)}
        <Skeleton class="h-28 rounded-lg" />
      {/each}
    </div>
  {:else if repos.length === 0}
    <div class="mx-auto max-w-md rounded-lg border border-border bg-card p-8" data-testid="docs-empty">
      <p class="text-sm font-medium text-foreground">{$t("docs.hub.empty.title")}</p>
      <p class="mt-2 text-[13px] text-muted-foreground">{$t("docs.hub.empty.body")}</p>
      <ol class="mt-3 list-decimal space-y-1 pl-5 font-mono text-[12px] text-muted-foreground">
        <li>{$t("docs.hub.empty.step1")}</li>
        <li>{$t("docs.hub.empty.step2")}</li>
        <li>{$t("docs.hub.empty.step3")}</li>
      </ol>
    </div>
  {:else}
    <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
      {#each repos as entry (entry.repo)}
        <RepoCard {entry} />
      {/each}
    </div>
  {/if}
</div>
