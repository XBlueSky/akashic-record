<script lang="ts">
	import { docsHeader } from "$lib/state/docs-header.svelte.js";
	import { page } from "$app/state";
	import type { LayoutData } from "./$types.js";
	import DocsNavTree from "$lib/components/docs/DocsNavTree.svelte";
	import * as Sheet from "$lib/components/ui/sheet";
	import { t } from "svelte-i18n";

	let { data, children }: { data: LayoutData; children: import("svelte").Snippet } = $props();

	$effect(() => {
		docsHeader.stamp = data.stamp;
		return () => {
			docsHeader.stamp = null;
		};
	});

	const base = $derived(
		`/docs/${encodeURIComponent(data.repo)}/${encodeURIComponent(data.selector)}`,
	);
	const currentFullKey = $derived.by(() => {
		// page.params 已由 SvelteKit 解碼——不要再 decodeURIComponent(雙重解碼 bug)。
		const p = page.params.page ?? "";
		return p === "" ? data.indexPath : `${data.indexDir === "" ? "" : `${data.indexDir}/`}${p}.md`;
	});
</script>

<div class="mx-auto flex w-full max-w-[1440px] gap-8 px-6 py-6">
	<div class="hidden w-60 shrink-0 lg:block">
		<div class="sticky top-4">
			<DocsNavTree nav={data.resolvedNav} {base} {currentFullKey} />
		</div>
	</div>
	<main class="min-w-0 flex-1">
		<div class="mb-3 lg:hidden">
			<Sheet.Root>
				<Sheet.Trigger
					class="rounded border border-border px-2 py-1 text-[11px] text-muted-foreground hover:text-foreground"
					>{$t("docs.nav.open")}</Sheet.Trigger
				>
				<Sheet.Content side="left" class="w-72 overflow-y-auto bg-sidebar p-4">
					<DocsNavTree nav={data.resolvedNav} {base} {currentFullKey} />
				</Sheet.Content>
			</Sheet.Root>
		</div>
		{@render children()}
	</main>
</div>
