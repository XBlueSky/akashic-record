<script lang="ts">
	import { goto } from "$app/navigation";
	import { fetchSagas, isAuthError } from "$lib/api";
	import type { Saga } from "$lib/types/index.js";
	import { fade } from "svelte/transition";
	import { untrack } from "svelte";

	let {
		repoName,
		status = undefined,
	}: {
		repoName: string;
		status?: string;
	} = $props();

	let sagas = $state<Saga[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	// Local filter; intentionally seeds from the URL prop once, then owned by UI interaction.
	let statusFilter = $state<string>(untrack(() => status ?? ""));

	// Stale-fetch generation guard
	let fetchGen = 0;

	const STATUS_OPTIONS: { label: string; value: string }[] = [
		{ label: "All", value: "" },
		{ label: "Active", value: "active" },
		{ label: "Open", value: "open" },
		{ label: "Resolved", value: "resolved" },
		{ label: "Archived", value: "archived" },
	];

	async function load(filter: string): Promise<void> {
		const gen = ++fetchGen;
		loading = true;
		error = null;
		try {
			const result = await fetchSagas(repoName, filter || undefined);
			if (gen !== fetchGen) return; // stale response — discard
			sagas = result;
		} catch (e: unknown) {
			if (gen !== fetchGen) return;
			if (!isAuthError(e)) {
				error = e instanceof Error ? e.message : String(e);
			}
			sagas = [];
		} finally {
			if (gen === fetchGen) loading = false;
		}
	}

	// React to statusFilter changes (and initial mount)
	$effect(() => {
		load(statusFilter);
	});

	function sourceIcon(type: string | null): string {
		switch (type) {
			case "issue":
				return "◆";
			case "branch":
				return "⑂";
			case "manual":
				return "✎";
			default:
				return "•";
		}
	}

	function statusBadgeClass(status: string): string {
		switch (status) {
			case "active":
				return "bg-blue-500/15 text-blue-400 border-blue-500/25";
			case "open":
				return "bg-green-500/15 text-green-400 border-green-500/25";
			case "resolved":
				return "bg-amber-500/15 text-amber-400 border-amber-500/25";
			case "archived":
				return "bg-zinc-500/15 text-zinc-500 border-zinc-500/25";
			default:
				return "bg-zinc-500/15 text-zinc-500 border-zinc-500/25";
		}
	}

	function formatDate(iso: string): string {
		const d = new Date(iso);
		return d.toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
	}

	function sagaHref(saga: Saga): string {
		return `/r/${encodeURIComponent(repoName)}/sagas/${encodeURIComponent(saga.id)}`;
	}

	function handleSagaClick(e: MouseEvent, saga: Saga): void {
		e.preventDefault();
		goto(sagaHref(saga));
	}
</script>

<div class="p-6 max-w-5xl mx-auto">
	<!-- Header -->
	<div class="flex items-center justify-between mb-5">
		<h2 class="font-mono text-sm font-medium text-slate-200 tracking-wide">
			Sagas
			<span class="text-slate-600 ml-2 text-[10px] font-normal">{repoName}</span>
		</h2>

		<!-- Status filter -->
		<select
			bind:value={statusFilter}
			class="font-mono text-[11px] bg-zinc-900 border border-zinc-800 text-slate-300 rounded-md px-2.5 py-1.5 focus:border-blue-500/50 focus:outline-none appearance-none cursor-pointer"
		>
			{#each STATUS_OPTIONS as opt (opt.value)}
				<option value={opt.value}>{opt.label}</option>
			{/each}
		</select>
	</div>

	<!-- Loading skeleton -->
	{#if loading}
		<div class="space-y-2" transition:fade={{ duration: 150 }}>
			{#each Array(5) as _, i (i)}
				<div class="bg-zinc-900 border border-zinc-800/60 rounded-md px-4 py-3">
					<div class="flex items-center gap-3">
						<div class="h-3 w-48 bg-white/[0.04] rounded animate-pulse"></div>
						<div class="h-3 w-16 bg-white/[0.04] rounded animate-pulse ml-auto"></div>
					</div>
				</div>
			{/each}
		</div>

		<!-- Error state -->
	{:else if error}
		<div class="text-center py-16" transition:fade={{ duration: 150 }}>
			<p class="font-mono text-xs text-red-400">{error}</p>
		</div>

		<!-- Empty state -->
	{:else if sagas.length === 0}
		<div class="text-center py-16" transition:fade={{ duration: 150 }}>
			<p class="font-mono text-xs text-slate-600">
				No sagas found{statusFilter ? ` with status "${statusFilter}"` : ""}.
			</p>
		</div>

		<!-- Saga cards -->
	{:else}
		<div class="space-y-1.5" transition:fade={{ duration: 150 }}>
			{#each sagas as saga (saga.id)}
				<a
					href={sagaHref(saga)}
					onclick={(e) => handleSagaClick(e, saga)}
					class="block w-full text-left bg-zinc-900 border border-zinc-800/60 rounded-md px-4 py-3 hover:bg-zinc-800/60 hover:border-zinc-700/60 transition-colors cursor-pointer group no-underline"
				>
					<div class="flex items-center gap-3 min-w-0">
						<!-- Source type icon -->
						<span
							class="font-mono text-sm shrink-0 {saga.source_type === 'issue'
								? 'text-blue-400'
								: saga.source_type === 'branch'
									? 'text-green-400'
									: 'text-amber-400'}"
							title={saga.source_type ?? "unknown"}
						>
							{sourceIcon(saga.source_type)}
						</span>

						<!-- Name -->
						<span
							class="font-mono text-[12px] text-slate-200 truncate group-hover:text-blue-300 transition-colors"
						>
							{saga.name ?? saga.source_ref ?? saga.id.slice(0, 8)}
						</span>

						<!-- Source ref (if different from name) -->
						{#if saga.source_ref && saga.source_ref !== saga.name}
							<span class="font-mono text-[10px] text-slate-600 truncate shrink-0">
								{saga.source_ref}
							</span>
						{/if}

						<!-- Spacer -->
						<span class="flex-1"></span>

						<!-- Note count -->
						<span class="font-mono text-[10px] text-slate-500 shrink-0" title="Notes">
							{saga.note_count} note{saga.note_count !== 1 ? "s" : ""}
						</span>

						<!-- Status badge -->
						<span
							class="font-mono text-[10px] px-2 py-0.5 rounded border shrink-0 {statusBadgeClass(
								saga.status,
							)}"
						>
							{saga.status}
						</span>

						<!-- Created date -->
						<span class="font-mono text-[10px] text-slate-600 shrink-0 w-20 text-right">
							{formatDate(saga.created_at)}
						</span>
					</div>
				</a>
			{/each}
		</div>
	{/if}
</div>
