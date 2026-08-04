<script lang="ts">
  import { page } from '$app/state';
  import SagaTimelineView from '$lib/components/SagaTimelineView.svelte';
</script>

<SagaTimelineView
  repoName={page.params.repo!}
  sagaId={page.params.id!}
/>
