<script lang="ts">
  import { page } from "$app/state";
  import { t } from "svelte-i18n";
</script>

<div class="mx-auto max-w-md py-24 text-center">
  <p class="font-mono text-[32px] text-muted-foreground">{page.status}</p>
  <p class="mt-2 text-sm text-foreground">{page.error?.message}</p>
  <a href="/docs" class="mt-6 inline-block text-sm text-primary hover:underline">{$t("docs.title")}</a>
</div>
