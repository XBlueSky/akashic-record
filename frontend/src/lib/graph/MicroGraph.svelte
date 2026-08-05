<!-- frontend/src/lib/graph/MicroGraph.svelte -->
<script lang="ts">
	import { onDestroy, untrack } from "svelte";
	import * as d3 from "d3";
	import type { ChunkNode, CallEdge, GhostNode, DocumentSectionItem } from "$lib/types";
	import type { MicroNode, MicroLink } from "./types";
	import {
		isClassType,
		isEnumType,
		getChunkColors,
		microNodeLabel,
		pillWidth,
		PILL_HEIGHT,
		CLASS_CARD_HEIGHT,
		GHOST_PILL_WIDTH,
		GHOST_PILL_HEIGHT,
		getSectionColors,
	} from "./types";
	import {
		buildCallGraphLayout,
		createForceSimulation,
		edgeBezierPath,
		type CallGraphLayout,
		buildSectionTreeLayout,
		createSectionSimulation,
		type SectionLayoutNode,
		type SectionLayoutEdge,
		type SectionTreeLayout,
	} from "./layout";
	import { computeFitTransform, type FitRect } from "./geometry";

	interface Props {
		chunks: ChunkNode[];
		calls: CallEdge[];
		external: GhostNode[];
		moduleName: string;
		selectedChunkId: string | null;
		onselectchunk?: (chunkId: string) => void;
		onghostclick?: (ghostModuleId: string) => void;
		sectionMode?: boolean;
		sections?: DocumentSectionItem[];
		documentTitle?: string;
		onselectsection?: (section: DocumentSectionItem) => void;
	}

	let {
		chunks,
		calls,
		external,
		moduleName,
		selectedChunkId,
		onselectchunk,
		onghostclick,
		sectionMode = false,
		sections = [],
		documentTitle = "",
		onselectsection,
	}: Props = $props();

	// ── DOM refs ──
	let containerEl: HTMLDivElement | undefined = $state();
	let svgEl: SVGSVGElement | undefined = $state();

	// ── Layout state ──
	let layoutResult: CallGraphLayout | null = $state(null);
	let nodePositions: Map<string, { x: number; y: number }> = $state(new Map());

	// ── Derived graph data ──
	let graphNodes: MicroNode[] = $derived.by(() => {
		if (!layoutResult) return [];
		// Z-order: ghost (bottom) → function → class → enum → hotspot (top)
		const zOrder = (n: MicroNode): number => {
			if (n.isGhost) return 0;
			if (layoutResult!.hotspotIds.has(n.id)) return 4;
			if (isEnumType(n.chunk_type)) return 3;
			if (isClassType(n.chunk_type)) return 2;
			return 1; // function/method
		};
		return [...layoutResult.nodes].sort((a, b) => zOrder(a) - zOrder(b));
	});
	let graphLinks: MicroLink[] = $derived.by(() => (layoutResult ? layoutResult.links : []));
	let hotspotIds: Set<string> = $derived.by(() =>
		layoutResult ? layoutResult.hotspotIds : new Set(),
	);
	// id → node index so the per-edge template helpers resolve endpoints in O(1)
	// instead of an O(N) Array.find() per edge per render (mirrors the Map the
	// tick handler already builds).
	let nodeById: Map<string, MicroNode> = $derived(new Map(graphNodes.map((n) => [n.id, n])));

	// ── Force simulation ──
	let simulation: d3.Simulation<MicroNode, MicroLink> | null = null;

	// ── Selection state ──
	// eslint-disable-next-line @typescript-eslint/no-unused-vars -- computed by computeSelection() on every selection change but not yet consulted for node/edge styling (pre-existing gap, out of scope for this lint pass)
	let upstreamIds: Set<string> = $state(new Set());
	// eslint-disable-next-line @typescript-eslint/no-unused-vars -- see upstreamIds above
	let downstreamIds: Set<string> = $state(new Set());

	// ── Hover state (debounced to prevent flicker) ──
	let hoveredNodeId: string | null = $state(null);
	let hoverTimer: ReturnType<typeof setTimeout> | null = null;
	function setHoveredNode(id: string | null) {
		if (hoverTimer) clearTimeout(hoverTimer);
		hoverTimer = setTimeout(() => {
			hoveredNodeId = id;
		}, 30);
	}
	let hoveredConnectedIds: Set<string> = $derived.by(() => {
		if (!hoveredNodeId) return new Set<string>();
		const connected = new Set<string>();
		connected.add(hoveredNodeId);
		for (const link of graphLinks) {
			const sid = linkSourceId(link),
				tid = linkTargetId(link);
			if (sid === hoveredNodeId) connected.add(tid);
			if (tid === hoveredNodeId) connected.add(sid);
		}
		return connected;
	});

	// ── Zoom transform ──
	let zoomTransform: d3.ZoomTransform = $state(d3.zoomIdentity);
	let zoomBehavior: d3.ZoomBehavior<SVGSVGElement, unknown> | null = null;

	// ── Section tree state ──
	let sectionLayout: SectionTreeLayout | null = $state(null);
	let sectionPositions: Map<string, { x: number; y: number }> = $state(new Map());
	let sectionSim: d3.Simulation<SectionLayoutNode, SectionLayoutEdge> | null = null;

	let sectionNodes: SectionLayoutNode[] = $derived.by(() =>
		sectionLayout ? sectionLayout.nodes : [],
	);
	let sectionEdges: SectionLayoutEdge[] = $derived.by(() =>
		sectionLayout ? sectionLayout.edges : [],
	);
	// id → section-node index (O(1) endpoint lookup per edge; see nodeById).
	let sectionNodeById: Map<string, SectionLayoutNode> = $derived(
		new Map(sectionNodes.map((n) => [n.id, n])),
	);

	// ── Section node geometry helpers ──
	function sectionPillWidth(node: SectionLayoutNode): number {
		return pillWidth(node.heading);
	}

	function getSectionNodeX(node: SectionLayoutNode): number {
		return sectionPositions.get(node.id)?.x ?? node.x;
	}
	function getSectionNodeY(node: SectionLayoutNode): number {
		return sectionPositions.get(node.id)?.y ?? node.y;
	}

	function sectionNodeColors(node: SectionLayoutNode) {
		return getSectionColors(node.depth, (node.section.explains?.length ?? 0) > 0);
	}

	// ── Section edge ID helpers (d3-force mutates source/target) ──
	function secEdgeSourceId(edge: SectionLayoutEdge): string {
		return typeof edge.source === "string" ? edge.source : edge.source.id;
	}
	function secEdgeTargetId(edge: SectionLayoutEdge): string {
		return typeof edge.target === "string" ? edge.target : edge.target.id;
	}

	// ── Section edge helpers ──
	function computeSectionEdgePath(edge: SectionLayoutEdge): string {
		const sid = secEdgeSourceId(edge),
			tid = secEdgeTargetId(edge);
		const source = sectionNodeById.get(sid);
		const target = sectionNodeById.get(tid);
		if (!source || !target) return "";
		const sx = getSectionNodeX(source),
			sy = getSectionNodeY(source);
		const tx = getSectionNodeX(target),
			ty = getSectionNodeY(target);
		return edgeBezierPath(sx, sy, tx, ty, 0);
	}

	function sectionEdgeTerminus(edge: SectionLayoutEdge): { x: number; y: number } {
		const target = sectionNodeById.get(secEdgeTargetId(edge));
		if (!target) return { x: 0, y: 0 };
		return { x: getSectionNodeX(target), y: getSectionNodeY(target) };
	}

	function sectionEdgeStrokeColor(edge: SectionLayoutEdge): string {
		const source = sectionNodeById.get(secEdgeSourceId(edge));
		if (!source) return "#64748B";
		return sectionNodeColors(source).stroke;
	}

	function sectionEdgeDotColor(edge: SectionLayoutEdge): string {
		const target = sectionNodeById.get(secEdgeTargetId(edge));
		if (!target) return "#64748B";
		return sectionNodeColors(target).stroke;
	}

	// ── Section hover state (debounced) ──
	let hoveredSectionId: string | null = $state(null);
	let sectionHoverTimer: ReturnType<typeof setTimeout> | null = null;
	function setHoveredSection(id: string | null) {
		if (sectionHoverTimer) clearTimeout(sectionHoverTimer);
		sectionHoverTimer = setTimeout(() => {
			hoveredSectionId = id;
		}, 30);
	}
	let hoveredSectionConnectedIds: Set<string> = $derived.by(() => {
		if (!hoveredSectionId) return new Set<string>();
		const connected = new Set<string>();
		connected.add(hoveredSectionId);
		for (const edge of sectionEdges) {
			const sid = secEdgeSourceId(edge),
				tid = secEdgeTargetId(edge);
			if (sid === hoveredSectionId) connected.add(tid);
			if (tid === hoveredSectionId) connected.add(sid);
		}
		return connected;
	});

	function sectionEdgeOpacity(edge: SectionLayoutEdge): number {
		const sid = secEdgeSourceId(edge),
			tid = secEdgeTargetId(edge);
		if (hoveredSectionId) {
			const isConnected = sid === hoveredSectionId || tid === hoveredSectionId;
			return isConnected ? 0.7 : 0.06;
		}
		return 0.15;
	}

	// ── Section tree layout init + force simulation ──
	function initSectionLayout() {
		if (!sectionMode || sections.length === 0 || !containerEl) return;

		const W = containerEl.clientWidth;
		const H = containerEl.clientHeight;
		const cx = W / 2,
			cy = H / 2;

		sectionLayout = buildSectionTreeLayout(sections, cx, cy);

		// Set initial positions for first Svelte render
		const posMap = new Map<string, { x: number; y: number }>();
		for (const node of sectionLayout.nodes) posMap.set(node.id, { x: node.x, y: node.y });
		sectionPositions = posMap;

		if (sectionSim) sectionSim.stop();
		let hasFitted = false;

		sectionSim = createSectionSimulation(sectionLayout.nodes, sectionLayout.edges, cx, cy, () => {
			// D3 direct DOM update — no Svelte re-render during sim
			if (!svgEl) return;
			const svg = d3.select(svgEl);

			// PERF FIX: index section nodes by id ONCE per tick (O(N)) so the
			// per-node / per-edge / per-dot .each() callbacks below do O(1) Map
			// lookups instead of O(N) Array.find() — same O(N^2)+O(E*N) → O(N+E)
			// optimization as the call-graph tick. d3-force mutates these node
			// objects in place, so rebuilding per tick reads fresh x/y.
			const nodeById = new Map<string, SectionLayoutNode>();
			for (const n of sectionLayout!.nodes) nodeById.set(n.id, n);

			svg.selectAll<SVGGElement, unknown>(".section-node").each(function () {
				const el = d3.select(this);
				const nid = el.attr("data-node-id");
				const node = nodeById.get(nid);
				if (node) el.attr("transform", `translate(${node.x},${node.y})`);
			});

			svg.selectAll<SVGPathElement, unknown>(".sec-edge-path").each(function () {
				const el = d3.select(this);
				const sid = el.attr("data-source");
				const tid = el.attr("data-target");
				const sn = nodeById.get(sid);
				const tn = nodeById.get(tid);
				if (sn && tn) el.attr("d", edgeBezierPath(sn.x, sn.y, tn.x, tn.y, 0));
			});

			svg.selectAll<SVGCircleElement, unknown>(".sec-edge-dot").each(function () {
				const el = d3.select(this);
				const tid = el.attr("data-target");
				const tn = nodeById.get(tid);
				if (tn) {
					el.attr("cx", tn.x);
					el.attr("cy", tn.y);
				}
			});

			if (!hasFitted && sectionSim && sectionSim.alpha() < 0.05) {
				hasFitted = true;
				const finalPos = new Map<string, { x: number; y: number }>();
				for (const node of sectionLayout!.nodes) finalPos.set(node.id, { x: node.x, y: node.y });
				sectionPositions = finalPos;
				autoFitSections();
			}
		});
	}

	// ── Section tree auto-fit (via computeFitTransform) ──
	function autoFitSections() {
		if (!svgEl || !containerEl || !sectionLayout || sectionNodes.length === 0) return;

		const rects: FitRect[] = sectionNodes.map((node) => ({
			x: getSectionNodeX(node),
			y: getSectionNodeY(node),
			halfW: sectionPillWidth(node) / 2,
			halfH: PILL_HEIGHT / 2,
		}));

		const sw = containerEl.clientWidth;
		const sh = containerEl.clientHeight;
		const { tx, ty, scale } = computeFitTransform(rects, sw, sh);
		const transform = d3.zoomIdentity.translate(tx, ty).scale(scale);

		if (zoomBehavior && svgEl) {
			d3.select(svgEl).call(zoomBehavior.transform, transform);
		}
	}

	// ── Section tree zoom setup ──
	function setupSectionZoom() {
		if (!svgEl) return;

		zoomBehavior = d3
			.zoom<SVGSVGElement, unknown>()
			.scaleExtent([0.1, 5])
			.on("zoom", (event) => {
				zoomTransform = event.transform;
			});

		d3.select(svgEl).call(zoomBehavior);

		// Click empty space — no-op in section mode (just allow deselect)
		d3.select(svgEl).on("click", (event) => {
			if (event.target === svgEl) {
				// no-op for section mode, just deselect
			}
		});
	}

	// ── Svelte action for d3.drag on section nodes (fx/fy pinning) ──
	function sectionDrag(el: SVGGElement, nodeId: string) {
		const drag = d3
			.drag<SVGGElement, unknown>()
			.on("start", () => {
				const node = sectionLayout?.nodes.find((n) => n.id === nodeId);
				if (node) {
					node.fx = node.x;
					node.fy = node.y;
				}
				sectionSim?.alphaTarget(0.05).restart();
			})
			.on("drag", (event) => {
				const node = sectionLayout?.nodes.find((n) => n.id === nodeId);
				if (!node) return;
				node.fx = (node.fx ?? node.x) + event.dx / zoomTransform.k;
				node.fy = (node.fy ?? node.y) + event.dy / zoomTransform.k;
			})
			.on("end", () => {
				const node = sectionLayout?.nodes.find((n) => n.id === nodeId);
				if (node) {
					node.fx = null;
					node.fy = null;
				}
				sectionSim?.alphaTarget(0);
				// Sync final positions to Svelte for hover reactivity
				if (sectionLayout) {
					const posMap = new Map<string, { x: number; y: number }>();
					for (const n of sectionLayout.nodes) posMap.set(n.id, { x: n.x, y: n.y });
					sectionPositions = posMap;
				}
			});
		d3.select(el).call(drag);
		return {
			destroy() {
				d3.select(el).on(".drag", null);
			},
		};
	}

	// ── Section node click handler ──
	function handleSectionClick(event: MouseEvent, node: SectionLayoutNode) {
		event.stopPropagation();
		onselectsection?.(node.section);
	}

	// ── Node position helpers ──
	function getNodeX(node: MicroNode): number {
		return nodePositions.get(node.id)?.x ?? node.x;
	}
	function getNodeY(node: MicroNode): number {
		return nodePositions.get(node.id)?.y ?? node.y;
	}

	// ── Link source/target ID helper (d3-force mutates string → object) ──
	function linkSourceId(link: MicroLink): string {
		return typeof link.source === "string" ? link.source : (link.source as unknown as MicroNode).id;
	}
	function linkTargetId(link: MicroLink): string {
		return typeof link.target === "string" ? link.target : (link.target as unknown as MicroNode).id;
	}

	// ── Node geometry helpers ──
	function nodeLabel(node: MicroNode): string {
		return microNodeLabel(node);
	}

	function nodeWidth(node: MicroNode): number {
		if (node.isGhost) return GHOST_PILL_WIDTH;
		return pillWidth(nodeLabel(node));
	}

	function nodeHeight(node: MicroNode): number {
		if (node.isGhost) return GHOST_PILL_HEIGHT;
		if (isClassType(node.chunk_type)) return CLASS_CARD_HEIGHT;
		return PILL_HEIGHT;
	}

	// ── Edge path computation ──
	function computeEdgePath(link: MicroLink): string {
		const sid = linkSourceId(link),
			tid = linkTargetId(link);
		const sourceNode = nodeById.get(sid);
		const targetNode = nodeById.get(tid);
		if (!sourceNode || !targetNode) return "";

		const sx = getNodeX(sourceNode);
		const sy = getNodeY(sourceNode);
		const tx = getNodeX(targetNode);
		const ty = getNodeY(targetNode);

		return edgeBezierPath(sx, sy, tx, ty, link.curvature);
	}

	// ── Edge terminus (dot) position ──
	function edgeTerminus(link: MicroLink): { x: number; y: number } {
		const targetNode = nodeById.get(linkTargetId(link));
		if (!targetNode) return { x: 0, y: 0 };
		return { x: getNodeX(targetNode), y: getNodeY(targetNode) };
	}

	// ── Edge color: source node's stroke color ──
	function edgeStrokeColor(link: MicroLink): string {
		const sourceNode = nodeById.get(linkSourceId(link));
		if (!sourceNode) return "#64748B";
		return getChunkColors(sourceNode).stroke;
	}

	// ── Edge style helpers ──
	function isHotspotEdge(link: MicroLink): boolean {
		const sid = linkSourceId(link),
			tid = linkTargetId(link);
		return hotspotIds.has(sid) || hotspotIds.has(tid);
	}

	function edgeDashArray(link: MicroLink): string {
		const sourceNode = nodeById.get(linkSourceId(link));
		const targetNode = nodeById.get(linkTargetId(link));
		if (sourceNode?.isGhost || targetNode?.isGhost) return "3 2";
		// Confidence-based dash: solid (>=0.9), dashed (>=0.5), dotted (<0.5)
		if (link.confidence > 0 && link.confidence < 0.5) return "2 3";
		if (link.confidence >= 0.5 && link.confidence < 0.9) return "6 4";
		return "none";
	}

	// ── Selection logic ──
	function computeSelection(selectedId: string | null) {
		if (!selectedId) {
			upstreamIds = new Set();
			downstreamIds = new Set();
			return;
		}
		const up = new Set<string>();
		const down = new Set<string>();
		for (const link of graphLinks) {
			const sid = linkSourceId(link),
				tid = linkTargetId(link);
			if (sid === selectedId) down.add(tid);
			if (tid === selectedId) up.add(sid);
		}
		upstreamIds = up;
		downstreamIds = down;
	}

	// ── Initialize layout + force simulation ──
	function initLayout() {
		if (sectionMode || chunks.length === 0 || !containerEl) return;

		const W = containerEl.clientWidth;
		const H = containerEl.clientHeight;
		const cx = W / 2,
			cy = H / 2;

		layoutResult = buildCallGraphLayout(chunks, calls, external, cx, cy);

		// Set initial positions for first Svelte render
		const posMap = new Map<string, { x: number; y: number }>();
		for (const node of layoutResult.nodes) {
			posMap.set(node.id, { x: node.x, y: node.y });
		}
		nodePositions = posMap;

		if (simulation) simulation.stop();
		let hasFitted = false;
		let hasZoomedToChunk = false;

		simulation = createForceSimulation(layoutResult.nodes, layoutResult.links, cx, cy, () => {
			// Tick: use D3 direct DOM update — NO Svelte reactivity (prevents flicker)
			if (!svgEl) return;
			const svg = d3.select(svgEl);

			// PERF FIX: index nodes by id ONCE per tick (O(N)) so the per-node /
			// per-edge / per-dot .each() callbacks below do O(1) Map lookups instead
			// of O(N) Array.find(). This turns each tick from O(N^2)+O(E*N) into
			// O(N+E). d3-force mutates the same node objects in place, so rebuilding
			// the map each tick still reads fresh x/y positions.
			const nodeById = new Map<string, MicroNode>();
			for (const n of layoutResult!.nodes) nodeById.set(n.id, n);

			// Update node transforms directly
			svg.selectAll<SVGGElement, unknown>(".micro-node").each(function () {
				const el = d3.select(this);
				const nodeId = el.attr("data-node-id");
				const node = nodeById.get(nodeId);
				if (node) el.attr("transform", `translate(${node.x},${node.y})`);
			});

			// Update edge paths directly
			svg.selectAll<SVGPathElement, unknown>(".edge-path").each(function () {
				const el = d3.select(this);
				const sid = el.attr("data-source");
				const tid = el.attr("data-target");
				const sn = nodeById.get(sid);
				const tn = nodeById.get(tid);
				if (sn && tn) {
					const curv = parseFloat(el.attr("data-curvature") || "0");
					el.attr("d", edgeBezierPath(sn.x, sn.y, tn.x, tn.y, curv));
				}
			});

			// Update dot terminus positions directly
			svg.selectAll<SVGCircleElement, unknown>(".edge-dot").each(function () {
				const el = d3.select(this);
				const tid = el.attr("data-target");
				const tn = nodeById.get(tid);
				if (tn) {
					el.attr("cx", tn.x);
					el.attr("cy", tn.y);
				}
			});

			// Sync to Svelte state only when settled (for hover/selection reactivity)
			if (!hasFitted && simulation && simulation.alpha() < 0.05) {
				hasFitted = true;
				// Sync final positions to Svelte state
				const finalPos = new Map<string, { x: number; y: number }>();
				for (const node of layoutResult!.nodes) {
					finalPos.set(node.id, { x: node.x, y: node.y });
				}
				nodePositions = finalPos;
				autoFit();

				// After auto-fit, if a chunk is pre-selected (nav intent), zoom to it
				if (!hasZoomedToChunk && selectedChunkId) {
					hasZoomedToChunk = true;
					const targetNode = layoutResult!.nodes.find((n) => n.id === selectedChunkId);
					if (targetNode && svgEl && containerEl && zoomBehavior) {
						const sw = containerEl.clientWidth;
						const sh = containerEl.clientHeight;
						const scale = 1.2;
						const tx = sw / 2 - targetNode.x * scale;
						const ty = sh / 2 - targetNode.y * scale;
						const transform = d3.zoomIdentity.translate(tx, ty).scale(scale);
						d3.select(svgEl).transition().duration(500).call(zoomBehavior.transform, transform);
					}
				}
			}
		});
	}

	// ── Auto-fit viewport (via computeFitTransform) ──
	function autoFit() {
		if (!svgEl || !containerEl || !layoutResult || graphNodes.length === 0) return;

		const rects: FitRect[] = graphNodes.map((node) => ({
			x: getNodeX(node),
			y: getNodeY(node),
			halfW: nodeWidth(node) / 2,
			halfH: nodeHeight(node) / 2,
		}));

		const sw = containerEl.clientWidth;
		const sh = containerEl.clientHeight;
		const { tx, ty, scale } = computeFitTransform(rects, sw, sh);
		const transform = d3.zoomIdentity.translate(tx, ty).scale(scale);

		if (zoomBehavior && svgEl) {
			d3.select(svgEl).call(zoomBehavior.transform, transform);
		}
	}

	// ── Setup d3.zoom ──
	function setupZoom() {
		if (!svgEl) return;

		zoomBehavior = d3
			.zoom<SVGSVGElement, unknown>()
			.scaleExtent([0.1, 5])
			.on("zoom", (event) => {
				zoomTransform = event.transform;
			});

		d3.select(svgEl).call(zoomBehavior);

		// Click empty space to deselect
		d3.select(svgEl).on("click", (event) => {
			// Only deselect if clicking the SVG background (not a node)
			if (event.target === svgEl || (event.target as Element)?.tagName === "svg") {
				onselectchunk?.("");
			}
		});
	}

	// ── Svelte action for d3.drag on individual nodes ──
	function nodeDrag(el: SVGGElement, nodeId: string) {
		const drag = d3
			.drag<SVGGElement, unknown>()
			.on("start", () => {
				const node = layoutResult?.nodes.find((n) => n.id === nodeId);
				if (node) {
					node.fx = node.x;
					node.fy = node.y;
				}
				simulation?.alphaTarget(0.05).restart();
			})
			.on("drag", (event) => {
				const node = layoutResult?.nodes.find((n) => n.id === nodeId);
				if (!node) return;
				node.fx = (node.fx ?? node.x) + event.dx / zoomTransform.k;
				node.fy = (node.fy ?? node.y) + event.dy / zoomTransform.k;
			})
			.on("end", () => {
				const node = layoutResult?.nodes.find((n) => n.id === nodeId);
				if (node) {
					node.fx = null;
					node.fy = null;
				}
				simulation?.alphaTarget(0);
				// Sync final positions to Svelte for hover/selection reactivity
				if (layoutResult) {
					const posMap = new Map<string, { x: number; y: number }>();
					for (const n of layoutResult.nodes) posMap.set(n.id, { x: n.x, y: n.y });
					nodePositions = posMap;
				}
			});
		d3.select(el).call(drag);
		return {
			destroy() {
				d3.select(el).on(".drag", null);
			},
		};
	}

	// ── Node click handler ──
	function handleNodeClick(event: MouseEvent, node: MicroNode) {
		event.stopPropagation();
		if (node.isGhost && node.ghostModuleId) {
			onghostclick?.(node.ghostModuleId);
		} else {
			onselectchunk?.(node.id);
		}
	}

	// ── Reactivity: watch selectedChunkId ──
	$effect(() => {
		const id = selectedChunkId;
		untrack(() => computeSelection(id));
	});

	// ── Hover visual: D3 transition + attr ──
	function applyCallGraphHover() {
		if (!svgEl) return;
		const svg = d3.select(svgEl);
		const hid = hoveredNodeId;
		const connected = hoveredConnectedIds;

		// Nodes: transition opacity
		svg
			.selectAll<SVGGElement, unknown>(".micro-node")
			.transition()
			.duration(200)
			.attr("opacity", function () {
				if (!hid) return 1;
				const nid = d3.select(this).attr("data-node-id");
				return connected.has(nid) ? 1 : 0.15;
			});

		// Edges: transition opacity
		svg
			.selectAll<SVGPathElement, unknown>(".edge-path")
			.transition()
			.duration(200)
			.attr("opacity", function () {
				if (!hid) return null; // fall back to CSS default
				const el = d3.select(this);
				const sid = el.attr("data-source"),
					tid = el.attr("data-target");
				return sid === hid || tid === hid ? 0.6 : 0.03;
			});

		// Dots: transition opacity
		svg
			.selectAll<SVGCircleElement, unknown>(".edge-dot")
			.transition()
			.duration(200)
			.attr("opacity", function () {
				if (!hid) return null;
				const el = d3.select(this);
				const tid = el.attr("data-target");
				// A dot is connected if the hovered node is source or target of an edge ending at this dot
				const isConn =
					svg.selectAll(`.edge-path[data-target="${tid}"][data-source="${hid}"]`).size() > 0 ||
					svg.selectAll(`.edge-path[data-source="${tid}"][data-target="${hid}"]`).size() > 0 ||
					tid === hid;
				return isConn ? 0.5 : 0.01;
			});
	}

	function applySectionTreeHover() {
		if (!svgEl) return;
		const svg = d3.select(svgEl);
		const hid = hoveredSectionId;
		const connected = hoveredSectionConnectedIds;

		svg
			.selectAll<SVGGElement, unknown>(".section-node")
			.transition()
			.duration(200)
			.attr("opacity", function () {
				if (!hid) return 1;
				const nid = d3.select(this).attr("data-node-id");
				return connected.has(nid) ? 1 : 0.15;
			});

		svg
			.selectAll<SVGPathElement, unknown>(".sec-edge-path")
			.transition()
			.duration(200)
			.attr("opacity", function () {
				if (!hid) return null;
				const el = d3.select(this);
				const sid = el.attr("data-source"),
					tid = el.attr("data-target");
				return sid === hid || tid === hid ? 0.7 : 0.04;
			});

		svg
			.selectAll<SVGCircleElement, unknown>(".sec-edge-dot")
			.transition()
			.duration(200)
			.attr("opacity", function () {
				if (!hid) return null;
				const el = d3.select(this);
				const tid = el.attr("data-target");
				const isConn =
					svg.selectAll(`.sec-edge-path[data-target="${tid}"][data-source="${hid}"]`).size() > 0 ||
					svg.selectAll(`.sec-edge-path[data-source="${tid}"][data-target="${hid}"]`).size() > 0 ||
					tid === hid;
				return isConn ? 0.5 : 0.02;
			});
	}

	$effect(() => {
		hoveredNodeId;
		hoveredConnectedIds;
		untrack(() => applyCallGraphHover());
	});
	$effect(() => {
		hoveredSectionId;
		hoveredSectionConnectedIds;
		untrack(() => applySectionTreeHover());
	});

	// ── Lifecycle: init on mount ──
	$effect(() => {
		if (containerEl && svgEl && !sectionMode) {
			untrack(() => {
				initLayout();
				setupZoom();
				// auto-fit is called inside the simulation tick when alpha < 0.05
			});
		}
	});

	// ── Section tree lifecycle: init on mount ──
	$effect(() => {
		if (containerEl && svgEl && sectionMode && sections.length > 0) {
			untrack(() => {
				initSectionLayout();
				setupSectionZoom();
				// auto-fit handled inside simulation tick when alpha < 0.05
			});
		}
	});

	onDestroy(() => {
		if (simulation) {
			simulation.stop();
			simulation = null;
		}
		if (sectionSim) {
			sectionSim.stop();
			sectionSim = null;
		}
		zoomBehavior = null;
	});
