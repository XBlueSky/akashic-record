// frontend/src/lib/graph/layout.ts
import * as d3 from "d3";
import type { ChunkNode, CallEdge, GhostNode, DocumentSectionItem } from "../types";
import type { MicroNode, MicroLink } from "./types";
import { isClassType, microNodeLabel, pillWidth, PILL_HEIGHT, CLASS_CARD_HEIGHT } from "./types";

// ── Call Graph Layout (d3-force) ─────────────────────────────

export interface CallGraphLayout {
	nodes: MicroNode[];
	links: MicroLink[];
	hotspotIds: Set<string>;
	disconnectedCount: number;
}

/** Collision radius accounting for pill/card widths */
function collisionRadius(node: MicroNode): number {
	if (node.isGhost) return 20;
	const label = microNodeLabel(node);
	const w = pillWidth(label);
	const h = isClassType(node.chunk_type) ? CLASS_CARD_HEIGHT : PILL_HEIGHT;
	return Math.max(w, h) / 2 + 8;
}

export function buildCallGraphLayout(
	chunks: ChunkNode[],
	calls: CallEdge[],
	external: GhostNode[],
	cx: number,
	cy: number,
): CallGraphLayout {
	// Compute degree
	const degreeMap = new Map<string, { in: number; out: number }>();
	for (const c of chunks) degreeMap.set(c.id, { in: 0, out: 0 });
	for (const call of calls) {
		const s = degreeMap.get(call.source);
		const t = degreeMap.get(call.target);
		if (s) s.out++;
		if (t) t.in++;
	}

	// Filter to connected nodes
	const connectedChunks = chunks.filter((c) => {
		const d = degreeMap.get(c.id);
		return d && (d.in > 0 || d.out > 0);
	});
	const disconnectedCount = chunks.length - connectedChunks.length;

	// Top 3 hotspots
	const ranked = connectedChunks
		.map((c) => ({ id: c.id, inDeg: degreeMap.get(c.id)?.in || 0 }))
		.sort((a, b) => b.inDeg - a.inDeg);
	const hotspotIds = new Set(ranked.slice(0, 3).map((c) => c.id));

	// Build nodes
	const nodes: MicroNode[] = connectedChunks.map((c) => ({
		...c,
		x: cx + (Math.random() - 0.5) * 80,
		y: cy + (Math.random() - 0.5) * 80,
		inDegree: degreeMap.get(c.id)?.in || 0,
		outDegree: degreeMap.get(c.id)?.out || 0,
	}));

	const ghosts: MicroNode[] = external.map((g) => ({
		id: g.id,
		name: g.name,
		chunk_type: g.chunk_type,
		line_count: 0,
		x: cx + (Math.random() - 0.5) * 120,
		y: cy + (Math.random() - 0.5) * 120,
		isGhost: true,
		ghostModuleId: g.module_id,
		ghostModulePath: g.module_path,
		direction: g.direction,
		inDegree: 0,
		outDegree: 0,
	}));

	const allNodes = [...nodes, ...ghosts];
	const nodeMap = new Map(allNodes.map((n) => [n.id, n]));

	// Build links
	const links: MicroLink[] = calls
		.map((c) => ({
			source: c.source,
			target: c.target,
			confidence: c.confidence,
			method: c.method,
			isExternal: !nodes.find((n) => n.id === c.source) || !nodes.find((n) => n.id === c.target),
			curvature: 0,
		}))
		.filter((l) => nodeMap.has(l.source) && nodeMap.has(l.target));
	assignEdgeCurvatures(links);

	return { nodes: allNodes, links, hotspotIds, disconnectedCount };
}

/** Create a compact, gravity-pulled d3 force simulation */
export function createForceSimulation(
	nodes: MicroNode[],
	links: MicroLink[],
	cx: number,
	cy: number,
	onTick: () => void,
): d3.Simulation<MicroNode, MicroLink> {
	const n = nodes.length;
	// Tight constellation: strong gravity, short links, soft repulsion, radial bound
	const chargeStrength = n > 40 ? -20 : n > 20 ? -35 : -50;
	const linkDist = n > 40 ? 40 : n > 20 ? 55 : 70;
	const radius = Math.min(300, 80 + n * 4); // radial constraint scales with node count

	return d3
		.forceSimulation(nodes)
		.force(
			"link",
			d3
				.forceLink<MicroNode, MicroLink>(links)
				.id((d) => d.id)
				.distance(linkDist)
				.strength(0.7),
		)
		.force("charge", d3.forceManyBody().strength(chargeStrength).distanceMax(200))
		.force(
			"collide",
			d3
				.forceCollide<MicroNode>()
				.radius((d) => collisionRadius(d) * 0.7) // tighter collision
				.strength(0.6),
		)
		.force("x", d3.forceX(cx).strength(0.15))
		.force("y", d3.forceY(cy).strength(0.15))
		.force("radial", d3.forceRadial(radius * 0.6, cx, cy).strength(0.05)) // soft radial bound
		.alphaMin(0.005)
		.on("tick", onTick);
}

