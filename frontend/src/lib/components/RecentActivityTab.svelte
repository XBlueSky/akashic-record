<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "svelte-i18n";
  import { Skeleton } from "$lib/components/ui/skeleton";
  import { listMyAudit, isAuthError } from "$lib/api";
  import type { AuditEntry } from "$lib/api";

  let entries: AuditEntry[] = $state([]);
  let loading = $state(true);
  let error: string | null = $state(null);

  onMount(async () => {
    try {
      entries = await listMyAudit(50);
    } catch (e) {
      if (!isAuthError(e)) {
        error = $t("account.activity.loadFailed");
      }
    } finally {
      loading = false;
    }
  });

  function fmt(ts: string): string {
    return new Date(ts).toLocaleString();
  }
</script>

{#if loading}
  <div class="flex flex-col gap-2">
    {#each [0, 1, 2, 3, 4] as _i}
      <Skeleton class="h-12 w-full" />
    {/each}
  </div>
{:else if error}
  <p class="text-sm text-red-500">{error}</p>
{:else if entries.length === 0}
  <p class="text-sm text-muted-foreground">{$t("account.activity.empty")}</p>
{:else}
  <ul class="flex flex-col gap-2">
    {#each entries as e}
      <li class="rounded border p-2 text-sm">
        <div class="flex items-baseline justify-between">
          <span class="font-mono">{e.action}</span>
          <span class="text-xs text-muted-foreground">{fmt(e.ts)}</span>
        </div>
        {#if e.target_id}
          <div class="text-xs text-muted-foreground">→ {e.target_id}</div>
        {/if}
        <div class="mt-1 text-xs text-muted-foreground">
          {$t("account.activity.via")} <span class="font-mono">{e.actor_token_id}</span>
          {#if e.ip}
            {" "}{$t("account.activity.from")} <span class="font-mono">{e.ip}</span>
          {/if}
        </div>
      </li>
    {/each}
  </ul>
{/if}
