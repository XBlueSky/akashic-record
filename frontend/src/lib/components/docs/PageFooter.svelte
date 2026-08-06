<script lang="ts">
	import { t } from "svelte-i18n";
	import { addToast } from "$lib/state/toast.svelte.js";
	import type { NavPageResolved } from "$lib/docs/nav.js";
	import { encodeUrlPath } from "$lib/docs/paths.js";

	interface Props {
		prev?: NavPageResolved;
		next?: NavPageResolved;
		base: string;
		stamp: string;
		fullKey: string;
	}
	let { prev, next, base, stamp, fullKey }: Props = $props();

	function hrefFor(p: NavPageResolved): string {
		return p.urlPath === "" ? base : `${base}/${encodeUrlPath(p.urlPath)}`;
	}

	async function report(): Promise<void> {
		await navigator.clipboard.writeText(`${stamp}\npage: ${fullKey}\nurl: ${location.href}`);
		addToast($t("docs.footer.reportCopied"));
	}
</script>

<footer class="mt-10 border-t border-border pt-4" data-testid="page-footer">
	{#if prev || next}
		<div class="mb-4 flex justify-between gap-4 text-[13px]">
			{#if prev}
				<a
					href={hrefFor(prev)}
					class="group text-muted-foreground transition-colors duration-150 hover:text-foreground"
				>
					<span class="block text-[10px] uppercase tracking-wider">{$t("docs.footer.prev")}</span>
					<span class="text-primary group-hover:underline">← {prev.title}</span>
				</a>
			{:else}<span></span>{/if}
			{#if next}
				<a
					href={hrefFor(next)}
					class="group text-right text-muted-foreground transition-colors duration-150 hover:text-foreground"
				>
					<span class="block text-[10px] uppercase tracking-wider">{$t("docs.footer.next")}</span>
					<span class="text-primary group-hover:underline">{next.title} →</span>
				</a>
			{:else}<span></span>{/if}
		</div>
	{/if}
	<div class="flex items-center justify-between gap-4">
		<span class="font-mono text-[10px] text-muted-foreground">{stamp}</span>
		<button
			class="rounded border border-border px-2 py-1 text-[11px] text-muted-foreground transition-colors duration-150 hover:text-foreground"
			onclick={report}>{$t("docs.footer.report")}</button
		>
	</div>
</footer>
