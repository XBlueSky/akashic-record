<script lang="ts">
	import { t } from "svelte-i18n";
	import type { TocEntry } from "$lib/docs/markdown.js";

	interface Props {
		toc: TocEntry[];
		container: HTMLElement | null;
	}
	let { toc, container }: Props = $props();

	let active = $state<string | null>(null);

	$effect(() => {
		active = null;
		if (!container || toc.length === 0) return;
		const headings = toc
			.map((e) => container.querySelector(`#${CSS.escape(e.id)}`))
			.filter((el): el is Element => el !== null);
		const obs = new IntersectionObserver(
			(entries) => {
				const visible = entries
					.filter((e) => e.isIntersecting)
					.sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
				if (visible[0]) active = visible[0].target.id;
			},
			{ rootMargin: "0px 0px -70% 0px" },
		);
		for (const h of headings) obs.observe(h);
		return () => obs.disconnect();
	});
</script>

{#if toc.length > 0}
	<aside class="hidden w-48 shrink-0 xl:block" data-testid="docs-toc">
		<div class="sticky top-4">
			<div class="mb-2 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
				{$t("docs.toc")}
			</div>
			<ul class="border-l border-border">
				{#each toc as e (e.id)}
					<li>
						<a
							href={`#${e.id}`}
							aria-current={active === e.id ? "location" : undefined}
							class="-ml-px block border-l-2 py-0.5 text-[12px] leading-5 transition-colors duration-150 {e.level ===
							3
								? 'pl-6'
								: 'pl-3'} {active === e.id
								? 'border-primary text-foreground'
								: 'border-transparent text-muted-foreground hover:text-foreground'}">{e.text}</a
						>
					</li>
				{/each}
			</ul>
		</div>
	</aside>
{/if}
