<script lang="ts">
	import { t } from "svelte-i18n";
	import { fetchDocumentDetail } from "$lib/api";
	import type { ExplainsTarget } from "$lib/types/index.js";

	interface Props {
		documentId: string | undefined;
		repo: string;
	}
	let { documentId, repo }: Props = $props();

	let targets = $state<ExplainsTarget[]>([]);

	$effect(() => {
		targets = [];
		const id = documentId;
		if (!id) return;
		let cancelled = false;
		fetchDocumentDetail(id)
			.then((detail) => {
				if (cancelled) return;
				const all = detail.sections.flatMap((s) => s.explains);
				const seen = new Set<string>();
				targets = all
					.sort((a, b) => b.confidence - a.confidence)
					.filter((e) => (seen.has(e.chunk_id) ? false : (seen.add(e.chunk_id), true)))
					.slice(0, 5);
			})
			.catch(() => {
				/* derive races / transient errors: stay hidden (D2) */
			});
		return () => {
			cancelled = true;
		};
	});
</script>

{#if targets.length > 0}
	<section class="mt-8" data-testid="related-code">
		<h2 class="mb-2 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
			{$t("docs.related.title")}
		</h2>
		<ul class="space-y-1">
			{#each targets as e (e.chunk_id)}
				<li>
					<a
						href={`/r/${encodeURIComponent(repo)}/graph?chunk=${encodeURIComponent(e.chunk_id)}`}
						class="group flex items-baseline gap-2 rounded border border-border bg-card px-3 py-1.5 transition-colors duration-150 hover:border-primary/40"
					>
						<span class="font-mono text-[12px] text-foreground group-hover:text-primary"
							>{e.chunk_name}</span
						>
						<span class="truncate font-mono text-[10px] text-muted-foreground">{e.module_path}</span
						>
						<span class="ml-auto font-mono text-[10px] text-muted-foreground"
							>{Math.round(e.confidence * 100)}%</span
						>
					</a>
				</li>
			{/each}
		</ul>
	</section>
{/if}
