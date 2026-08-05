<script lang="ts">
  import { t } from "svelte-i18n";
  import { highlightCode } from "$lib/docs/highlight.js";

  interface Props {
    code: string;
    lang: string;
  }
  let { code, lang }: Props = $props();

  let html = $state<string | null>(null);
  let copied = $state(false);

  $effect(() => {
    let cancelled = false;
    void highlightCode(code, lang).then((h) => {
      if (!cancelled && h !== null) html = h;
    });
    return () => {
      cancelled = true;
    };
  });

  async function copy(): Promise<void> {
    await navigator.clipboard.writeText(code);
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }
</script>

<div class="docs-codeblock group relative">
  {#if lang}
    <span class="docs-codeblock-lang">{lang}</span>
  {/if}
  <button
    class="docs-codeblock-copy opacity-0 transition-opacity duration-150 focus-visible:opacity-100 group-hover:opacity-100"
    onclick={copy}
  >{copied ? $t("docs.codeCopied") : $t("docs.codeCopy")}</button>
  {#if html}
    <!-- eslint-disable-next-line svelte/no-at-html-tags -- html is Shiki-generated markup (highlightCode); source text is escaped by the tokenizer, not raw user HTML -->
    {@html html}
  {:else}
    <pre class="docs-code"><code>{code}</code></pre>
  {/if}
</div>
