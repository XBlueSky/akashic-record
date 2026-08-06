<script lang="ts">
	import "../app.css";
	import "$lib/i18n";
	import { onMount } from "svelte";
	import { page } from "$app/state";
	import { goto, onNavigate } from "$app/navigation";
	import { auth } from "$lib/state/auth.svelte";
	import { SidebarProvider, SidebarInset } from "$lib/components/ui/sidebar/index.js";
	import Sidebar from "$lib/components/Sidebar.svelte";
	import CommandPalette from "$lib/components/CommandPalette.svelte";
	import Toast from "$lib/components/Toast.svelte";
	import TokenManagementModal from "$lib/components/TokenManagementModal.svelte";

	let { children } = $props();

	let showTokenModal = $derived(page.url.searchParams.get("settings") === "tokens");

	function closeTokenModal() {
		const u = new URL(page.url);
		u.searchParams.delete("settings");
		goto(u.pathname + u.search);
	}

	onMount(() => {
		auth.checkAuth();
	});

	// View Transitions API — feature-detected; degrades gracefully in
	// headless / Firefox / older Safari (no crash, just instant navigation).
	onNavigate((navigation) => {
		if (!document.startViewTransition) return;
		return new Promise((resolve) => {
			document.startViewTransition(async () => {
				resolve();
				await navigation.complete;
			});
		});
	});
</script>

<CommandPalette />
<Toast />

{#if showTokenModal}
	<TokenManagementModal onclose={closeTokenModal} />
{/if}

{#if page.url.pathname === "/docs" || page.url.pathname.startsWith("/docs/")}
	<!-- Docs area owns its chrome (D1): no global sidebar shell. -->
	{@render children()}
{:else}
	<SidebarProvider>
		<Sidebar />
		<SidebarInset>
			<div class="h-svh w-full overflow-y-auto p-6">
				{@render children()}
			</div>
		</SidebarInset>
	</SidebarProvider>
{/if}
