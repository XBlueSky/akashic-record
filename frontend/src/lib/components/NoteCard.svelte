<script lang="ts">
	import type { Note, Category } from "$lib/types/index.js";
	import * as Card from "$lib/components/ui/card";
	import { Badge } from "$lib/components/ui/badge";
	import { Pencil, Trash2, ChevronDown, ChevronRight } from "@lucide/svelte";
	import { t, locale } from "svelte-i18n";
	import { Marked } from "marked";
	import DOMPurify from "dompurify";
	import { slide } from "svelte/transition";

	// Module-local instance — avoids mutating the global marked singleton.
	const md = new Marked({ breaks: true, gfm: true });

	let {
		note,
		highlight = false,
		canEdit = false,
		canDelete = false,
		onedit,
		ondelete,
	}: {
		note: Note;
		highlight?: boolean;
		canEdit?: boolean;
		canDelete?: boolean;
		onedit?: (uuid: string) => void;
		ondelete?: (uuid: string) => void;
	} = $props();

	let expanded = $state(false);

	const categoryColor: Record<Category, string> = {
		ARCHITECTURE: "#3b82f6",
		BUG_FIX: "#ef4444",
		CONFIG: "#f59e0b",
		ONBOARDING: "#22c55e",
		DECISION: "#a855f6",
	};

	const categoryVariant: Record<Category, string> = {
		ARCHITECTURE: "bg-blue-500/10 text-blue-400 border-blue-500/20",
		BUG_FIX: "bg-red-500/10 text-red-400 border-red-500/20",
		CONFIG: "bg-amber-500/10 text-amber-400 border-amber-500/20",
		ONBOARDING: "bg-green-500/10 text-green-400 border-green-500/20",
		DECISION: "bg-purple-500/10 text-purple-400 border-purple-500/20",
	};

	function formatCategory(cat: Category): string {
		return cat.replace(/_/g, " ");
	}

	function formatDate(iso: string): string {
		const d = new Date(iso);
		const loc = $locale === "zh-TW" ? "zh-TW" : "en-US";
		return d.toLocaleDateString(loc, {
			year: "numeric",
			month: "short",
			day: "numeric",
			hour: "2-digit",
			minute: "2-digit",
		});
	}

	function renderContent(raw: string): string {
		return DOMPurify.sanitize(md.parse(raw) as string);
	}

	function toggleExpand() {
		expanded = !expanded;
	}
</script>

<!-- Category left stripe via border-left -->
<Card.Root
	id="note-{note.uuid}"
	class="rounded-xl border !py-0 transition-all hover:bg-white/[0.03] {highlight
		? 'border-amber-500/50 bg-amber-500/[0.06] ring-1 ring-amber-500/30'
		: 'border-white/10 bg-white/[0.02]'}"
	style="border-left: 3px solid {categoryColor[note.category]};"
