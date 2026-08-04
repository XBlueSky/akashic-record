<script lang="ts">
  import { t } from "svelte-i18n";
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import { Label } from "$lib/components/ui/label";
  import { revokePassthrough } from "$lib/api";
  import { toast } from "$lib/state/toast.svelte";

  let raw = $state("");
  let busy = $state(false);
  let success: string | null = $state(null);
  let inlineError: string | null = $state(null);

  let trimmed = $derived(raw.trim());
  let isToken = $derived(trimmed.startsWith("glpat-") && trimmed.length > 6);
  let isPrefix = $derived(/^[0-9a-f]{16}$/.test(trimmed));
  let valid = $derived(isToken || isPrefix);

  async function submit() {
    if (!valid || busy) return;
    busy = true;
    success = null;
    inlineError = null;
    try {
      const body = isToken ? { token: trimmed } : { prefix: trimmed };
      const r = await revokePassthrough(body);
      success = $t("account.passthrough.success", { values: { prefix: r.prefix } });
      raw = "";
    } catch (e: unknown) {
      // Surface the backend `error` field if available, otherwise the message
      const msg = e instanceof Error ? e.message : String(e);
      inlineError = $t("account.passthrough.errSubmit", { values: { message: msg } });
      toast.push(msg, "error");
    } finally {
      busy = false;
    }
  }
</script>

<div class="flex flex-col gap-3">
  <h3 class="text-base font-medium">{$t("account.passthrough.heading")}</h3>
  <p class="text-sm text-muted-foreground">{$t("account.passthrough.instruction")}</p>

  <div class="flex flex-col gap-2">
    <Label for="pat-input">{$t("account.passthrough.input")}</Label>
    <Input id="pat-input" bind:value={raw} placeholder="glpat-... or 16 hex" class="font-mono" />
    {#if !valid && trimmed.length > 0}
      <p class="text-xs text-red-500">{$t("account.passthrough.errInvalid")}</p>
    {/if}
  </div>

  <Button onclick={submit} disabled={!valid || busy} class="self-start">
    {$t("account.passthrough.revoke")}
  </Button>

  {#if success}
    <p class="text-sm text-green-500">{success}</p>
  {/if}
  {#if inlineError}
    <p class="text-sm text-red-500">{inlineError}</p>
  {/if}
</div>
