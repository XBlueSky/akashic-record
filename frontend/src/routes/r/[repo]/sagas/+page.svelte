<script lang="ts">
	import { page } from "$app/state";
	import SagaListView from "$lib/components/SagaListView.svelte";
</script>

<SagaListView
	repoName={page.params.repo!}
	status={page.url.searchParams.get("status") ?? undefined}
/>