>
	<!-- Header: category + meta + actions -->
	<Card.Header class="flex flex-row flex-wrap items-center justify-between gap-1.5 px-3 py-1.5">
		<div class="flex items-center gap-1.5 flex-wrap">
			<Badge
				variant="outline"
				class="rounded-full px-2 py-0.5 text-[10px] font-bold uppercase tracking-wide {categoryVariant[
					note.category
				]}"
			>
				{formatCategory(note.category)}
			</Badge>
			{#if note.saga_id}
				<Badge
					variant="outline"
					class="rounded-full px-2 py-0.5 text-[10px] font-bold uppercase tracking-wide bg-amber-500/10 text-amber-400 border-amber-500/20"
				>
					saga
				</Badge>
			{/if}
			{#if note.invalid_at}
				<Badge
					variant="outline"
					class="rounded-full px-2 py-0.5 text-[10px] font-bold uppercase tracking-wide bg-white/5 text-slate-500 border-white/10"
				>
					superseded
				</Badge>
			{/if}
		</div>
		<div class="flex items-center gap-2 text-xs text-slate-500">
			{#if note.branch_name}
				<span class="rounded bg-white/5 px-1.5 py-0.5 font-mono text-[10px] text-slate-400">
					{note.branch_name}
				</span>
			{/if}
			<span>{formatDate(note.created_at)}</span>
			{#if canEdit}
				<button
					class="p-1 rounded hover:bg-white/10 text-slate-500 hover:text-slate-300 transition-colors cursor-pointer bg-transparent border-none focus-visible:ring-1 focus-visible:ring-blue-500/50 outline-none"
					title="Edit note"
					onclick={(e) => {
						e.stopPropagation();
						onedit?.(note.uuid);
					}}
				>
					<Pencil size={13} />
				</button>
			{/if}
			{#if canDelete}
				<button
					class="p-1 rounded hover:bg-red-500/20 text-slate-500 hover:text-red-400 transition-colors cursor-pointer bg-transparent border-none focus-visible:ring-1 focus-visible:ring-red-500/50 outline-none"
					title="Delete note"
					onclick={(e) => {
						e.stopPropagation();
						ondelete?.(note.uuid);
					}}
				>
					<Trash2 size={13} />
				</button>
			{/if}
		</div>
	</Card.Header>

	<!-- Clickable summary row — expand/collapse -->
	{#if note.summary}
		<div
			class="flex items-start gap-2 px-3 py-1.5 cursor-pointer hover:bg-white/[0.02] transition-colors select-none"
			role="button"
			tabindex="0"
			onclick={toggleExpand}
			onkeydown={(e) => e.key === "Enter" && toggleExpand()}
		>
			<span class="mt-0.5 shrink-0 text-slate-500">
				{#if expanded}
					<ChevronDown size={14} />
				{:else}
					<ChevronRight size={14} />
				{/if}
			</span>
			<span class="text-sm font-semibold text-slate-100 leading-snug">
				{note.summary}
			</span>
		</div>
	{/if}

	<!-- Collapsible content -->
	{#if expanded}
		<div transition:slide={{ duration: 200 }}>
			{#if note.facts && note.facts.length > 0}
				<div class="px-3 py-1 ml-5">
					<ul class="list-none space-y-0">
						{#each note.facts as fact (fact)}
							<li
								class="relative pl-3 text-xs leading-normal text-slate-400 before:absolute before:left-0 before:text-slate-600 before:content-['•']"
							>
								{fact}
							</li>
						{/each}
					</ul>
				</div>
			{/if}

			<Card.Content class="px-3 py-1.5 ml-5 text-sm leading-normal text-slate-400 note-prose">
				<!-- eslint-disable-next-line svelte/no-at-html-tags -- renderContent() always runs content through DOMPurify.sanitize() before rendering -->
				{@html renderContent(note.content)}
			</Card.Content>

			{#if note.related_files && note.related_files.length > 0}
				<div class="flex flex-wrap items-center gap-1 border-t border-white/5 px-3 py-1.5 ml-5">
					<span
						class="mr-1 font-mono text-[10px] font-semibold uppercase tracking-wider text-muted-foreground"
						>{$t("repo.files")}</span
					>
					{#each note.related_files as file (file)}
						<Badge
							variant="outline"
							class="border-blue-500/20 bg-blue-500/10 font-mono text-[10px] text-blue-400"
						>
							{file}
						</Badge>
					{/each}
				</div>
			{/if}

			{#if note.related_symbols && note.related_symbols.length > 0}
				<div class="flex flex-wrap items-center gap-1 border-t border-white/5 px-3 py-1.5 ml-5">
					<span
						class="mr-1 font-mono text-[10px] font-semibold uppercase tracking-wider text-muted-foreground"
						>{$t("repo.symbols")}</span
					>
					{#each note.related_symbols as sym (sym)}
						<Badge
							variant="outline"
							class="border-green-500/20 bg-green-500/10 font-mono text-[10px] text-green-400"
						>
							{sym}
						</Badge>
					{/each}
				</div>
			{/if}
		</div>
	{/if}

	<!-- Tags always visible (scannable even when collapsed) -->
	{#if note.tags && note.tags.length > 0}
		<Card.Footer class="flex flex-wrap gap-1 border-t border-white/5 px-3 !pt-1.5 !pb-1.5">
			{#each note.tags as tag (tag)}
				<Badge
					variant="outline"
					class="rounded-full border-white/10 bg-white/[0.03] px-1.5 py-0 text-[10px] text-slate-500 font-mono"
				>
					{tag}
				</Badge>
			{/each}
		</Card.Footer>
	{/if}
</Card.Root>

<style>
	/* Compact prose — dense developer dashboard, not blog */
	:global(.note-prose h1) {
		font-size: 1.1em;
		font-weight: 700;
		margin: 0.4em 0 0.15em;
		color: #e2e8f0;
	}
	:global(.note-prose h2) {
		font-size: 1em;
		font-weight: 600;
		margin: 0.35em 0 0.1em;
		color: #e2e8f0;
	}
	:global(.note-prose h3) {
		font-size: 0.95em;
		font-weight: 600;
		margin: 0.3em 0 0.1em;
		color: #cbd5e1;
	}
	:global(.note-prose ul) {
		list-style: disc;
		padding-left: 1.25em;
		margin: 0.15em 0;
	}
	:global(.note-prose ol) {
		list-style: decimal;
		padding-left: 1.25em;
		margin: 0.15em 0;
	}
	:global(.note-prose li) {
		margin: 0;
	}
	:global(.note-prose blockquote) {
		border-left: 2px solid #475569;
		padding-left: 0.6em;
		margin: 0.2em 0;
		color: #94a3b8;
		font-style: italic;
	}
	:global(.note-prose pre) {
		overflow-x: auto;
		border-radius: 0.25rem;
		border: 1px solid hsl(var(--border));
		background: #09090b;
		padding: 0.5rem;
		margin: 0.25em 0;
		font-size: 0.8em;
		line-height: 1.4;
	}
	:global(.note-prose code) {
		font-family: ui-monospace, monospace;
	}
	:global(.note-prose :not(pre) > code) {
		background: hsl(var(--muted));
		padding: 0.1em 0.3em;
		border-radius: 0.2rem;
		font-size: 0.85em;
		color: #60a5fa;
	}
	:global(.note-prose p) {
		margin: 0.15em 0;
	}
	:global(.note-prose p:first-child) {
		margin-top: 0;
	}
	:global(.note-prose p:last-child) {
		margin-bottom: 0;
	}
	:global(.note-prose a) {
		color: #60a5fa;
		text-decoration: underline;
	}
	:global(.note-prose table) {
		border-collapse: collapse;
		margin: 0.25em 0;
		width: 100%;
	}
	:global(.note-prose th, .note-prose td) {
		border: 1px solid #334155;
		padding: 0.2em 0.5em;
		font-size: 0.85em;
	}
	:global(.note-prose th) {
		background: #1e293b;
		font-weight: 600;
	}
</style>
