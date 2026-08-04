<script lang="ts">
  import { page } from "$app/state";
  import { t } from "svelte-i18n";
  import Logo from "$lib/components/Logo.svelte";
  import { commandPalette } from "$lib/state/command-palette.svelte.js";
  import { docsHeader } from "$lib/state/docs-header.svelte.js";
  import { addToast } from "$lib/state/toast.svelte.js";

  let { children } = $props();

  async function copyStamp(): Promise<void> {
    if (!docsHeader.stamp) return;
    await navigator.clipboard.writeText(docsHeader.stamp);
    addToast(String($t("docs.stampCopied") ?? "Stamp copied"));
  }
</script>

<div class="flex h-svh flex-col bg-background text-foreground">
  <header
    data-testid="docs-topbar"
    class="flex h-10 shrink-0 items-center gap-3 border-b border-border px-4"
  >
    <a
      href="/"
      class="flex items-center text-muted-foreground transition-colors duration-150 hover:text-foreground"
      title={String($t("docs.backToApp") ?? "Back to Akashic Record")}
    >
      <div class="h-4 w-4">
        <Logo />
      </div>
    </a>
    <span class="font-mono text-[11px] text-muted-foreground">docs</span>
    {#if page.params.repo}
      <span class="text-[11px] text-muted-foreground">/</span>
      <a
        href={`/docs/${encodeURIComponent(page.params.repo)}/latest`}
        class="font-mono text-[13px] text-foreground"
      >{page.params.repo}</a>
    {/if}
    {#if docsHeader.stamp}
      <button
        class="ml-1 rounded border border-border px-2 py-0.5 font-mono text-[10px] text-muted-foreground transition-colors duration-150 hover:text-foreground"
        onclick={copyStamp}
        title={$t("docs.copyStamp")}
      >{docsHeader.stamp}</button>
    {/if}
    <button
      class="ml-auto flex items-center gap-2 rounded border border-border px-2 py-1 text-[11px] text-muted-foreground transition-colors duration-150 hover:text-foreground"
      onclick={() => commandPalette.show()}
    >
      {$t("docs.search")}
      <kbd class="font-mono text-[10px]">⌘K</kbd>
    </button>
  </header>
  <div class="min-h-0 flex-1 overflow-y-auto" data-docs-scroll>
    {@render children()}
  </div>
</div>