</script>

{#if sectionMode}
	<div class="relative w-full h-full overflow-hidden" bind:this={containerEl}>
		<!-- SVG Section Tree -->
		<svg bind:this={svgEl} class="w-full h-full" style="background: transparent;">
			<defs>
				<!-- Glow filter for D0 root (rose) -->
				<filter id="glow-rose-section" x="-50%" y="-50%" width="200%" height="200%">
					<feGaussianBlur in="SourceAlpha" stdDeviation="6" result="blur" />
					<feFlood flood-color="#F472B6" flood-opacity="0.18" result="color" />
					<feComposite in="color" in2="blur" operator="in" result="glow" />
					<feMerge>
						<feMergeNode in="glow" />
						<feMergeNode in="SourceGraphic" />
					</feMerge>
				</filter>
				<!-- Glow filter for amber (explains) -->
				<filter id="glow-amber-section" x="-50%" y="-50%" width="200%" height="200%">
					<feGaussianBlur in="SourceAlpha" stdDeviation="5" result="blur" />
					<feFlood flood-color="#FBBF24" flood-opacity="0.15" result="color" />
					<feComposite in="color" in2="blur" operator="in" result="glow" />
					<feMerge>
						<feMergeNode in="glow" />
						<feMergeNode in="SourceGraphic" />
					</feMerge>
				</filter>
			</defs>

			<g transform="translate({zoomTransform.x},{zoomTransform.y}) scale({zoomTransform.k})">
				<!-- Edges -->
				<g class="section-edges">
					{#each sectionEdges as edge (`${secEdgeSourceId(edge)}-${secEdgeTargetId(edge)}`)}
						{@const path = computeSectionEdgePath(edge)}
						{@const terminus = sectionEdgeTerminus(edge)}
						{@const dotColor = sectionEdgeDotColor(edge)}
						{@const opacity = sectionEdgeOpacity(edge)}
						<!-- Edge path -->
						<path
							class="sec-edge-path"
							data-source={secEdgeSourceId(edge)}
							data-target={secEdgeTargetId(edge)}
							d={path}
							fill="none"
							stroke={sectionEdgeStrokeColor(edge)}
							stroke-width="1.2"
							{opacity}
							stroke-linecap="round"
						/>
						<!-- Dot terminus at target -->
						<circle
							class="sec-edge-dot"
							data-target={secEdgeTargetId(edge)}
							cx={terminus.x}
							cy={terminus.y}
							r="5"
							fill={dotColor}
							opacity={opacity * 0.5}
						/>
						<circle
							class="sec-edge-dot"
							data-target={secEdgeTargetId(edge)}
							cx={terminus.x}
							cy={terminus.y}
							r="2.5"
							fill={dotColor}
							opacity={opacity * 0.85}
						/>
					{/each}
				</g>

				<!-- Nodes -->
				<g class="section-nodes">
					{#each sectionNodes as node (node.id)}
						{@const colors = sectionNodeColors(node)}
						{@const w = sectionPillWidth(node)}
						{@const h = PILL_HEIGHT}
						{@const nx = getSectionNodeX(node)}
						{@const ny = getSectionNodeY(node)}
						{@const hasExplains = (node.section.explains?.length ?? 0) > 0}
						{@const explainsCount = node.section.explains?.length ?? 0}
						{@const isRoot = node.depth === 0}
						{@const glowFilter = hasExplains
							? "url(#glow-amber-section)"
							: isRoot
								? "url(#glow-rose-section)"
								: "none"}
						<!-- svelte-ignore a11y_click_events_have_key_events -->
						<g
							class="section-node outline-none"
							data-node-id={node.id}
							transform="translate({nx},{ny})"
							style="cursor: pointer; animation-delay: {node.depth * 80}ms"
							tabindex="0"
							role="button"
							onclick={(e) => handleSectionClick(e, node)}
							onmouseenter={() => setHoveredSection(node.id)}
							onmouseleave={() => setHoveredSection(null)}
							use:sectionDrag={node.id}
						>
							<!-- Invisible hit area for stable hover -->
							<rect
								x={-(pillWidth(node.heading) / 2 + 12)}
								y={-(PILL_HEIGHT / 2 + 10)}
								width={pillWidth(node.heading) + 24}
								height={PILL_HEIGHT + 20}
								fill="transparent"
								stroke="none"
								pointer-events="all"
							/>
							<!-- Glow halo for root or explains nodes -->
							{#if glowFilter !== "none"}
								<rect
									x={-w / 2 - 3}
									y={-h / 2 - 3}
									width={w + 6}
									height={h + 6}
									rx={(h + 6) / 2}
									fill="none"
									stroke={colors.stroke}
									stroke-width="0.5"
									opacity="0.25"
									filter={glowFilter}
								/>
							{/if}
							<!-- Pill shape -->
							<rect
								x={-w / 2}
								y={-h / 2}
								width={w}
								height={h}
								rx={h / 2}
								fill={colors.fill}
								stroke={colors.stroke}
								stroke-width={isRoot ? 1.8 : 1.2}
							/>
							<!-- Heading text -->
							<text
								x="0"
								y="0"
								text-anchor="middle"
								dominant-baseline="central"
								fill={colors.textFill}
								font-family="'JetBrains Mono', monospace"
								font-size="10"
							>
								{node.heading}
							</text>
							<!-- Amber explains count badge at top-right -->
							{#if hasExplains}
								{@const badgeText = String(explainsCount)}
								{@const badgeW = Math.max(badgeText.length * 7 + 8, 20)}
								<rect
									x={w / 2 - badgeW / 2 + 4}
									y={-h / 2 - 8}
									width={badgeW}
									height={14}
									rx="7"
									fill="#2A1F05"
									stroke="#FBBF24"
									stroke-width="0.8"
								/>
								<text
									x={w / 2 + 4}
									y={-h / 2 - 1}
									text-anchor="middle"
									dominant-baseline="central"
									fill="#FDE68A"
									font-family="'JetBrains Mono', monospace"
									font-size="8"
									font-weight="600"
								>
									{explainsCount}
								</text>
							{/if}
							<title
								>{node.heading} (depth {node.depth}){hasExplains
									? ` — ${explainsCount} explains`
									: ""}</title
							>
						</g>
					{/each}
				</g>
			</g>
		</svg>

		<!-- Section Tree Legend overlay (HTML) -->
		<div
			class="absolute top-3 left-3 z-10 rounded-lg border border-white/[0.06] bg-[hsl(220_25%_7%/0.85)] backdrop-blur-sm px-3 py-2.5 min-w-[110px]"
		>
			<span
				class="font-mono text-[10px] font-semibold tracking-widest uppercase text-muted-foreground block mb-1.5"
				>Section Tree</span
			>
			<!-- D0 Rose — Root -->
			<div class="flex items-center gap-2 mb-1">
				<span
					class="w-2.5 h-2.5 rounded-full shrink-0"
					style="background: #3D1428; border: 1.2px solid #F472B6;"
				></span>
				<span class="text-[10px] text-muted-foreground font-mono">Root</span>
			</div>
			<!-- D1 Teal — Level 1 -->
			<div class="flex items-center gap-2 mb-1">
				<span
					class="w-2.5 h-2.5 rounded-full shrink-0"
					style="background: #0D3331; border: 1.2px solid #2DD4BF;"
				></span>
				<span class="text-[10px] text-muted-foreground font-mono">Level 1</span>
			</div>
			<!-- D2 Blue — Level 2 -->
			<div class="flex items-center gap-2 mb-1">
				<span
					class="w-2.5 h-2.5 rounded-full shrink-0"
					style="background: #172554; border: 1.2px solid #60A5FA;"
				></span>
				<span class="text-[10px] text-muted-foreground font-mono">Level 2</span>
			</div>
			<!-- D3 Violet — Level 3+ -->
			<div class="flex items-center gap-2 mb-1">
				<span
					class="w-2.5 h-2.5 rounded-full shrink-0"
					style="background: #1E1B4B; border: 1.2px solid #A78BFA;"
				></span>
				<span class="text-[10px] text-muted-foreground font-mono">Level 3+</span>
			</div>
			<div class="h-px bg-white/[0.06] my-1.5"></div>
			<!-- Amber — Has Explains -->
			<div class="flex items-center gap-2 mb-1">
				<span
					class="w-2.5 h-2.5 rounded-full shrink-0"
					style="background: #2A1F05; border: 1.2px solid #FBBF24;"
				></span>
				<span class="text-[10px] text-muted-foreground font-mono">Has Explains</span>
			</div>
			<!-- Dot + Hierarchy -->
			<div class="flex items-center gap-2">
				<svg class="w-4 h-2.5 shrink-0" viewBox="0 0 20 10">
					<path
						d="M10,0 C10,5 17,5 17,10"
						stroke="#F472B6"
						stroke-width="1.2"
						fill="none"
						opacity="0.4"
					/>
					<circle cx="17" cy="10" r="2" fill="#2DD4BF" opacity="0.7" />
				</svg>
				<span class="text-[10px] text-muted-foreground font-mono">Hierarchy</span>
			</div>
		</div>

		<!-- Stats watermark -->
		<div class="absolute bottom-3 left-4 z-2">
			<span class="font-mono text-[9px] text-[rgba(100,116,139,0.35)]">
				{documentTitle || "Document"} | {sections.length} sections
			</span>
		</div>
	</div>
{:else}
	<div class="relative w-full h-full overflow-hidden" bind:this={containerEl}>
		<!-- SVG Call Graph -->
		<svg bind:this={svgEl} class="w-full h-full" style="background: transparent;">
			<defs>
				<!-- Glow filters -->
				<filter id="glow-teal" x="-50%" y="-50%" width="200%" height="200%">
					<feGaussianBlur in="SourceAlpha" stdDeviation="5" result="blur" />
					<feFlood flood-color="#2DD4BF" flood-opacity="0.15" result="color" />
					<feComposite in="color" in2="blur" operator="in" result="glow" />
					<feMerge>
						<feMergeNode in="glow" />
						<feMergeNode in="SourceGraphic" />
					</feMerge>
				</filter>
				<filter id="glow-rose" x="-50%" y="-50%" width="200%" height="200%">
					<feGaussianBlur in="SourceAlpha" stdDeviation="5" result="blur" />
					<feFlood flood-color="#FB7185" flood-opacity="0.15" result="color" />
					<feComposite in="color" in2="blur" operator="in" result="glow" />
					<feMerge>
						<feMergeNode in="glow" />
						<feMergeNode in="SourceGraphic" />
					</feMerge>
				</filter>
				<filter id="glow-amber" x="-50%" y="-50%" width="200%" height="200%">
					<feGaussianBlur in="SourceAlpha" stdDeviation="5" result="blur" />
					<feFlood flood-color="#FBBF24" flood-opacity="0.15" result="color" />
					<feComposite in="color" in2="blur" operator="in" result="glow" />
					<feMerge>
						<feMergeNode in="glow" />
						<feMergeNode in="SourceGraphic" />
					</feMerge>
				</filter>
			</defs>

			<g transform="translate({zoomTransform.x},{zoomTransform.y}) scale({zoomTransform.k})">
				<!-- (No depth bands in force-directed mode) -->

				<!-- Edges -->
				<g class="edges">
					{#each graphLinks as link, i (`${linkSourceId(link)}-${linkTargetId(link)}-${link.method}-${i}`)}
						{@const path = computeEdgePath(link)}
						{@const terminus = edgeTerminus(link)}
						{@const isHot = isHotspotEdge(link)}
						<!-- Edge path -->
						<path
							class="edge-path {link.isExternal ? 'external' : ''} {isHot ? 'hot' : ''}"
							data-source={linkSourceId(link)}
							data-target={linkTargetId(link)}
							data-curvature={link.curvature}
							d={path}
							fill="none"
							stroke={edgeStrokeColor(link)}
							stroke-width={isHot ? 2 : 1}
							stroke-linecap="round"
							stroke-dasharray={edgeDashArray(link)}
						/>
						<!-- Dot terminus at target -->
						<circle
							class="edge-dot {link.isExternal ? 'external' : ''} {isHot ? 'hot' : ''}"
							data-target={linkTargetId(link)}
							cx={terminus.x}
							cy={terminus.y}
							r={isHot ? 6 : 5}
							fill={edgeStrokeColor(link)}
						/>
						<circle
							class="edge-dot {link.isExternal ? 'external' : ''} {isHot ? 'hot' : ''}"
							data-target={linkTargetId(link)}
							cx={terminus.x}
							cy={terminus.y}
							r={isHot ? 3 : 2.5}
							fill={edgeStrokeColor(link)}
						/>
					{/each}
				</g>

				<!-- Nodes -->
				<g class="nodes">
					{#each graphNodes as node (node.id)}
						{@const colors = getChunkColors(node)}
						{@const label = nodeLabel(node)}
						{@const w = nodeWidth(node)}
						{@const h = nodeHeight(node)}
						{@const nx = getNodeX(node)}
						{@const ny = getNodeY(node)}
						{@const isHot = hotspotIds.has(node.id)}
						{@const glowFilter = isHot
							? isClassType(node.chunk_type)
								? "url(#glow-rose)"
								: isEnumType(node.chunk_type)
									? "url(#glow-amber)"
									: "url(#glow-teal)"
							: "none"}
						<!-- svelte-ignore a11y_click_events_have_key_events -->
						<g
							class="micro-node outline-none"
							data-node-id={node.id}
							transform="translate({nx},{ny})"
							style="cursor: pointer;"
							tabindex="0"
							role="button"
							onclick={(e) => handleNodeClick(e, node)}
							onmouseenter={() => setHoveredNode(node.id)}
							onmouseleave={() => setHoveredNode(null)}
							use:nodeDrag={node.id}
						>
							<!-- Invisible hit area larger than node for stable hover -->
							<rect
								x={-(pillWidth(microNodeLabel(node)) / 2 + 12)}
								y={-(isClassType(node.chunk_type) ? CLASS_CARD_HEIGHT : PILL_HEIGHT) / 2 - 10}
								width={pillWidth(microNodeLabel(node)) + 24}
								height={(isClassType(node.chunk_type) ? CLASS_CARD_HEIGHT : PILL_HEIGHT) + 20}
								fill="transparent"
								stroke="none"
								pointer-events="all"
							/>
							{#if node.isGhost}
								<!-- Ghost node: dashed pill -->
								<rect
									x={-w / 2}
									y={-h / 2}
									width={w}
									height={h}
									rx={h / 2}
									fill={colors.fill}
									stroke={colors.stroke}
									stroke-width="1"
									stroke-dasharray="3 2"
									opacity="0.4"
								/>
								<text
									x="0"
									y="0"
									text-anchor="middle"
									dominant-baseline="central"
									fill={colors.textFill}
									font-family="'JetBrains Mono', monospace"
									font-size="9"
									opacity="0.5"
								>
									{label}
								</text>
							{:else if isClassType(node.chunk_type)}
								<!-- Class/Struct: Header card -->
								{#if isHot}
									<rect
										x={-w / 2 - 3}
										y={-h / 2 - 3}
										width={w + 6}
										height={h + 6}
										rx="10"
										fill="none"
										stroke={colors.stroke}
										stroke-width="0.5"
										opacity="0.2"
										filter={glowFilter}
									/>
								{/if}
								<rect
									x={-w / 2}
									y={-h / 2}
									width={w}
									height={h}
									rx="7"
									fill={colors.fill}
									stroke={colors.stroke}
									stroke-width={isHot ? 2 : 1.2}
								/>
								<!-- Header bar -->
								<rect
									x={-w / 2}
									y={-h / 2}
									width={w}
									height="16"
									rx="7"
									fill={colors.stroke}
									opacity="0.13"
								/>
								<!-- Bottom corners of header bar need square bottom -->
								<rect
									x={-w / 2}
									y={-h / 2 + 9}
									width={w}
									height="7"
									fill={colors.stroke}
									opacity="0.13"
								/>
								<!-- Type label + line count in header -->
								<text
									x={-w / 2 + 8}
									y={-h / 2 + 11}
									fill={colors.textFill}
									font-family="'JetBrains Mono', monospace"
									font-size="8"
									font-weight="600"
									letter-spacing="0.08em"
									opacity="0.6"
								>
									{node.chunk_type.toUpperCase()}
								</text>
								<text
									x={w / 2 - 8}
									y={-h / 2 + 11}
									text-anchor="end"
									fill={colors.textFill}
									font-family="'JetBrains Mono', monospace"
									font-size="7"
									opacity="0.4"
								>
									{node.line_count}L
								</text>
								<!-- Class name -->
								<text
									x="0"
									y={h / 2 - 8}
									text-anchor="middle"
									dominant-baseline="auto"
									fill={colors.textFill}
									font-family="'JetBrains Mono', monospace"
									font-size="11"
									font-weight="500"
								>
									{label}
								</text>
							{:else if isEnumType(node.chunk_type)}
								<!-- Enum/Constant: Amber rounded rect with type indicator -->
								{#if isHot}
									<rect
										x={-w / 2 - 3}
										y={-h / 2 - 3}
										width={w + 6}
										height={h + 6}
										rx={h / 2 + 3}
										fill="none"
										stroke={colors.stroke}
										stroke-width="0.5"
										opacity="0.2"
										filter={glowFilter}
									/>
								{/if}
								<rect
									x={-w / 2}
									y={-h / 2}
									width={w}
									height={h}
									rx="6"
									fill={colors.fill}
									stroke={colors.stroke}
									stroke-width={isHot ? 2 : 1.2}
								/>
								<!-- "E" type indicator -->
								<text
									x={-w / 2 + 8}
									y="0"
									text-anchor="start"
									dominant-baseline="central"
									fill={colors.stroke}
									font-family="'JetBrains Mono', monospace"
									font-size="9"
									font-weight="700"
									opacity="0.5"
								>
									E
								</text>
								<!-- Label -->
								<text
									x="4"
									y="0"
									text-anchor="middle"
									dominant-baseline="central"
									fill={colors.textFill}
									font-family="'JetBrains Mono', monospace"
									font-size="10"
								>
									{label}
								</text>
							{:else}
								<!-- Function/Method: Pill shape -->
								{#if isHot}
									<rect
										x={-w / 2 - 3}
										y={-h / 2 - 3}
										width={w + 6}
										height={h + 6}
										rx={(h + 6) / 2}
										fill="none"
										stroke={colors.stroke}
										stroke-width="0.5"
										opacity="0.2"
										filter={glowFilter}
									/>
								{/if}
								<rect
									x={-w / 2}
									y={-h / 2}
									width={w}
									height={h}
									rx={h / 2}
									fill={colors.fill}
									stroke={colors.stroke}
									stroke-width={isHot ? 2 : 1.2}
								/>
								<text
									x="0"
									y="0"
									text-anchor="middle"
									dominant-baseline="central"
									fill={colors.textFill}
									font-family="'JetBrains Mono', monospace"
									font-size="10"
								>
									{label}
								</text>
							{/if}
							<title
								>{node.isGhost
									? `Ghost: ${label} (${node.ghostModulePath ?? ""})`
									: `${node.chunk_type}: ${label} (${node.line_count} lines)`}</title
							>
						</g>
					{/each}
				</g>
			</g>
		</svg>

		<!-- Legend overlay (HTML) -->
		<div
			class="absolute top-3 left-3 z-10 rounded-lg border border-white/[0.06] bg-[hsl(220_25%_7%/0.85)] backdrop-blur-sm px-3 py-2.5 min-w-[110px]"
		>
			<span
				class="font-mono text-[10px] font-semibold tracking-widest uppercase text-muted-foreground block mb-1.5"
				>Call Graph</span
			>
			<!-- Function -->
			<div class="flex items-center gap-2 mb-1">
				<span
					class="w-2.5 h-2.5 rounded-full shrink-0"
					style="background: #0D3331; border: 1.2px solid #2DD4BF;"
				></span>
				<span class="text-[10px] text-muted-foreground font-mono">Function</span>
			</div>
			<!-- Class / Struct -->
			<div class="flex items-center gap-2 mb-1">
				<span
					class="w-3.5 h-2.5 shrink-0 rounded-[2px]"
					style="background: #2D0F1E; border: 1px solid #FB7185; border-left: 2px solid rgba(251,113,133,0.4);"
				></span>
				<span class="text-[10px] text-muted-foreground font-mono">Class / Struct</span>
			</div>
			<!-- Hotspot -->
			<div class="flex items-center gap-2 mb-1">
				<span
					class="w-2.5 h-2.5 rounded-full shrink-0"
					style="background: #0D3331; border: 1.5px solid #2DD4BF; box-shadow: 0 0 4px rgba(45,212,191,0.3);"
				></span>
				<span class="text-[10px] text-muted-foreground font-mono">Hotspot</span>
			</div>
			<!-- Enum -->
			<div class="flex items-center gap-2 mb-1">
				<span
					class="w-3 h-2.5 shrink-0 rounded-[2px]"
					style="background: #2A1F05; border: 1px solid #FBBF24;"
				></span>
				<span class="text-[10px] text-muted-foreground font-mono">Enum</span>
			</div>
			<!-- Ghost -->
			<div class="flex items-center gap-2 mb-1">
				<span
					class="w-3 h-2 shrink-0 rounded-full"
					style="background: #1E293B; border: 1px dashed #64748B; opacity: 0.5;"
				></span>
				<span class="text-[10px] text-muted-foreground font-mono">Ghost</span>
			</div>
			<div class="h-px bg-white/[0.06] my-1.5"></div>
			<!-- Edge with dot -->
			<div class="flex items-center gap-2">
				<svg class="w-4 h-2.5 shrink-0" viewBox="0 0 20 10">
					<path d="M0,5 Q10,0 17,5" stroke="#2DD4BF" stroke-width="1.2" fill="none" opacity="0.5" />
					<circle cx="17" cy="5" r="2" fill="#2DD4BF" opacity="0.7" />
				</svg>
				<span class="text-[10px] text-muted-foreground font-mono">Edge</span>
			</div>
		</div>

		<!-- Stats watermark -->
		<div class="absolute bottom-3 left-4 z-2">
			<span class="font-mono text-[9px] text-[rgba(100,116,139,0.35)]">
				{moduleName} | {chunks.length} chunks | {calls.length} calls | {external.length} external
				{#if layoutResult?.disconnectedCount}
					| {layoutResult.disconnectedCount} disconnected
				{/if}
			</span>
		</div>
	</div>
{/if}

<style>
	@keyframes node-enter {
		from {
			opacity: 0;
			transform: scale(0.9);
		}
		to {
			opacity: 1;
			transform: scale(1);
		}
	}
	.micro-node,
	.section-node {
		animation: node-enter 350ms ease-out backwards;
	}
	.micro-node:focus,
	.micro-node:focus-visible,
	.section-node:focus,
	.section-node:focus-visible {
		outline: none;
	}
	/* Default edge/dot opacity (overridden by D3 transition on hover) */
	.edge-path {
		opacity: 0.1;
	}
	.edge-path.hot {
		opacity: 0.18;
	}
	.edge-path.external {
		opacity: 0.06;
	}
	.sec-edge-path {
		opacity: 0.15;
	}
	.edge-dot {
		opacity: 0.08;
	}
	.edge-dot.hot {
		opacity: 0.12;
	}
	.edge-dot.external {
		opacity: 0.04;
	}
	.sec-edge-dot {
		opacity: 0.1;
	}
</style>