function assignEdgeCurvatures(links: MicroLink[]) {
	const pairMap = new Map<string, MicroLink[]>();
	for (const link of links) {
		const key = [link.source, link.target].sort().join("::");
		if (!pairMap.has(key)) pairMap.set(key, []);
		pairMap.get(key)!.push(link);
	}
	for (const group of pairMap.values()) {
		if (group.length === 1) group[0].curvature = 0;
		else
			group.forEach((link, i) => {
				link.curvature = (i / (group.length - 1) - 0.5) * 2;
			});
	}
}

// ── Section Tree Layout (force-directed) ──────────────────────

export interface SectionLayoutNode {
	id: string;
	parentId: string | null;
	heading: string;
	depth: number;
	x: number;
	y: number;
	vx?: number;
	vy?: number;
	fx?: number | null;
	fy?: number | null;
	section: DocumentSectionItem;
}

export interface SectionLayoutEdge {
	source: string | SectionLayoutNode;
	target: string | SectionLayoutNode;
}

export interface SectionTreeLayout {
	nodes: SectionLayoutNode[];
	edges: SectionLayoutEdge[];
}

export function buildSectionTreeLayout(
	sections: DocumentSectionItem[],
	cx: number,
	cy: number,
): SectionTreeLayout {
	if (sections.length === 0) return { nodes: [], edges: [] };

	const nodes: SectionLayoutNode[] = sections.map((s) => ({
		id: s.id,
		parentId: s.parent_id,
		heading: s.heading,
		depth: s.depth,
		x: cx + (Math.random() - 0.5) * 80,
		y: cy + (Math.random() - 0.5) * 80,
		section: s,
	}));

	const nodeIdSet = new Set(nodes.map((n) => n.id));
	const edges: SectionLayoutEdge[] = [];
	for (const node of nodes) {
		if (node.parentId && nodeIdSet.has(node.parentId)) {
			edges.push({ source: node.parentId, target: node.id });
		}
	}

	return { nodes, edges };
}

/** Create force simulation for section tree — same compact style as call graph */
export function createSectionSimulation(
	nodes: SectionLayoutNode[],
	edges: SectionLayoutEdge[],
	cx: number,
	cy: number,
	onTick: () => void,
): d3.Simulation<SectionLayoutNode, SectionLayoutEdge> {
	const n = nodes.length;
	const chargeStrength = n > 40 ? -20 : n > 20 ? -35 : -50;
	const linkDist = n > 40 ? 40 : n > 20 ? 55 : 70;
	const radius = Math.min(300, 80 + n * 4);

	return d3
		.forceSimulation(nodes)
		.force(
			"link",
			d3
				.forceLink<SectionLayoutNode, SectionLayoutEdge>(edges)
				.id((d) => d.id)
				.distance(linkDist)
				.strength(0.7),
		)
		.force("charge", d3.forceManyBody().strength(chargeStrength).distanceMax(200))
		.force(
			"collide",
			d3
				.forceCollide<SectionLayoutNode>()
				.radius((d) => (pillWidth(d.heading) / 2) * 0.7 + 4)
				.strength(0.6),
		)
		.force("x", d3.forceX(cx).strength(0.15))
		.force("y", d3.forceY(cy).strength(0.15))
		.force("radial", d3.forceRadial(radius * 0.6, cx, cy).strength(0.05))
		.on("tick", onTick);
}

// ── SVG Edge Path Helpers ──────────────────────────────────────

export function edgeBezierPath(
	sx: number,
	sy: number,
	tx: number,
	ty: number,
	curvature: number,
): string {
	const dx = tx - sx,
		dy = ty - sy;
	const len = Math.sqrt(dx * dx + dy * dy) || 1;
	const perpX = -dy / len,
		perpY = dx / len;
	const offset = curvature ? curvature * Math.min(len * 0.25, 40) : len * 0.08;
	const mx = (sx + tx) / 2 + perpX * offset;
	const my = (sy + ty) / 2 + perpY * offset;
	return `M${sx},${sy} Q${mx},${my} ${tx},${ty}`;
}

export function sectionEdgePath(sx: number, sy: number, tx: number, ty: number): string {
	const my = (sy + ty) / 2;
	return `M${sx},${sy} C${sx},${my} ${tx},${my} ${tx},${ty}`;
}
