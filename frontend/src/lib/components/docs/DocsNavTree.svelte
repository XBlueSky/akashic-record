<script lang="ts">
	import { t } from "svelte-i18n";
	import type { ResolvedNav } from "$lib/docs/nav.js";
	import { encodeUrlPath } from "$lib/docs/paths.js";

	interface Props {
		nav: ResolvedNav;
		base: string;
		currentFullKey: string | null;
	}
	let { nav, base, currentFullKey }: Props = $props();

	let root = $state<HTMLElement | null>(null);

	function hrefFor(urlPath: string): string {
		return urlPath === "" ? base : `${base}/${encodeUrlPath(urlPath)}`;
	}

	function handleKey(e: KeyboardEvent): void {
		if (!root) return;
		const items = Array.from(root.querySelectorAll<HTMLAnchorElement>("a[data-nav-item]"));
		const i = items.indexOf(document.activeElement as HTMLAnchorElement);
		if (i === -1) return;
		let target: number | null = null;
		if (e.key === "ArrowDown") target = Math.min(i + 1, items.length - 1);
		else if (e.key === "ArrowUp") target = Math.max(i - 1, 0);
		else if (e.key === "Home") target = 0;
		else if (e.key === "End") target = items.length - 1;
		if (target !== null) {
			e.preventDefault();
			items[target].focus();
		}
	}
</script>

<nav bind:this={root} aria-label={$t("docs.nav.menu")} data-testid="docs-nav" onkeydown={handleKey}>
	<ul role="tree" class="space-y-5">
		{#each nav.groups as g (g.title)}
			<li role="group" aria-label={g.title}>
				<div
					class="mb-1 px-2 text-[10px] font-medium uppercase tracking-wider text-muted-foreground"
				>
					{g.title}
				</div>
				<ul class="space-y-0.5">
					{#each g.pages as p (p.fullKey)}
						<li role="treeitem" aria-selected={p.fullKey === currentFullKey}>
							<a
								data-nav-item
								href={hrefFor(p.urlPath)}
								aria-current={p.fullKey === currentFullKey ? "page" : undefined}
								title={p.description || undefined}
								class="block rounded px-2 py-1 text-[13px] leading-5 outline-none transition-colors duration-150 focus-visible:ring-2 focus-visible:ring-ring {p.fullKey ===
								currentFullKey
									? 'bg-secondary text-foreground'
									: 'text-muted-foreground hover:bg-secondary/60 hover:text-foreground'}"
								>{p.title}</a
							>
						</li>
					{/each}
				</ul>
			</li>
		{/each}
	</ul>
</nav>
