<script lang="ts">
	import { t } from "svelte-i18n";
	import type { LanguageChipEntry } from "$lib/docs/language.js";
	import { rewriteHref, type DocsRenderCtx } from "$lib/docs/markdown.js";

	interface Props {
		chips: LanguageChipEntry[];
		ctx: DocsRenderCtx;
	}
	let { chips, ctx }: Props = $props();

	const resolved = $derived(
		chips.map((c) => {
			if (c.href === null) return { label: c.label, href: null, active: true };
			const r = rewriteHref(c.href, ctx);
			return { label: c.label, href: r.kind === "page" ? r.href : null, active: false };
		}),
	);
</script>

<div class="mb-4 flex items-center gap-1.5" data-testid="language-chip">
	<span class="mr-1 font-mono text-[10px] uppercase tracking-wider text-muted-foreground"
		>{$t("docs.languages")}</span
	>
	{#each resolved as c (c.label)}
		{#if c.href}
			<a
				href={c.href}
				class="rounded border border-border px-2 py-0.5 font-mono text-[11px] text-muted-foreground transition-colors duration-150 hover:border-primary/40 hover:text-foreground"
				>{c.label}</a
			>
		{:else}
			<span
				class="rounded border px-2 py-0.5 font-mono text-[11px] {c.active
					? 'border-primary/50 text-foreground'
					: 'border-border text-muted-foreground'}">{c.label}</span
			>
		{/if}
	{/each}
</div>
