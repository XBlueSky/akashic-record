<!-- frontend/src/lib/components/GraphModuleView.svelte -->
<!-- Ported from .legacy/src/components/views/GraphModuleView.svelte -->
<!-- SP4a Unit C: real data orchestrator feeding MicroGraph engine -->
<script lang="ts">
  import { onDestroy, tick, untrack } from 'svelte';
  import * as d3 from 'd3';
  import {
    fetchModuleGraph,
    fetchModuleCallGraph,
    fetchDocGraph,
    fetchDocumentDetail,
    fetchClusterDetail,
    fetchGodNodes,
    isAuthError,
  } from '$lib/api';
  import type { GodNode } from '$lib/api';
  import type {
    ModuleNode,
    ModuleGraphData,
    ChunkCallGraph,
    DocGraphData,
    DocumentDetailData,
    DocumentSectionItem,
    SagaGroup,
    ImportEdge,
  } from '$lib/types';
  import MicroGraph from '$lib/graph/MicroGraph.svelte';
  import { t } from 'svelte-i18n';
  import { toast } from '$lib/state/toast.svelte';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';

  // ── Props ──────────────────────────────────────────────────────────────────
  interface Props {
    repoName: string;
    sourceType?: string | null;
    onselectmodule?: (module: ModuleNode) => void;
    /** moduleId + moduleName are passed together — but onselectchunk keeps the
     *  three-arg legacy signature so the page can set both ?module= and ?chunk= */
    onselectchunk?: (moduleId: string, moduleName: string, chunkId: string) => void;
    ondrilldownchange?: (moduleId: string | null, moduleName?: string) => void;
    onselectsection?: (section: DocumentSectionItem) => void;
    /** URL-driven: path of the module to auto-drill into on mount/change */
    drillToModulePath?: string;
    /** URL-driven: chunk to pre-select after drill-down */
    highlightChunkId?: string;
    onnavigaterepo?: (repoName: string) => void;
  }

  let {
    repoName,
    sourceType,
    onselectmodule,
    onselectchunk,
    ondrilldownchange,
    onselectsection,
    drillToModulePath,
    highlightChunkId,
    onnavigaterepo,
  }: Props = $props();

  let isDocMode = $derived(sourceType === 'website');
  let docDetail: DocumentDetailData | null = $state(null);

  // ?from= back-banner: decode once from the URL search params.
  let fromLabel = $derived(page.url.searchParams.get('from') ?? null);

  // Navigate back, preserving ?module= and ?chunk= but removing ?from=.
  function goBack() {
    const u = new URL(page.url.href);
    u.searchParams.delete('from');
    goto(u.pathname + u.search);
  }

  // ── State machine ──────────────────────────────────────────────────────────
  type ViewMode = 'MACRO' | 'MICRO';
  let viewMode: ViewMode = $state('MACRO');
  let drillModule: ModuleNode | null = $state(null);
  let drillData: ChunkCallGraph | null = $state(null);
  let drillLoading = $state(false);
  let drillError = $state('');

  let loading = $state(true);
  let error = $state('');
  let moduleCount = $state(0);
  let edgeCount = $state(0);
  let hoveredNode: string | null = $state(null);
  let legendOpen = $state(true);
  let isDragging = $state(false);
  let callsCount = $state(0);

  let selectedChunkId: string | null = $state(null);

  // ── DOM refs ───────────────────────────────────────────────────────────────
  let macroSvgEl = $state<SVGSVGElement | null>(null);
  let containerEl: HTMLDivElement;
  let simulation: d3.Simulation<SimNode, SimLink> | null = null;

  let hotspotIds: Set<string> = $state(new Set());
  let disconnectedCount = $state(0);
  let sagaGroups: SagaGroup[] = $state([]);
  let godNodeIds: Set<string> = $state(new Set());

  // ── Macro types ────────────────────────────────────────────────────────────
  interface SimNode extends ModuleNode {
    x: number;
    y: number;
    vx?: number;
    vy?: number;
    fx?: number | null;
    fy?: number | null;
    isRepo?: boolean;
    section_count?: number;
    explains_count?: number;
  }

  interface SimLink {
    source: SimNode | string;
    target: SimNode | string;
    curvature: number;
    edgeType: 'HAS_MODULE' | 'IMPORTS_FROM';
    confidence: number;
  }

  let nodes: SimNode[] = $state([]);
  let links: SimLink[] = $state([]);

  // ── Color palette ──────────────────────────────────────────────────────────
  const MODULE_PALETTE = [
    '#06B6D4', '#818CF8', '#34D399', '#F472B6', '#FB923C', '#A78BFA',
    '#38BDF8', '#FBBF24', '#2DD4BF', '#E879F9', '#4ADE80', '#F87171',
    '#60A5FA', '#FACC15', '#C084FC', '#22D3EE',
  ];

  let moduleColorScale: (label: string) => string = () => '#2DD4BF';

  function buildColorScale(simNodes: SimNode[]) {
    const labels = [...new Set(simNodes.filter(n => !n.isRepo).map(n => n.label))].sort();
    moduleColorScale = d3.scaleOrdinal<string>().domain(labels).range(MODULE_PALETTE);
  }

  // ── Macro node helpers ─────────────────────────────────────────────────────
  function nodeSize(node: SimNode): number {
    if (node.isRepo) return 32;
    const count = isDocMode ? (node.section_count ?? 0) : node.chunk_count;
    const raw = 7 + Math.log2(count + 1) * 3.5;
    const base = Math.min(raw, 26);
    return godNodeIds.has(node.id) ? Math.min(base * 1.3, 32) : base;
  }

  function nodeColor(node: SimNode): string {
    if (node.isRepo) return '#3B82F6';
    return isDocMode ? '#38BDF8' : moduleColorScale(node.label);
  }

  function nodeFillOpacity(node: SimNode): number {
    if (node.isRepo) return 0.08;
    return nodeSize(node) > 25 ? 0.5 : 0.8;
  }

  function nodeStrokeWidth(node: SimNode): number {
    return node.isRepo ? 3 : 1.5;
  }

  function nodeFilter(node: SimNode): string {
    if (node.isRepo) return 'url(#glow-strong)';
    if (godNodeIds.has(node.id)) return 'url(#glow-strong)';
    return 'url(#glow)';
  }

  function nodeStrokeDash(node: SimNode): string {
    return node.is_virtual ? '4 2' : 'none';
  }

  function nodeLabel(node: SimNode): string {
    if (node.isRepo) return node.label;
    const parts = node.path.split('/');
    const name = parts[parts.length - 1] || node.label;
    return name.length > 15 ? name.substring(0, 15) + '…' : name;
  }

  function showLabel(node: SimNode): boolean {
    return node.isRepo || node.note_count > 0 || godNodeIds.has(node.id) || nodeSize(node) >= 12;
  }

  function assignEdgeCurvatures(simLinks: SimLink[]) {
    const pairMap = new Map<string, SimLink[]>();
    for (const link of simLinks) {
      const sId = typeof link.source === 'string' ? link.source : (link.source as SimNode).id;
      const tId = typeof link.target === 'string' ? link.target : (link.target as SimNode).id;
      const key = [sId, tId].sort().join('::');
      if (!pairMap.has(key)) pairMap.set(key, []);
      pairMap.get(key)!.push(link);
    }
    for (const group of pairMap.values()) {
      if (group.length === 1) {
        group[0].curvature = 0;
      } else {
        group.forEach((link, i) => {
          link.curvature = ((i / (group.length - 1)) - 0.5) * 2;
        });
      }
    }
  }

  function edgePath(d: SimLink): string {
    const s = d.source as SimNode;
    const t = d.target as SimNode;
    const dx = t.x - s.x, dy = t.y - s.y;
    const len = Math.sqrt(dx * dx + dy * dy) || 1;
    const ux = dx / len, uy = dy / len;
    const rS = nodeSize(s) + 2, rT = nodeSize(t) + 2;
    const sx = s.x + ux * rS, sy = s.y + uy * rS;
    const tx = t.x - ux * rT, ty = t.y - uy * rT;
    if (!d.curvature) return `M${sx},${sy}L${tx},${ty}`;
    const mx = (sx + tx) / 2, my = (sy + ty) / 2;
    const arcOffset = d.curvature * Math.min(len * 0.25, 60);
    return `M${sx},${sy}Q${mx + (-uy) * arcOffset},${my + ux * arcOffset},${tx},${ty}`;
  }

  function edgeColor(d: SimLink): string {
    const s = d.source as SimNode;
    return s.isRepo ? '#64748B' : moduleColorScale(s.label);
  }

  // ── Saga convex hull helpers ───────────────────────────────────────────────
  const SAGA_HULL_COLORS = ['#F59E0B', '#3B82F6', '#10B981', '#EC4899', '#8B5CF6', '#EF4444'];

  function sagaHullColor(idx: number): string {
    return SAGA_HULL_COLORS[idx % SAGA_HULL_COLORS.length];
  }

  function computeSagaHull(group: SagaGroup): [number, number][] | null {
    const memberNodes = nodes.filter(n => group.module_ids.includes(n.id));
    if (memberNodes.length < 2) return null;
    const points: [number, number][] = memberNodes.map(n => [n.x, n.y]);
    if (memberNodes.length === 2) {
      const [a, b] = points;
      const dx = b[0] - a[0], dy = b[1] - a[1];
      const len = Math.sqrt(dx * dx + dy * dy) || 1;
      const ux = dx / len, uy = dy / len;
      const pad = 30;
      return [
        [a[0] - ux * pad + uy * pad, a[1] - uy * pad - ux * pad],
        [a[0] - ux * pad - uy * pad, a[1] - uy * pad + ux * pad],
        [b[0] + ux * pad - uy * pad, b[1] + uy * pad + ux * pad],
        [b[0] + ux * pad + uy * pad, b[1] + uy * pad - ux * pad],
      ];
    }
    const hull = d3.polygonHull(points);
    if (!hull) return null;
    const cx = d3.mean(hull, p => p[0]) ?? 0;
    const cy = d3.mean(hull, p => p[1]) ?? 0;
    const pad = 25;
    return hull.map(([x, y]) => {
      const dx = x - cx, dy = y - cy;
      const dist = Math.sqrt(dx * dx + dy * dy) || 1;
      return [x + (dx / dist) * pad, y + (dy / dist) * pad] as [number, number];
    });
  }

  function applyHoverOpacity() {
    if (!macroSvgEl || viewMode !== 'MACRO') return;
    const svg = d3.select(macroSvgEl);
    svg.selectAll<SVGGElement, SimNode>('g.node-group')
      .transition().duration(250)
      .attr('opacity', d => {
        if (!hoveredNode) return 1;
        if (d.id === hoveredNode) return 1;
        return links.some(l =>
          ((l.source as SimNode).id === hoveredNode && (l.target as SimNode).id === d.id) ||
          ((l.target as SimNode).id === hoveredNode && (l.source as SimNode).id === d.id)
        ) ? 1 : 0.15;
      });
    svg.selectAll<SVGPathElement, SimLink>('.edges path')
      .data(links)
      .transition().duration(250)
      .attr('opacity', d => !hoveredNode ? (0.3 + (d?.confidence ?? 0.8) * 0.35) :
        ((d?.source as SimNode)?.id === hoveredNode || (d?.target as SimNode)?.id === hoveredNode) ? 1 : 0.06)
      .attr('stroke-width', d => !hoveredNode ? 1.8 :
        ((d?.source as SimNode)?.id === hoveredNode || (d?.target as SimNode)?.id === hoveredNode) ? 3 : 1.8);
  }

  // ── Drill-down (Strategy B: Dual Canvas Swap) ──────────────────────────────
  async function enterDrilldown(node: SimNode) {
    if (viewMode !== 'MACRO' || node.isRepo) return;
    drillModule = node;
    drillLoading = true;
    drillError = '';
    selectedChunkId = null;
    ondrilldownchange?.(node.id, node.label);

    simulation?.stop();

    if (isDocMode) {
      try {
        const isCluster = node.id.startsWith('cluster:');
        const entityId = node.id.replace(/^(cluster:|doc:)/, '');
        const detail = isCluster
          ? await fetchClusterDetail(entityId)
          : await fetchDocumentDetail(entityId);
        docDetail = detail;
        drillData = null;
        drillLoading = false;
        viewMode = 'MICRO';
      } catch (e: unknown) {
        if (!isAuthError(e)) {
          drillError = e instanceof Error ? e.message : 'Failed to load document sections';
        }
        drillModule = null;
        drillLoading = false;
        ondrilldownchange?.(null);
        simulation?.alpha(0.1).restart();
        return;
      }
    } else {
      try {
        const moduleId = node.id.startsWith('mod:') ? node.id.slice(4) : node.id;
        drillData = await fetchModuleCallGraph(moduleId);
      } catch (e: unknown) {
        if (!isAuthError(e)) {
          drillError = e instanceof Error ? e.message : 'Failed to load call graph';
        }
        drillModule = null;
        drillLoading = false;
        ondrilldownchange?.(null);
        simulation?.alpha(0.1).restart();
        return;
      }
      drillLoading = false;

      const degreeMap = new Map<string, { in: number; out: number }>();
      for (const chunk of drillData.chunks) {
        degreeMap.set(chunk.id, { in: 0, out: 0 });
      }
      for (const call of drillData.calls) {
        const s = degreeMap.get(call.source);
        const t = degreeMap.get(call.target);
        if (s) s.out++;
        if (t) t.in++;
      }

      const connectedChunks = drillData.chunks.filter(c => {
        const d = degreeMap.get(c.id);
        return d && (d.in > 0 || d.out > 0);
      });
      disconnectedCount = drillData.chunks.length - connectedChunks.length;

      const ranked = connectedChunks
        .map(c => ({ id: c.id, inDeg: degreeMap.get(c.id)?.in || 0 }))
        .sort((a, b) => b.inDeg - a.inDeg);
      hotspotIds = new Set(ranked.slice(0, 3).map(c => c.id));
    }

    viewMode = 'MICRO';

    if (highlightChunkId && drillData) {
      const chunkExists = drillData.chunks.some(c => c.id === highlightChunkId);
      if (chunkExists) {
        tick().then(() => { selectedChunkId = highlightChunkId; });
      }
    }
  }

  function exitDrilldown(callback?: () => void) {
    if (viewMode !== 'MICRO') return;
    selectedChunkId = null;
    drillData = null;
    docDetail = null;
    drillModule = null;
    drillError = '';
    viewMode = 'MACRO';
    ondrilldownchange?.(null);

    tick().then(() => {
      if (nodes.length > 0) initSimulation();
      if (callback) setTimeout(callback, 100);
    });
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && viewMode === 'MICRO') exitDrilldown();
  }

  // ── Macro graph loading ────────────────────────────────────────────────────
  // Request-sequencing guard: each loadGraph() call captures a monotonically
  // increasing token. After any await we bail if a newer load has started, so
  // a slow in-flight fetch for a previous repo cannot resolve last and
  // overwrite nodes/links (and start a simulation) with stale data.
  let loadGeneration = 0;

  async function loadGraph(repo: string) {
    const myGeneration = ++loadGeneration;
    cleanup();
    loading = true;
    error = '';
    hoveredNode = null;

    try {
      if (isDocMode) {
        const data: DocGraphData = await fetchDocGraph(repo);
        if (myGeneration !== loadGeneration) return;
        moduleCount = data.nodes.length;
        edgeCount = data.edges.length;
        callsCount = 0;

        const simNodes: SimNode[] = data.nodes.map(n => ({
          ...n,
          chunk_count: n.section_count,
          note_count: 0,
          section_count: n.section_count,
          explains_count: n.explains_count,
          x: (Math.random() - 0.5) * 800,
          y: (Math.random() - 0.5) * 800,
          isRepo: n.language === 'repository',
        }));

        const repoNodeId = simNodes.find(n => n.isRepo)?.id;
        const simLinks: SimLink[] = data.edges.map((e: ImportEdge) => ({
          source: e.source,
          target: e.target,
          curvature: 0,
          edgeType: (e.source === repoNodeId ? 'HAS_MODULE' : 'IMPORTS_FROM') as SimLink['edgeType'],
          confidence: e.source === repoNodeId ? 1.0 : ((e as unknown as Record<string, unknown>)['confidence'] as number ?? 0.8),
        }));
        assignEdgeCurvatures(simLinks);

        nodes = simNodes;
        links = simLinks;
        buildColorScale(simNodes);
      } else {
        const data: ModuleGraphData = await fetchModuleGraph(repo);
        if (myGeneration !== loadGeneration) return;
        moduleCount = data.nodes.length;
        edgeCount = data.edges.length;
        callsCount = data.calls_count ?? 0;

        const simNodes: SimNode[] = data.nodes.map(n => ({
          ...n,
          x: (Math.random() - 0.5) * 800,
          y: (Math.random() - 0.5) * 800,
          isRepo: !n.path,
        }));

        const repoNodeId = simNodes.find(n => n.isRepo)?.id;
        const simLinks: SimLink[] = data.edges.map(e => ({
          source: e.source,
          target: e.target,
          curvature: 0,
          edgeType: (e.source === repoNodeId ? 'HAS_MODULE' : 'IMPORTS_FROM') as SimLink['edgeType'],
          confidence: e.source === repoNodeId ? 1.0 : ((e as unknown as Record<string, unknown>)['confidence'] as number ?? 0.8),
        }));
        assignEdgeCurvatures(simLinks);

        nodes = simNodes;
        links = simLinks;
        sagaGroups = data.saga_groups ?? [];
        buildColorScale(simNodes);
      }
    } catch (e: unknown) {
      if (myGeneration !== loadGeneration) return;
      if (!isAuthError(e)) error = e instanceof Error ? e.message : 'Failed to load graph';
    } finally {
      if (myGeneration === loadGeneration) {
        loading = false;
        await tick();
        if (myGeneration === loadGeneration && !error && nodes.length > 0) initSimulation();
        fetchGodNodes(repo).then(res => {
          if (myGeneration !== loadGeneration) return;
          godNodeIds = new Set(res.god_nodes.filter((g: GodNode) => g.node_type === 'module').map((g: GodNode) => g.id));
        }).catch(() => {});
      }
    }
  }

  function initSimulation() {
    if (!containerEl || !macroSvgEl) return;
    const width = containerEl.clientWidth;
    const height = containerEl.clientHeight;

    const repoNode = nodes.find(n => n.isRepo);
    if (repoNode) { repoNode.fx = width / 2; repoNode.fy = height / 2; }
    const radius = Math.min(width, height) * 0.35;

    simulation = d3.forceSimulation<SimNode>(nodes)
      .force('link', d3.forceLink<SimNode, SimLink>(links).id((d) => d.id)
        .distance((d) => d.edgeType === 'HAS_MODULE' ? 160 : 280)
        .strength((d) => d.edgeType === 'HAS_MODULE' ? 0.7 : 0.12))
      .force('charge', d3.forceManyBody().strength(-400))
      .force('collide', d3.forceCollide<SimNode>().radius((d) => nodeSize(d) + 15))
      .force('radial', d3.forceRadial<SimNode>(radius, width / 2, height / 2).strength(0.06))
      .alphaMin(0.005)
      .on('tick', () => {
        const svg = d3.select(macroSvgEl!);
        svg.selectAll<SVGGElement, SimNode>('g.node-group')
          .data(nodes)
          .attr('transform', d => `translate(${d.x},${d.y})`);
        svg.selectAll<SVGPathElement, SimLink>('.edges path')
          .data(links)
          .attr('d', edgePath);
        const hullByGroup = new Map<SagaGroup, [number, number][] | null>(
          sagaGroups.map(g => [g, computeSagaHull(g)])
        );
        svg.selectAll<SVGPolygonElement, SagaGroup>('.saga-hulls polygon')
          .data(sagaGroups)
          .attr('points', g => {
            const hull = hullByGroup.get(g);
            return hull ? hull.map(p => p.join(',')).join(' ') : '';
          });
        svg.selectAll<SVGTextElement, SagaGroup>('.saga-hulls text')
          .data(sagaGroups)
          .attr('x', g => {
            const hull = hullByGroup.get(g);
            return hull ? (d3.mean(hull, p => p[0]) ?? 0) : 0;
          })
          .attr('y', g => {
            const hull = hullByGroup.get(g);
            return hull ? (d3.mean(hull, p => p[1]) ?? 0) : 0;
          });
      });

    const svg = d3.select(macroSvgEl);
    const g = svg.select('g.graph-root');
    svg.call(
      d3.zoom<SVGSVGElement, unknown>()
        .scaleExtent([0.3, 5])
        .on('zoom', (event) => { g.attr('transform', event.transform); })
    );
    setupDrag();
  }

  function setupDrag() {
    if (!macroSvgEl || !simulation) return;
    d3.select(macroSvgEl)
      .selectAll<SVGGElement, SimNode>('g.node-group')
      .data(nodes)
      .call(
        d3.drag<SVGGElement, SimNode>()
          .on('start', (event, d) => {
            isDragging = true;
            if (!event.active) simulation!.alphaTarget(0.3).restart();
            d.fx = d.x; d.fy = d.y;
          })
          .on('drag', (event, d) => { d.fx = event.x; d.fy = event.y; })
          .on('end', (event, d) => {
            isDragging = false;
            if (!event.active) simulation!.alphaTarget(0);
            if (!d.isRepo) { d.fx = null; d.fy = null; }
          })
      );
  }

  $effect(() => { hoveredNode; applyHoverOpacity(); });

  function cleanup() {
    if (hoverDebounceTimer) clearTimeout(hoverDebounceTimer);
    if (simulation) { simulation.stop(); simulation = null; }
    nodes = [];
    links = [];
    hotspotIds = new Set();
    disconnectedCount = 0;
    void disconnectedCount;
    viewMode = 'MACRO';
    drillModule = null;
    drillData = null;
    docDetail = null;
  }

  // Click/dblclick disambiguation via timing
  let clickTimer: ReturnType<typeof setTimeout> | null = null;
  let lastClickTime = 0;
  let lastClickNodeId = '';

  function handleNodeClick(node: SimNode) {
    if (node.isRepo) return;
    const now = Date.now();
    const isDoubleClick = (now - lastClickTime < 400) && (lastClickNodeId === node.id);
    lastClickTime = now;
    lastClickNodeId = node.id;

    if (isDoubleClick) {
      if (clickTimer) { clearTimeout(clickTimer); clickTimer = null; }
      enterDrilldown(node);
      return;
    }

    if (clickTimer) clearTimeout(clickTimer);
    clickTimer = setTimeout(() => {
      clickTimer = null;
      if (viewMode !== 'MACRO') return;
      if (!isDocMode) onselectmodule?.(node);
    }, 400);
  }

  function handleNodeDblClick(_node: SimNode) {
    // Handled in handleNodeClick via timing
  }

  let hoverDebounceTimer: ReturnType<typeof setTimeout> | null = null;

  function handleNodeHover(nodeId: string | null) {
    if (isDragging) return;
    if (hoverDebounceTimer) clearTimeout(hoverDebounceTimer);
    hoverDebounceTimer = setTimeout(() => { hoveredNode = nodeId; }, 30);
  }

  $effect(() => { if (repoName) loadGraph(repoName); });

  // Auto-drilldown when drillToModulePath prop provides a target module
  $effect(() => {
    const path = drillToModulePath;
    if (!path || viewMode !== 'MACRO' || nodes.length === 0) return;

    untrack(() => {
      const targetNode = nodes.find(n => n.path === path && !n.isRepo);
      if (targetNode) {
        enterDrilldown(targetNode);
      } else {
        toast.push(`Module not found: ${path}`, 'error');
      }
    });
  });

  onDestroy(cleanup);
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="relative w-full h-full overflow-hidden" bind:this={containerEl} onkeydown={handleKeydown}>
  {#if loading}
    <div class="flex flex-col items-center justify-center h-full gap-2">
      <p class="font-mono text-lg font-semibold text-muted-foreground tracking-widest">{$t('graph.loading')}</p>
      <p class="text-xs text-muted-foreground font-mono">{repoName}</p>
    </div>
  {:else if error}
    <div class="flex flex-col items-center justify-center h-full gap-2">
      <p class="font-mono text-lg font-semibold text-red-500 tracking-widest">{$t('graph.error')}</p>
      <p class="text-xs text-muted-foreground font-mono">{error}</p>
    </div>
  {:else if viewMode === 'MACRO'}
    <!-- ═══ MACRO SVG (module constellation) ═══ -->
    <svg bind:this={macroSvgEl} class="w-full h-full" style="background: transparent;">
      <defs>
        <filter id="glow" x="-50%" y="-50%" width="200%" height="200%">
          <feGaussianBlur stdDeviation="3" result="blur" />
          <feMerge><feMergeNode in="blur" /><feMergeNode in="SourceGraphic" /></feMerge>
        </filter>
        <filter id="glow-strong" x="-50%" y="-50%" width="200%" height="200%">
          <feGaussianBlur stdDeviation="6" result="blur" />
          <feGaussianBlur stdDeviation="12" in="SourceGraphic" result="bloom" />
          <feMerge><feMergeNode in="bloom" /><feMergeNode in="blur" /><feMergeNode in="SourceGraphic" /></feMerge>
        </filter>
      </defs>
      <g class="graph-root">
        <!-- Saga convex hulls (behind everything) -->
        <g class="saga-hulls">
          {#each sagaGroups as group, idx}
            {@const hull = computeSagaHull(group)}
            {#if hull}
              <polygon
                points={hull.map(p => p.join(',')).join(' ')}
                fill={sagaHullColor(idx)}
                fill-opacity="0.06"
                stroke={sagaHullColor(idx)}
                stroke-opacity="0.2"
                stroke-width="1"
                stroke-dasharray="4 3"
              />
              {@const cx = d3.mean(hull, p => p[0]) ?? 0}
              {@const cy = d3.mean(hull, p => p[1]) ?? 0}
              <text x={cx} y={cy} text-anchor="middle" dominant-baseline="central"
                fill={sagaHullColor(idx)} font-family="monospace" font-size="9"
                opacity="0.4" pointer-events="none">
                {group.name}
              </text>
            {/if}
          {/each}
        </g>
        <g class="edges">
          {#each links as link}
            <path stroke={edgeColor(link)} stroke-width="1.8" fill="none"
              opacity={0.3 + link.confidence * 0.35}
              stroke-dasharray={link.confidence >= 0.9 ? 'none' : link.confidence >= 0.5 ? '6 4' : '2 3'}
              stroke-linecap="round" />
          {/each}
        </g>
        <g class="nodes">
          {#each nodes as node (node.id)}
            <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
            <g class="node-group outline-none" transform="translate({node.x},{node.y})" role="button" tabindex="0"
              onclick={() => handleNodeClick(node)} ondblclick={() => handleNodeDblClick(node)}
              onkeydown={(e) => e.key === 'Enter' && handleNodeClick(node)}
              onmouseenter={() => handleNodeHover(node.id)} onmouseleave={() => handleNodeHover(null)}
              style="cursor: {node.isRepo ? 'default' : 'pointer'};">
              <circle r={nodeSize(node) + 8} fill="transparent" stroke="none" pointer-events="all" />
              <circle r={nodeSize(node)} fill={nodeColor(node)} fill-opacity={nodeFillOpacity(node)}
                stroke={nodeColor(node)} stroke-width={nodeStrokeWidth(node)}
                stroke-dasharray={nodeStrokeDash(node)} filter={nodeFilter(node)} />
              {#if node.note_count > 0 && !node.isRepo}
                <circle cx={nodeSize(node) * 0.7} cy={-nodeSize(node) * 0.7} r="4" fill="#F59E0B" class="animate-pulse" pointer-events="none" />
              {/if}
              {#if showLabel(node)}
                <text y={nodeSize(node) + 14} text-anchor="middle" fill="#94A3B8" font-family="monospace" font-size="11" opacity="0.85" pointer-events="none">
                  {nodeLabel(node)}
                </text>
              {/if}
              <title>{node.isRepo ? node.label : isDocMode ? `${nodeLabel(node)} — ${node.section_count ?? 0} sections, ${node.explains_count ?? 0} explains\nDouble-click to drill down` : `${nodeLabel(node)} — ${node.chunk_count} chunks, ${node.note_count} notes\nDouble-click to drill down`}</title>
            </g>
          {/each}
        </g>
      </g>
    </svg>

    <!-- Legend overlay -->
    <div class="absolute top-4 left-4 z-2">
      {#if legendOpen}
        <div class="glass rounded-lg p-3 min-w-[130px]">
          <button class="w-full flex items-center justify-between mb-1.5 cursor-pointer bg-transparent border-none" onclick={() => legendOpen = false}>
            <span class="font-mono text-[10px] font-semibold tracking-widest uppercase text-muted-foreground">Legend</span>
            <span class="text-[10px] text-muted-foreground">x</span>
          </button>
          <div class="flex items-center gap-2 mb-1">
            <span class="w-2.5 h-2.5 rounded-full shrink-0" style="background: rgba(59,130,246,0.1); border: 2px solid #3B82F6; box-shadow: 0 0 6px #3B82F6;"></span>
            <span class="text-[10px] text-muted-foreground font-mono">Repository</span>
          </div>
          <div class="flex items-center gap-2 mb-1">
            <span class="w-2 h-2 rounded-full shrink-0" style="background: {isDocMode ? '#38BDF8' : '#06B6D4'}; box-shadow: 0 0 6px {isDocMode ? '#38BDF8' : '#06B6D4'};"></span>
            <span class="text-[10px] text-muted-foreground font-mono">{isDocMode ? 'Document' : 'Module'}</span>
          </div>
          <div class="flex items-center gap-2 mb-1">
            <span class="w-3.5 border-t border-dashed shrink-0" style="border-color: #2DD4BF;"></span>
            <span class="text-[10px] text-muted-foreground font-mono">Virtual</span>
          </div>
          <div class="flex items-center gap-2 mb-1">
            <span class="w-2 h-2 rounded-full shrink-0" style="background: #F59E0B; box-shadow: 0 0 6px #F59E0B;"></span>
            <span class="text-[10px] text-muted-foreground font-mono">Has Notes</span>
          </div>
          <div class="h-px bg-border my-1.5"></div>
          <div class="flex items-center gap-2">
            <svg class="w-4 h-3 shrink-0" viewBox="0 0 20 14">
              <path d="M2,12 Q10,0 18,12" stroke="#06B6D4" stroke-width="2" fill="none" opacity="0.6" stroke-linecap="round" />
            </svg>
            <span class="text-[10px] text-muted-foreground font-mono">{$t('graph.connections')}</span>
          </div>
        </div>
      {:else}
        <button class="glass rounded-lg px-2.5 py-1.5 cursor-pointer bg-transparent border border-white/5 hover:border-white/10 transition-colors"
          onclick={() => legendOpen = true} title="Show legend">
          <span class="font-mono text-[10px] text-muted-foreground tracking-wider">LEGEND</span>
        </button>
      {/if}
    </div>

    <!-- Macro stats badge -->
    <div class="absolute bottom-4 left-4 z-2 glass rounded-lg px-3.5 py-2 text-xs text-muted-foreground font-mono">
      {#if isDocMode}
        <span>{moduleCount} documents</span>
        <span class="mx-1.5">|</span>
        <span>{edgeCount} links</span>
      {:else}
        <span>{$t('graph.modules', { values: { count: moduleCount } })}</span>
        <span class="mx-1.5">|</span>
        <span>{$t('graph.edges', { values: { count: edgeCount } })}</span>
        {#if callsCount > 0}
          <span class="mx-1.5">|</span>
          <span>{$t('graph.calls', { values: { count: callsCount } })}</span>
        {/if}
      {/if}
    </div>

  {:else if viewMode === 'MICRO' && isDocMode && docDetail}
    <!-- ═══ MICRO: section tree ═══ -->
    <MicroGraph
      chunks={[]}
      calls={[]}
      external={[]}
      moduleName={drillModule?.label ?? ''}
      selectedChunkId={null}
      sectionMode={true}
      sections={docDetail.sections}
      documentTitle={docDetail.document.title}
      onselectsection={(section) => {
        onselectsection?.(section);
      }}
    />
  {:else if viewMode === 'MICRO'}
    <!-- ═══ MICRO: chunk call graph ═══ -->
    <MicroGraph
      chunks={drillData?.chunks ?? []}
      calls={drillData?.calls ?? []}
      external={drillData?.external ?? []}
      moduleName={drillModule?.label ?? ''}
      selectedChunkId={selectedChunkId}
      onselectchunk={(chunkId) => {
        selectedChunkId = chunkId || null;
        if (chunkId && drillModule) onselectchunk?.(drillModule.id, drillModule.label, chunkId);
      }}
      onghostclick={(moduleId) => {
        exitDrilldown(() => {
          const targetNode = nodes.find(n => n.id === moduleId);
          if (targetNode) enterDrilldown(targetNode);
        });
      }}
    />

    <!-- Back breadcrumb -->
    {#if drillModule}
      <button class="absolute top-4 right-4 z-10 glass rounded-lg px-3 py-1.5 cursor-pointer border border-white/10 hover:border-blue-500/30 hover:bg-blue-500/8 active:bg-blue-500/12 transition-all"
        onclick={() => exitDrilldown()}>
        <span class="font-mono text-xs font-semibold text-blue-400 tracking-wide">&larr; {$t('graph.backToModules')}</span>
      </button>
    {/if}
  {/if}

  <!-- Loading overlay for drill-down -->
  {#if drillLoading}
    <div class="absolute inset-0 flex items-center justify-center">
      <p class="font-mono text-sm text-muted-foreground tracking-widest">{$t('graph.drillLoading')}</p>
    </div>
  {/if}

  <!-- ?from= back-banner: shown when arriving via an explains cross-repo link -->
  {#if fromLabel}
    <button
      class="absolute top-4 left-1/2 -translate-x-1/2 z-20 glass rounded-lg px-3 py-1.5 cursor-pointer border border-amber-500/20 hover:border-amber-500/40 hover:bg-amber-500/8 active:bg-amber-500/12 transition-all"
      onclick={goBack}
      title="Return to previous view"
    >
      <span class="font-mono text-xs font-semibold text-amber-400 tracking-wide">&larr; {fromLabel}</span>
    </button>
  {/if}
</div>

<style>
  .node-group:focus,
  .node-group:focus-visible {
    outline: none;
  }
</style>
