<script lang="ts">
	import { onMount, onDestroy } from "svelte";
	import * as Command from "$lib/components/ui/command";
	import { fetchRepos, searchKnowledge } from "$lib/api";
	import type { Repository, SearchKnowledgeResult } from "$lib/types";
	import { goto } from "$app/navigation";
	import { commandPalette } from "$lib/state/command-palette.svelte";
	import { t } from "svelte-i18n";
	import { getDocsIndexMap, docsHitHref, type DocsIndexInfo } from "$lib/docs/search-links.js";

	let inputValue = $state("");
	let repos = $state<Repository[]>([]);
	let searchResults = $state<SearchKnowledgeResult[]>([]);
	let searching = $state(false);
	let docsIndex = $state<Map<string, DocsIndexInfo> | null>(null);
	let debounceTimer: ReturnType<typeof setTimeout> | undefined;

	// Fetch repos on mount for client-side fuzzy filtering
	onMount(async () => {
		try {
			repos = await fetchRepos();
		} catch {
			repos = [];
		}
	});

	// Warm the docs index up front: a DOC hit's deep link needs each repo's
	// index dir to build the page URL, and without it docsHitHref() degrades to
	// the repo root. Results become clickable the moment the search resolves, so
	// fetching only then leaves a window where a fast click loses the page path.
	// Memoized, so selectResult()'s await is a no-op once this settles.
	onMount(() => {
		void getDocsIndexMap()
			.then((m) => (docsIndex = m))
			.catch(() => {});
	});

	onDestroy(() => {
		if (debounceTimer) clearTimeout(debounceTimer);
	});

	// Keyboard shortcut: Ctrl+K / Cmd+K
	function handleKeydown(e: KeyboardEvent) {
		if ((e.metaKey || e.ctrlKey) && e.key === "k") {
			e.preventDefault();
			commandPalette.toggle();
		}
	}

	// Client-side fuzzy filter for repos
	let filteredRepos = $derived.by(() => {
		const q = inputValue.trim().toLowerCase();
		if (!q) return repos;
		return repos.filter((r) => r.name.toLowerCase().includes(q));
	});

	// Debounced semantic search when input changes
	$effect(() => {
		const q = inputValue.trim();
		if (debounceTimer) clearTimeout(debounceTimer);

		if (!q) {
			searchResults = [];
			searching = false;
			return;
		}

		searching = true;
		debounceTimer = setTimeout(async () => {
			try {
				searchResults = await searchKnowledge(q, undefined, 10);
			} catch {
				searchResults = [];
			} finally {
				searching = false;
			}
		}, 300);
	});

	// Reset state when dialog closes
	$effect(() => {
		if (!commandPalette.open) {
			inputValue = "";
			searchResults = [];
			searching = false;
		}
	});

	function selectRepo(name: string) {
		commandPalette.hide();
		goto("/r/" + encodeURIComponent(name));
	}

	async function selectResult(result: SearchKnowledgeResult) {
		if (result.docs) {
			// If the mount-time warm-up hasn't settled yet, wait for it rather than
			// navigating with a missing index — docsHitHref() would silently drop the
			// page path and land on the repo root instead of the hit.
			if (!docsIndex) {
				try {
					docsIndex = await getDocsIndexMap();
				} catch {
					docsIndex = null;
				}
			}
			const info = docsIndex?.get(result.docs.repo);
			goto(docsHitHref(result.docs, info));
			commandPalette.hide();
			return;
		}
		commandPalette.hide();
		goto("/r/" + encodeURIComponent(result.repo_name));
	}

	function scorePercent(score: number): string {
		return `${Math.round(score * 100)}%`;
	}

	function layerColor(layer: string): string {
		if (layer === "MAP") return "bg-transparent border border-emerald-500/30 text-emerald-400";
		if (layer === "DOC") return "bg-transparent border border-cyan-500/30 text-cyan-400";
		return "bg-transparent border border-purple-500/30 text-purple-400";
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<Command.Dialog
	bind:open={commandPalette.open}
	title="Command Palette"
	description="Search repositories and knowledge base"
>
	<Command.Input placeholder={$t("command.placeholder")} bind:value={inputValue} />
	<Command.List class="max-h-[400px]">
		{#if !inputValue.trim() && repos.length === 0}
			<Command.Empty>{$t("command.noRepos")}</Command.Empty>
		{:else if inputValue.trim() && filteredRepos.length === 0 && searchResults.length === 0 && !searching}
			<Command.Empty>{$t("common.noResults")}</Command.Empty>
		{/if}

		<!-- Repositories group -->
		{#if filteredRepos.length > 0}
			<Command.Group heading={$t("command.repos")}>
				{#each filteredRepos as repo (repo.name)}
					<Command.Item
						value={`repo:${repo.name}`}
						onSelect={() => selectRepo(repo.name)}
						class="gap-2"
					>
						<span
							class="h-1.5 w-1.5 shrink-0 rounded-full bg-blue-400"
							style="box-shadow: 0 0 6px rgba(59,130,246,0.4)"
						></span>
						<span class="flex-1 truncate font-mono text-sm text-slate-200">{repo.name}</span>
						{#if repo.last_synced_at}
							<span class="text-[10px] text-slate-500">
								{new Date(repo.last_synced_at).toLocaleDateString()}
							</span>
						{/if}
					</Command.Item>
				{/each}
			</Command.Group>
		{/if}

		<!-- Knowledge search results -->
		{#if searching}
			<Command.Loading>
				<div class="px-4 py-3 font-mono text-sm text-slate-500">{$t("command.searching")}</div>
			</Command.Loading>
		{/if}

		{#if searchResults.length > 0}
			<Command.Separator />
			<Command.Group heading={$t("command.knowledge")}>
				{#each searchResults as result (result.id)}
					<Command.Item
						value={`knowledge:${result.id}:${result.name}`}
						onSelect={() => selectResult(result)}
						class="flex-col items-start gap-1 py-2"
					>
						<div class="flex w-full items-center gap-2">
							<span
								class="inline-flex shrink-0 rounded px-1.5 py-0.5 text-[10px] font-bold uppercase tracking-wider {layerColor(
									result.layer,
								)}"
							>
								{result.layer}
							</span>
							<span
								class="inline-flex shrink-0 rounded bg-white/5 border border-white/10 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wider text-slate-400"
							>
								{result.type}
							</span>
							<span class="flex-1 truncate font-mono text-sm text-slate-200">{result.name}</span>
							<span class="shrink-0 text-[10px] text-slate-500">{scorePercent(result.score)}</span>
						</div>
						{#if result.layer === "MAP" && result.module}
							<span class="w-full truncate font-mono text-xs text-slate-500 pl-1">
								{result.module}
							</span>
						{:else if result.layer === "NOTE" && result.summary}
							<span class="w-full truncate text-xs text-slate-500 pl-1">
								{result.summary}
							</span>
						{:else if result.layer === "DOC" && result.docs}
							<span class="w-full truncate font-mono text-xs text-slate-500 pl-1">
								{result.docs.path}
							</span>
						{:else if result.layer === "DOC" && result.summary}
							<span class="w-full truncate text-xs text-slate-500 pl-1">
								{result.summary}
							</span>
						{/if}
					</Command.Item>
				{/each}
			</Command.Group>
		{/if}
	</Command.List>

	<!-- Footer with keyboard hints -->
	<div
		class="flex items-center justify-between bg-black/20 border-t border-white/5 px-3 py-2 text-xs text-slate-500"
	>
		<div class="flex items-center gap-3">
			<span class="flex items-center gap-1">
				<kbd
					class="rounded border border-white/10 bg-white/5 px-1.5 py-0.5 font-mono text-[10px] text-slate-400"
					>↑↓</kbd
				>
				{$t("command.navigate")}
			</span>
			<span class="flex items-center gap-1">
				<kbd
					class="rounded border border-white/10 bg-white/5 px-1.5 py-0.5 font-mono text-[10px] text-slate-400"
					>↵</kbd
				>
				{$t("command.select")}
			</span>
		</div>
		<span class="flex items-center gap-1">
			<kbd
				class="rounded border border-white/10 bg-white/5 px-1.5 py-0.5 font-mono text-[10px] text-slate-400"
				>esc</kbd
			>
			{$t("command.close")}
		</span>
	</div>
</Command.Dialog>
