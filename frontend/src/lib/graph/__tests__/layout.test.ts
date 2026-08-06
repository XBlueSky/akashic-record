import { describe, it, expect } from "vitest";
import {
	buildCallGraphLayout,
	buildSectionTreeLayout,
	edgeBezierPath,
	sectionEdgePath,
} from "../layout";
import type { ChunkNode, CallEdge, GhostNode, DocumentSectionItem } from "$lib/types";

const chunk = (id: string, over: Partial<ChunkNode> = {}): ChunkNode =>
	({
		id,
		name: id,
		chunk_type: "function",
		line_count: 1,
		...over,
	}) as ChunkNode;
const call = (source: string, target: string, over: Partial<CallEdge> = {}): CallEdge =>
	({
		source,
		target,
		confidence: 1,
		method: "static",
		...over,
	}) as CallEdge;

describe("buildCallGraphLayout", () => {
	it("keeps only connected chunks and counts disconnected", () => {
		const chunks = [chunk("a"), chunk("b"), chunk("c")]; // c disconnected
		const calls = [call("a", "b")];
		const r = buildCallGraphLayout(chunks, calls, [], 0, 0);
		const ids = r.nodes
			.filter((n) => !n.isGhost)
			.map((n) => n.id)
			.sort();
		expect(ids).toEqual(["a", "b"]);
		expect(r.disconnectedCount).toBe(1);
	});

	it("computes in/out degree and picks top-3 hotspots by in-degree", () => {
		const chunks = ["a", "b", "c", "d", "hub"].map((id) => chunk(id));
		const calls = [call("a", "hub"), call("b", "hub"), call("c", "hub"), call("hub", "d")];
		const r = buildCallGraphLayout(chunks, calls, [], 0, 0);
		const hub = r.nodes.find((n) => n.id === "hub")!;
		expect(hub.inDegree).toBe(3);
		expect(hub.outDegree).toBe(1);
		expect(r.hotspotIds.has("hub")).toBe(true);
		expect(r.hotspotIds.size).toBeLessThanOrEqual(3);
	});

	it("builds ghost nodes from external and only links between existing nodes", () => {
		const chunks = [chunk("a"), chunk("b")];
		const ext: GhostNode[] = [
			{
				id: "g1",
				name: "ext",
				chunk_type: "function",
				module_id: "m",
				module_path: "p/m",
				direction: "callee",
			} as GhostNode,
		];
		const calls = [call("a", "b"), call("a", "g1"), call("a", "missing")];
		const r = buildCallGraphLayout(chunks, calls, ext, 0, 0);
		expect(r.nodes.some((n) => n.isGhost && n.id === "g1")).toBe(true);
		// a→b and a→g1 kept (both endpoints exist); a→missing dropped
		expect(r.links).toHaveLength(2);
	});

	it("assigns curvature 0 to a lone edge and spreads multi-edges", () => {
		const chunks = [chunk("a"), chunk("b")];
		const r1 = buildCallGraphLayout(chunks, [call("a", "b")], [], 0, 0);
		expect(r1.links[0].curvature).toBe(0);
		const r2 = buildCallGraphLayout(
			chunks,
			[call("a", "b"), call("a", "b", { method: "dyn" })],
			[],
			0,
			0,
		);
		expect(new Set(r2.links.map((l) => l.curvature)).size).toBeGreaterThan(1);
	});
});

describe("buildSectionTreeLayout", () => {
	const sec = (id: string, parent_id: string | null, depth: number): DocumentSectionItem =>
		({ id, parent_id, depth, heading: id }) as DocumentSectionItem;

	it("empty input → empty layout", () => {
		expect(buildSectionTreeLayout([], 0, 0)).toEqual({ nodes: [], edges: [] });
	});

	it("creates parent→child edges only when parent present", () => {
		const r = buildSectionTreeLayout(
			[sec("root", null, 0), sec("child", "root", 1), sec("orphan", "ghost", 1)],
			0,
			0,
		);
		expect(r.nodes).toHaveLength(3);
		expect(r.edges).toEqual([{ source: "root", target: "child" }]);
	});
});

describe("edge path geometry", () => {
	it("edgeBezierPath returns a quadratic path through a control point", () => {
		const p = edgeBezierPath(0, 0, 100, 0, 0);
		expect(p).toMatch(/^M0,0 Q[\d.-]+,[\d.-]+ 100,0$/);
	});
	it("sectionEdgePath returns a cubic vertical path", () => {
		const p = sectionEdgePath(0, 0, 0, 100);
		expect(p).toMatch(/^M0,0 C/);
	});
});
