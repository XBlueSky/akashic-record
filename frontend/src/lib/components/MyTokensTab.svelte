<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "svelte-i18n";
  import { Button } from "$lib/components/ui/button";
  import { Skeleton } from "$lib/components/ui/skeleton";
  import * as AlertDialog from "$lib/components/ui/alert-dialog";
  import { listMcpTokens, revokeMcpToken, isAuthError } from "$lib/api";
  import type { McpTokenSummary } from "$lib/api";
  import { toast } from "$lib/state/toast.svelte";

  let tokens: McpTokenSummary[] = $state([]);
  let loading = $state(true);
  let error: string | null = $state(null);
  let revokingId: string | null = $state(null);
  let confirmId: string | null = $state(null);
  let dialogOpen = $derived(confirmId !== null);

  // Stale-fetch guard: only apply results from the most recent fetch
  let fetchGen = 0;

  onMount(async () => {
    await refresh();
  });

  async function refresh() {
    const gen = ++fetchGen;
    loading = true;
    error = null;
    try {
      const result = await listMcpTokens();
      if (gen !== fetchGen) return; // stale
      tokens = result;
    } catch (e) {
      if (gen !== fetchGen) return;
      if (!isAuthError(e)) {
        error = $t("account.tokens.loadFailed");
      }
    } finally {
      if (gen === fetchGen) loading = false;
    }
  }

  async function doRevoke(id: string) {
    revokingId = id;
    try {
      await revokeMcpToken(id);
      await refresh();
    } catch (e) {
      if (!isAuthError(e)) {
        toast.push(e instanceof Error ? e.message : "Revoke failed", "error");
      }
    } finally {
      revokingId = null;
      confirmId = null;
    }
  }

  function fmt(ts: string | null): string {
    if (!ts) return "—";
    return new Date(ts).toLocaleString();
  }
</script>

{#if loading}
  <div class="flex flex-col gap-2">
    <Skeleton class="h-10 w-full" />
    <Skeleton class="h-10 w-full" />
  </div>
{:else if error}
  <p class="text-sm text-red-500">{error}</p>
{:else if tokens.length === 0}
  <p class="text-sm text-muted-foreground">{$t("account.tokens.empty")}</p>
{:else}
  <table class="w-full text-sm">
    <thead class="border-b text-left text-muted-foreground">
      <tr>
        <th class="py-2 pr-2">{$t("account.tokens.label")}</th>
        <th class="py-2 pr-2">{$t("account.tokens.issued")}</th>
        <th class="py-2 pr-2">{$t("account.tokens.lastUsed")}</th>
        <th class="py-2 pr-2">{$t("account.tokens.expires")}</th>
        <th class="py-2 pr-2">{$t("account.tokens.status")}</th>
        <th class="py-2"></th>
      </tr>
    </thead>
    <tbody>
      {#each tokens as token (token.id)}
        <tr class="border-b last:border-b-0" class:opacity-50={token.revoked_at}>
          <td class="py-2 pr-2 font-mono">{token.label ?? "—"}</td>
          <td class="py-2 pr-2">{fmt(token.issued_at)}</td>
          <td class="py-2 pr-2">{fmt(token.last_used_at)}</td>
          <td class="py-2 pr-2">{fmt(token.expires_at)}</td>
          <td class="py-2 pr-2">
            {#if token.revoked_at}
              {$t("account.tokens.revoked", { values: { ts: fmt(token.revoked_at) } })}
            {:else}
              {$t("account.tokens.active")}
            {/if}
          </td>
          <td class="py-2 text-right">
            {#if !token.revoked_at}
              <Button
                size="sm"
                variant="destructive"
                disabled={revokingId === token.id}
                onclick={() => (confirmId = token.id)}
              >
                {$t("account.tokens.revoke")}
              </Button>
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<AlertDialog.Root bind:open={dialogOpen}>
  <AlertDialog.Content>
    <AlertDialog.Header>
      <AlertDialog.Title>{$t("account.tokens.revoke")}</AlertDialog.Title>
      <AlertDialog.Description>
        {$t("account.tokens.confirmRevoke")}
      </AlertDialog.Description>
    </AlertDialog.Header>
    <AlertDialog.Footer>
      <AlertDialog.Cancel onclick={() => (confirmId = null)}>Cancel</AlertDialog.Cancel>
      <AlertDialog.Action
        disabled={revokingId !== null}
        onclick={() => confirmId && doRevoke(confirmId)}
      >
        {$t("account.tokens.revoke")}
      </AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
