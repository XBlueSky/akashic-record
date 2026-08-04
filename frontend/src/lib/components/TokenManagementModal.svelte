<script lang="ts">
  import { t } from "svelte-i18n";
  import * as Dialog from "$lib/components/ui/dialog";
  import * as Tabs from "$lib/components/ui/tabs";
  import MyTokensTab from "./MyTokensTab.svelte";
  import RevokePassthroughTab from "./RevokePassthroughTab.svelte";
  import RecentActivityTab from "./RecentActivityTab.svelte";

  let { onclose }: { onclose: () => void } = $props();

  // The modal is always open while mounted; closing calls onclose which
  // removes the component from the tree (root layout removes ?settings=tokens).
  let open = $state(true);

  function handleOpenChange(next: boolean) {
    if (!next) onclose();
  }
</script>

<Dialog.Root {open} onOpenChange={handleOpenChange}>
  <Dialog.Content class="sm:max-w-2xl">
    <Dialog.Header>
      <Dialog.Title>{$t("account.title")}</Dialog.Title>
    </Dialog.Header>

    <Tabs.Root value="myTokens" class="w-full">
      <Tabs.List class="grid w-full grid-cols-3">
        <Tabs.Trigger value="myTokens">{$t("account.tabs.myTokens")}</Tabs.Trigger>
        <Tabs.Trigger value="revokePassthrough">{$t("account.tabs.revokePassthrough")}</Tabs.Trigger>
        <Tabs.Trigger value="recentActivity">{$t("account.tabs.recentActivity")}</Tabs.Trigger>
      </Tabs.List>

      <Tabs.Content value="myTokens" class="pt-4">
        <MyTokensTab />
      </Tabs.Content>

      <Tabs.Content value="revokePassthrough" class="pt-4">
        <RevokePassthroughTab />
      </Tabs.Content>

      <Tabs.Content value="recentActivity" class="pt-4">
        <RecentActivityTab />
      </Tabs.Content>
    </Tabs.Root>
  </Dialog.Content>
</Dialog.Root>
