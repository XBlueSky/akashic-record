<script lang="ts">
	import { goto } from "$app/navigation";
	import Landing from "$lib/landing/Landing.svelte";
</script>

<Landing onselect={(name) => goto("/r/" + encodeURIComponent(name))} />
