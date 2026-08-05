<script lang="ts">
	import { page } from "$app/state";
	import ModulesView from "$lib/components/ModulesView.svelte";
</script>

<ModulesView repoName={page.params.repo ?? ""} />
