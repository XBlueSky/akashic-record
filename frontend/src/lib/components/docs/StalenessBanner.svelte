<script lang="ts">
  import { t } from "svelte-i18n";

  interface Props {
    deriveStatus: string;
  }
  let { deriveStatus }: Props = $props();

  const message = $derived(
    deriveStatus === "pending" || deriveStatus === "running"
      ? $t("docs.staleness.indexing")
      : deriveStatus === "failed"
        ? $t("docs.staleness.failed")
        : null,
  );
</script>

{#if message}
  <div
    data-testid="staleness-banner"
    class="mb-4 border-l-2 border-primary/60 bg-secondary/50 px-3 py-2 text-[12px] text-muted-foreground"
  >{message}</div>
{/if}
