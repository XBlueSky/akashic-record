<script lang="ts">
	import { fetchGitLabBranches } from "$lib/api";
	import type { GitLabBranch } from "$lib/types/index.js";
	import { t } from "svelte-i18n";
	import { get } from "svelte/store";
	import * as Card from "$lib/components/ui/card";
	import { Input } from "$lib/components/ui/input";
	import { Label } from "$lib/components/ui/label";
	import { Button } from "$lib/components/ui/button";
	import { Badge } from "$lib/components/ui/badge";
	import * as Popover from "$lib/components/ui/popover";
	import * as Command from "$lib/components/ui/command";
	import { ArrowLeft, GitBranch, ChevronDown, Check, Loader } from "@lucide/svelte";

	let {
		onBack,
		onSubmit,
	}: {
		onBack: () => void;
		onSubmit: (data: { url: string; git_ref: string }) => void | Promise<void>;
	} = $props();

	let url = $state("");
	let gitRef = $state("");
	let error = $state<string | null>(null);
	// In-flight submit guard — prevents rapid double-clicks from firing twice.
	let submitting = $state(false);

	// Branch dropdown state
	let branches = $state<GitLabBranch[]>([]);
	let branchesLoading = $state(false);
	let branchesError = $state<string | null>(null);
	let branchOpen = $state(false);
	let filterText = $state("");
	let lastFetchedRepo = $state("");

	async function handleSubmit() {
		if (submitting) return;
		error = null;
		const trimmed = url.trim();
		if (!trimmed) {
			error = get(t)("ingest.gitlab.urlRequired");
			return;
		}
		if (!trimmed.startsWith("http")) {
			error = get(t)("ingest.gitlab.urlInvalid");
			return;
		}
		submitting = true;
		try {
			await onSubmit({ url: trimmed, git_ref: gitRef.trim() || "main" });
		} finally {
			submitting = false;
		}
	}

	let repoName = $derived(extractRepoName(url));

	// Auto-fetch branches when a valid repo URL is detected.
	$effect(() => {
		if (repoName !== "—" && repoName !== lastFetchedRepo) {
			loadBranches(repoName);
		}
	});

	async function loadBranches(repo: string) {
		// Guard against out-of-order async responses.
		lastFetchedRepo = repo;
		branchesLoading = true;
		branchesError = null;
		branches = [];
		gitRef = "";
		try {
			const result = await fetchGitLabBranches(repo);
			if (lastFetchedRepo !== repo) return;
			branches = result;
			const defaultBranch = result.find((b) => b.default);
			gitRef = defaultBranch?.name ?? result[0]?.name ?? "main";
		} catch (err: unknown) {
			if (lastFetchedRepo !== repo) return;
			branchesError = err instanceof Error ? err.message : get(t)("ingest.gitlab.branchError");
			gitRef = "main";
		} finally {
			if (lastFetchedRepo === repo) branchesLoading = false;
		}
	}

	let filteredBranches = $derived(
		branches.filter((b) => b.name.toLowerCase().includes(filterText.toLowerCase())),
	);

	function selectBranch(name: string) {
		gitRef = name;
		branchOpen = false;
		filterText = "";
	}

	function extractRepoName(u: string): string {
		try {
			const path = new URL(u).pathname.replace(/^\//, "").replace(/\.git$/, "");
			return path || "—";
		} catch {
			return "—";
		}
	}
</script>

<div class="mx-auto w-full max-w-[480px]">
	<Button
		variant="ghost"
		size="sm"
		onclick={onBack}
		class="mb-4 gap-1 font-mono text-xs text-muted-foreground"
	>
		<ArrowLeft size={14} />
		{$t("common.back")}
	</Button>

	<Card.Root class="border-border/50 bg-card/80 backdrop-blur-xl">
		<Card.Header>
			<span class="font-mono text-[10px] font-medium tracking-[0.12em] uppercase text-primary/35">
				{$t("ingest.gitlab.label")}
			</span>
			<Card.Title class="font-mono text-lg font-bold tracking-[0.15em]">
				{$t("ingest.gitlab.title")}
			</Card.Title>
		</Card.Header>

		<Card.Content class="space-y-4">
			<!-- URL field -->
			<div class="space-y-1.5">
				<Label
					class="font-mono text-xs font-medium tracking-[0.06em] uppercase text-secondary-foreground"
				>
					{$t("ingest.gitlab.urlLabel")}
				</Label>
				<Input
					type="text"
					bind:value={url}
					placeholder={$t("ingest.gitlab.urlPlaceholder")}
					class="font-mono"
				/>
			</div>

			<!-- Detected repo name -->
			{#if repoName !== "—"}
				<div class="rounded-md border border-primary/12 bg-primary/6 px-2.5 py-1.5 text-xs">
					<span class="text-muted-foreground">{$t("ingest.gitlab.detected")}</span>
					<span class="ml-1.5 font-mono text-primary">{repoName}</span>
				</div>
			{/if}

			<!-- Branch selector -->
			<div class="space-y-1.5">
				<Label
					class="font-mono text-xs font-medium tracking-[0.06em] uppercase text-secondary-foreground"
				>
					{$t("ingest.gitlab.branchLabel")}
				</Label>

				{#if branchesLoading}
					<div
						class="flex items-center gap-2 rounded-md border border-border px-3 py-2 font-mono text-xs text-muted-foreground"
					>
						<Loader size={12} class="animate-spin" />
						{$t("ingest.gitlab.branchLoading")}
					</div>
				{:else if branches.length > 0}
					<Popover.Root bind:open={branchOpen}>
						<Popover.Trigger class="w-full">
							<button
								type="button"
								class="flex w-full items-center justify-between rounded-md border border-input bg-background px-3 py-2 font-mono text-sm text-foreground shadow-xs transition-colors hover:border-primary"
							>
								<span class="inline-flex items-center gap-1.5">
									<GitBranch size={12} strokeWidth={1.5} />
									{gitRef}
								</span>
								<ChevronDown size={12} class="text-muted-foreground" />
							</button>
						</Popover.Trigger>
						<Popover.Content
							class="w-[var(--bits-popover-anchor-width)] overflow-hidden rounded-lg border border-white/10 bg-[#0f1729]/80 p-0 shadow-[0_4px_20px_-5px_rgba(0,0,0,0.5)] backdrop-blur-xl"
							align="start"
						>
							<Command.Root shouldFilter={false}>
								<Command.Input
									placeholder={$t("ingest.gitlab.branchFilter")}
									bind:value={filterText}
								/>
								<Command.List>
									<Command.Empty>{$t("ingest.gitlab.branchEmpty")}</Command.Empty>
									<Command.Group>
										{#each filteredBranches as branch (branch.name)}
											<Command.Item
												value={branch.name}
												onSelect={() => selectBranch(branch.name)}
												class={gitRef === branch.name
													? "bg-primary/8 text-primary"
													: "cursor-pointer px-3 py-2 text-sm text-slate-400 transition-colors hover:bg-white/5 hover:text-slate-200"}
											>
												<GitBranch size={12} strokeWidth={1.5} />
												{branch.name}
												{#if branch.default}
													<Badge variant="outline" class="ml-auto font-mono text-[10px]">
														{$t("ingest.gitlab.default")}
													</Badge>
												{/if}
												{#if gitRef === branch.name}
													<Check size={12} class="ml-auto" />
												{/if}
											</Command.Item>
										{/each}
									</Command.Group>
								</Command.List>
							</Command.Root>
						</Popover.Content>
					</Popover.Root>

					{#if branchesError}
						<p class="mt-1 font-mono text-xs text-yellow-500">{branchesError}</p>
					{/if}
				{:else}
					<Input type="text" bind:value={gitRef} placeholder="main" class="font-mono" />
					{#if branchesError}
						<p class="mt-1 font-mono text-xs text-yellow-500">{branchesError}</p>
					{/if}
				{/if}
			</div>

			<!-- Error message -->
			{#if error}
				<p class="text-sm text-destructive">{error}</p>
			{/if}
		</Card.Content>

		<Card.Footer>
			<Button
				class="w-full font-mono text-sm font-bold tracking-[0.1em]"
				onclick={handleSubmit}
				disabled={!url.trim() || submitting}
			>
				{$t("ingest.gitlab.submit")}
			</Button>
		</Card.Footer>
	</Card.Root>
</div>
