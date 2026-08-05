<script lang="ts">
	import { onMount, onDestroy } from "svelte";
	import { fetchRepos, fetchActiveJobs, isAuthError } from "$lib/api";
	import type { Repository, ActiveJob } from "$lib/types";
	import { connectEvents, onEvent, disconnectEvents } from "$lib/events";
	import {
		Loader,
		Plus,
		LogOut,
		LogIn,
		Search,
		Globe,
		GitBranch,
		PanelTop,
		User,
		Waypoints,
		Settings,
	} from "@lucide/svelte";
	import Logo from "$lib/components/Logo.svelte";
	import { auth } from "$lib/state/auth.svelte";
	import { commandPalette } from "$lib/state/command-palette.svelte";
	import * as Sidebar from "$lib/components/ui/sidebar";
	import { t, locale } from "svelte-i18n";
	import * as Popover from "$lib/components/ui/popover";
	import { goto } from "$app/navigation";
	import { page } from "$app/state";

	function setLocale(loc: string) {
		locale.set(loc);
	}

	let repos = $state<Repository[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let activeJobs = $state<ActiveJob[]>([]);

	// Derive selected repo from URL params
	let selectedRepo = $derived(page.params.repo ?? null);

	let unsubJobUpdate: (() => void) | null = null;
	let unsubReposChanged: (() => void) | null = null;

	let isRepoActive = $derived.by(
		() =>
			(name: string): boolean =>
				activeJobs.some((j) => j.repo_name === name),
	);

	let gitlabRepos = $derived(repos.filter((r) => r.source_type !== "website"));
	let webRepos = $derived(repos.filter((r) => r.source_type === "website"));

	onMount(async () => {
		try {
			repos = await fetchRepos();
		} catch (e: unknown) {
			if (!isAuthError(e)) error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}

		// Initial fetch of active jobs, then rely on SSE
		try {
			activeJobs = await fetchActiveJobs();
		} catch (e: unknown) {
			console.warn("Failed to fetch active jobs:", e);
		}

		connectEvents();

		// Live job updates — update activeJobs in-place from SSE data
		unsubJobUpdate = onEvent("job_update", (raw) => {
			const data = raw as {
				repo_name: string;
				status: string;
				processed_files: number | null;
				total_files: number | null;
			};
			const done = ["done", "completed", "failed"].includes(data.status);
			if (done) {
				activeJobs = activeJobs.filter((j) => j.repo_name !== data.repo_name);
			} else {
				const existing = activeJobs.find((j) => j.repo_name === data.repo_name);
				if (existing) {
					activeJobs = activeJobs.map((j) =>
						j.repo_name === data.repo_name
							? {
									...j,
									status: data.status,
									processed_files: data.processed_files,
									total_files: data.total_files,
								}
							: j,
					);
				} else {
					activeJobs = [
						...activeJobs,
						{
							repo_name: data.repo_name,
							status: data.status,
							processed_files: data.processed_files,
							total_files: data.total_files,
						},
					];
				}
			}
		});

		// Repo list changed — re-fetch
		unsubReposChanged = onEvent("repos_changed", async () => {
			try {
				repos = await fetchRepos();
			} catch (e: unknown) {
				console.warn("Failed to refresh repos:", e);
			}
		});
	});

	onDestroy(() => {
		unsubJobUpdate?.();
		unsubReposChanged?.();
		disconnectEvents();
	});

	function selectRepo(name: string | null) {
		if (name === null) {
			goto("/");
		} else {
			goto("/r/" + encodeURIComponent(name));
		}
	}

	function triggerSearch() {
		commandPalette.show();
	}

	function splitName(name: string): { host: string; path: string } | null {
		const idx = name.indexOf("/");
		if (idx < 0) return null;
		return { host: name.slice(0, idx), path: name.slice(idx + 1) };
	}
</script>

<Sidebar.Root>
	<!-- Header: Logo + Search -->
	<Sidebar.Header class="px-4 pb-0 pt-4">
		<button
			class="flex items-center gap-2.5 transition-opacity duration-200 hover:opacity-85 cursor-pointer bg-transparent border-none p-0"
			onclick={() => selectRepo(null)}
		>
			<Logo />
		</button>

		<!-- Search trigger (fake input) -->
		<button
			class="flex items-center justify-between w-full px-3 py-2 mt-4 mb-2 bg-white/5 border border-white/5 rounded-lg hover:bg-white/10 transition-colors cursor-pointer group"
			onclick={triggerSearch}
		>
			<span class="flex items-center gap-2">
				<Search
					size={14}
					strokeWidth={2}
					class="text-slate-500 group-hover:text-slate-300 transition-colors"
				/>
				<span class="font-mono text-sm text-slate-500 group-hover:text-slate-300 transition-colors"
					>{$t("sidebar.searchPlaceholder")}</span
				>
			</span>
			<kbd
				class="flex items-center justify-center px-1.5 py-0.5 text-[10px] font-mono text-slate-500 bg-black/20 border border-white/10 rounded"
				>⌘K</kbd
			>
		</button>
	</Sidebar.Header>

	<Sidebar.Separator />

	<Sidebar.Content>
		{#if loading}
			<Sidebar.Group>
				<Sidebar.GroupContent>
					<Sidebar.Menu>
						<Sidebar.MenuItem>
							<Sidebar.MenuButton
								class="font-mono text-muted-foreground text-xs italic pointer-events-none"
							>
								{$t("sidebar.loading")}
							</Sidebar.MenuButton>
						</Sidebar.MenuItem>
					</Sidebar.Menu>
				</Sidebar.GroupContent>
			</Sidebar.Group>
		{:else if error}
			<Sidebar.Group>
				<Sidebar.GroupContent>
					<Sidebar.Menu>
						<Sidebar.MenuItem>
							<Sidebar.MenuButton
								class="font-mono text-destructive text-xs italic pointer-events-none"
							>
								{error}
							</Sidebar.MenuButton>
						</Sidebar.MenuItem>
					</Sidebar.Menu>
				</Sidebar.GroupContent>
			</Sidebar.Group>
		{:else if repos.length === 0}
			<Sidebar.Group>
				<Sidebar.GroupContent>
					<Sidebar.Menu>
						<Sidebar.MenuItem>
							<Sidebar.MenuButton
								class="font-mono text-muted-foreground text-xs italic pointer-events-none"
							>
								{$t("sidebar.noRepos")}
							</Sidebar.MenuButton>
						</Sidebar.MenuItem>
					</Sidebar.Menu>
				</Sidebar.GroupContent>
			</Sidebar.Group>
		{:else}
			<!-- GitLab Repos Group -->
			{#if gitlabRepos.length > 0}
				<Sidebar.Group>
					<Sidebar.GroupLabel
						class="font-mono text-[10px] font-semibold uppercase tracking-[0.14em] text-slate-500"
					>
						{$t("sidebar.gitlabRepos")}
					</Sidebar.GroupLabel>
					<Sidebar.GroupContent>
						<Sidebar.Menu>
							{#each gitlabRepos as repo (repo.name)}
								<Sidebar.MenuItem>
									<Sidebar.MenuButton
										size="lg"
										isActive={selectedRepo === repo.name}
										onclick={() => selectRepo(selectedRepo === repo.name ? null : repo.name)}
										class="gap-2 group"
									>
										{#if isRepoActive(repo.name)}
											<span
												class="inline-flex h-3.5 w-3.5 shrink-0 items-center justify-center text-blue-400 animate-pulse"
											>
												<Loader size={12} strokeWidth={2} class="animate-spin" />
											</span>
										{:else}
											<GitBranch
												size={14}
												strokeWidth={1.5}
												class="w-3.5 h-3.5 shrink-0 text-slate-500 group-hover:text-blue-400 transition-colors"
											/>
										{/if}
										{@const parts = splitName(repo.name)}
										{#if parts}
											<div class="flex flex-col min-w-0 overflow-hidden" title={repo.name}>
												<span
													class="truncate font-mono text-sm text-slate-300 group-hover:text-blue-400 transition-colors"
													>{parts.path}</span
												>
												<span class="truncate text-[10px] font-mono text-slate-500 mt-0.5"
													>{parts.host}</span
												>
											</div>
										{:else}
											<span class="truncate font-mono text-xs tracking-wide" title={repo.name}
												>{repo.name}</span
											>
										{/if}
									</Sidebar.MenuButton>
								</Sidebar.MenuItem>
							{/each}
						</Sidebar.Menu>
					</Sidebar.GroupContent>
				</Sidebar.Group>
			{/if}

			<!-- Web Crawler Group -->
			{#if webRepos.length > 0}
				<Sidebar.Group class="mt-2">
					<Sidebar.GroupLabel
						class="font-mono text-[10px] font-semibold uppercase tracking-[0.14em] text-slate-500"
					>
						{$t("sidebar.webCrawler")}
					</Sidebar.GroupLabel>
					<Sidebar.GroupContent>
						<Sidebar.Menu>
							{#each webRepos as repo (repo.name)}
								<Sidebar.MenuItem>
									<Sidebar.MenuButton
										size="lg"
										isActive={selectedRepo === repo.name}
										onclick={() => selectRepo(selectedRepo === repo.name ? null : repo.name)}
										class="gap-2 group"
									>
										{#if isRepoActive(repo.name)}
											<span
												class="inline-flex h-3.5 w-3.5 shrink-0 items-center justify-center text-blue-400 animate-pulse"
											>
												<Loader size={12} strokeWidth={2} class="animate-spin" />
											</span>
										{:else}
											<PanelTop
												size={14}
												strokeWidth={1.5}
												class="w-3.5 h-3.5 shrink-0 text-slate-500 group-hover:text-blue-400 transition-colors"
											/>
										{/if}
										{@const parts = splitName(repo.name)}
										{#if parts}
											<div class="flex flex-col min-w-0 overflow-hidden" title={repo.name}>
												<span
													class="truncate font-mono text-sm text-slate-300 group-hover:text-blue-400 transition-colors"
													>{parts.path}</span
												>
												<span class="truncate text-[10px] font-mono text-slate-500 mt-0.5"
													>{parts.host}</span
												>
											</div>
										{:else}
											<span class="truncate font-mono text-xs tracking-wide" title={repo.name}
												>{repo.name}</span
											>
										{/if}
									</Sidebar.MenuButton>
								</Sidebar.MenuItem>
							{/each}
						</Sidebar.Menu>
					</Sidebar.GroupContent>
				</Sidebar.Group>
			{/if}
		{/if}

		<!-- + Add Source / Sagas nav -->
		<div class="px-2 mt-4">
			<button
				class="flex items-center gap-2 w-full px-2 py-2 text-xs font-mono text-slate-500 hover:text-slate-300 hover:bg-white/5 rounded-md cursor-pointer transition-colors bg-transparent border-none"
				onclick={() => goto("/ingest")}
			>
				<Plus size={14} strokeWidth={2} class="shrink-0" />
				<span class="tracking-wide">{$t("sidebar.addSource")}</span>
			</button>
			{#if selectedRepo}
				<button
					class="flex items-center gap-2 w-full px-2 py-2 text-xs font-mono text-slate-500 hover:text-slate-300 hover:bg-white/5 rounded-md cursor-pointer transition-colors bg-transparent border-none"
					onclick={() => goto(`/r/${encodeURIComponent(selectedRepo)}/sagas`)}
				>
					<Waypoints size={14} strokeWidth={2} class="shrink-0" />
					<span class="tracking-wide">Sagas</span>
				</button>
			{/if}
		</div>
	</Sidebar.Content>

	<!-- Footer -->
	<Sidebar.Footer>
		<!-- Locale switcher -->
		<div class="flex items-center px-2 py-1">
			<Popover.Root>
				<Popover.Trigger>
					<button
						class="flex items-center gap-1.5 rounded-md px-2 py-1 text-xs text-muted-foreground transition-colors hover:text-foreground hover:bg-accent cursor-pointer bg-transparent border-none"
					>
						<Globe size={12} strokeWidth={2} />
						<span class="font-mono text-[10px] uppercase tracking-wider">
							{$locale === "zh-TW" ? "中文" : "EN"}
						</span>
					</button>
				</Popover.Trigger>
				<Popover.Content
					align="start"
					class="min-w-[120px] p-0 bg-[#0f1729]/80 backdrop-blur-xl border border-white/10 shadow-[0_4px_20px_-5px_rgba(0,0,0,0.5)] rounded-lg overflow-hidden"
				>
					<button
						class="flex w-full items-center font-mono text-sm cursor-pointer bg-transparent border-none transition-colors {$locale ===
						'en'
							? 'bg-blue-500/10 text-blue-400 border-l-[3px] border-blue-500 px-2.5 py-2'
							: 'text-slate-400 hover:text-slate-200 hover:bg-white/5 px-3 py-2'}"
						onclick={() => setLocale("en")}
					>
						EN
					</button>
					<button
						class="flex w-full items-center font-mono text-sm cursor-pointer bg-transparent border-none transition-colors {$locale ===
						'zh-TW'
							? 'bg-blue-500/10 text-blue-400 border-l-[3px] border-blue-500 px-2.5 py-2'
							: 'text-slate-400 hover:text-slate-200 hover:bg-white/5 px-3 py-2'}"
						onclick={() => setLocale("zh-TW")}
					>
						繁體中文
					</button>
				</Popover.Content>
			</Popover.Root>
		</div>

		<Sidebar.Separator />

		<!-- User badge -->
		{#if auth.user}
			<div class="flex items-center gap-2 px-2 py-1.5">
				{#if auth.user.avatar_url}
					<img
						class="h-6 w-6 shrink-0 rounded-full"
						src={auth.user.avatar_url}
						alt={auth.user.username}
					/>
				{:else}
					<span
						class="flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-xs font-bold"
						style="background: rgba(59,130,246,0.15); color: var(--accent-blue, #3b82f6)"
					>
						{auth.user.username.charAt(0).toUpperCase()}
					</span>
				{/if}
				<span class="flex-1 truncate font-mono text-xs text-muted-foreground">
					{auth.user.username}
				</span>
				<button
					class="cursor-pointer border-none bg-transparent p-1 text-muted-foreground transition-colors duration-150 hover:text-blue-500"
					title={$t("sidebar.manageTokens")}
					aria-label={$t("sidebar.manageTokens")}
					onclick={() => {
						const u = new URL(page.url);
						u.searchParams.set("settings", "tokens");
						goto(u.pathname + u.search);
					}}
				>
					<Settings size={14} strokeWidth={2} />
				</button>
				<button
					class="cursor-pointer border-none bg-transparent p-1 text-muted-foreground transition-colors duration-150 hover:text-red-500"
					onclick={auth.logout.bind(auth)}
					title={$t("sidebar.signOut")}
				>
					<LogOut size={14} strokeWidth={2} />
				</button>
			</div>
		{:else}
			<div class="flex items-center gap-2 px-2 py-1.5">
				<span
					class="flex h-6 w-6 shrink-0 items-center justify-center rounded-full"
					style="background: rgba(255,255,255,0.05)"
				>
					<User size={12} strokeWidth={1.5} class="text-slate-600" />
				</span>
				<span class="flex-1 truncate font-mono text-xs text-slate-600"> anonymous </span>
				<button
					class="cursor-pointer border-none bg-transparent p-1 text-muted-foreground transition-colors duration-150 hover:text-blue-400"
					onclick={auth.login.bind(auth)}
					title="Sign in with GitLab"
				>
					<LogIn size={14} strokeWidth={2} />
				</button>
			</div>
		{/if}
	</Sidebar.Footer>
</Sidebar.Root>
