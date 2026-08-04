<script module lang="ts">
  let initialized = false;

  async function renderMermaid(id: string, code: string): Promise<string> {
    const { default: mermaid } = await import("mermaid");
    if (!initialized) {
      mermaid.initialize({
        startOnLoad: false,
        theme: "dark",
        securityLevel: "strict",
        fontFamily: "JetBrains Mono Variable, monospace",
      });
      initialized = true;
    }
    const { svg } = await mermaid.render(id, code);
    return svg;
  }

  let seq = 0;
</script>

<script lang="ts">
  import { t } from "svelte-i18n";

  interface Props {
    code: string;
  }
  let { code }: Props = $props();

  let svg = $state<string | null>(null);
  let failed = $state(false);
  const id = `docs-mermaid-${++seq}`;

  $effect(() => {
    let cancelled = false;
    svg = null;
    failed = false;
    renderMermaid(id, code)
      .then((s) => {
        if (!cancelled) svg = s;
      })
      .catch(() => {
        if (!cancelled) failed = true;
      });
    return () => {
      cancelled = true;
    };
  });
</script>

{#if svg}
  <div class="docs-mermaid-svg my-4">{@html svg}</div>
{:else if failed}
  <div class="my-4">
    <p class="mb-1 text-[11px] text-muted-foreground">
      {$t("docs.mermaidFailed")}
    </p>
    <pre class="docs-code"><code>{code}</code></pre>
  </div>
{:else}
  <pre class="docs-code my-4 animate-pulse"><code>{code}</code></pre>
{/if}
