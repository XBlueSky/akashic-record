<script lang="ts">
  // SP4a Unit C: graph page wires URL params ↔ GraphModuleView + ChunkSidebar
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import GraphModuleView from '$lib/components/GraphModuleView.svelte';
  import type { ModuleNode, DocumentSectionItem } from '$lib/types';

  // Layout load data (+layout.ts) carries the repo detail, including
  // source_type — the authoritative code-vs-doc-mode signal.
  let { data } = $props();

  // ── URL-derived props ────────────────────────────────────────────────────
  // sourceType: from the repo's source_type (via layout load data), so a
  // website repo correctly renders the doc section-tree (not a call graph).
  // Falls back to 'gitlab' (code mode) when detail is unavailable (backend down).
  // GraphModuleView branches code/doc behavior on sourceType === 'website'.
  let sourceType = $derived(data.detail?.source_type ?? 'gitlab');

  // Auto-drill: ?module= set by ondrilldownchange
  let drillToModulePath = $derived(page.url.searchParams.get('module') ?? undefined);
  // Pre-select chunk inside drill: ?chunk= set by onselectchunk
  let highlightChunkId = $derived(page.url.searchParams.get('chunk') ?? undefined);

  // ── URL param helpers ────────────────────────────────────────────────────
  /** Build a new URL preserving existing non-graph params, then merging overrides. */
  function buildUrl(overrides: Record<string, string | null>): string {
    const u = new URL(page.url.href);
    for (const [k, v] of Object.entries(overrides)) {
      if (v === null || v === '') {
        u.searchParams.delete(k);
      } else {
        u.searchParams.set(k, v);
      }
    }
    return u.pathname + u.search;
  }

  // ── Callbacks ────────────────────────────────────────────────────────────

  /**
   * User selected a chunk inside a drilled-down module.
   * Sets ?module=<moduleId>&modname=<moduleName>&chunk=<chunkId>
   * so the layout's ChunkSidebar panel opens.
   */
  function handleSelectChunk(moduleId: string, moduleName: string, chunkId: string) {
    goto(buildUrl({
      module: moduleId,
      modname: moduleName,
      chunk: chunkId,
      smode: null,
      doc: null,
    }), { replaceState: true });
  }

  /**
   * Drill-down entered or exited.
   * Sets ?module=<moduleId>&modname=<moduleName> on entry, clears on exit.
   */
  function handleDrilldownChange(moduleId: string | null, moduleName?: string) {
    if (moduleId) {
      goto(buildUrl({
        module: moduleId,
        modname: moduleName ?? moduleId,
        // Clear chunk + sidebar params — entering a new module
        chunk: null,
        smode: null,
        doc: null,
      }), { replaceState: true });
    } else {
      goto(buildUrl({
        module: null,
        modname: null,
        chunk: null,
        smode: null,
        doc: null,
      }), { replaceState: true });
    }
  }

  /**
   * User single-clicked a module node (code mode — opens module detail
   * sidebar when implemented in SP4d; for now just sets ?module=).
   */
  function handleSelectModule(module: ModuleNode) {
    goto(buildUrl({
      module: module.id,
      modname: module.label,
      chunk: null,
    }), { replaceState: true });
  }

  /**
   * User selected a document section (doc mode).
   * Sets ?smode=1&module=<section.id>&doc=<documentTitle> so the sidebar
   * opens in section mode.
   */
  function handleSelectSection(section: DocumentSectionItem) {
    goto(buildUrl({
      smode: '1',
      module: section.id,
      modname: section.heading,
      doc: section.heading,
      chunk: null,
    }), { replaceState: true });
  }

  /**
   * Cross-repo navigation (ghost-node / explains links).
   * Navigates to the other repo's graph page.
   */
  function handleNavigateRepo(repoName: string) {
    goto('/r/' + encodeURIComponent(repoName) + '/graph');
  }
</script>

<div class="h-full w-full">
  <GraphModuleView
    repoName={page.params.repo ?? ''}
    sourceType={sourceType}
    drillToModulePath={drillToModulePath}
    highlightChunkId={highlightChunkId}
    onselectchunk={handleSelectChunk}
    ondrilldownchange={handleDrilldownChange}
    onselectmodule={handleSelectModule}
    onselectsection={handleSelectSection}
    onnavigaterepo={handleNavigateRepo}
  />
</div>
