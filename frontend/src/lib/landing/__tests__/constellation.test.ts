/**
 * Unit tests for constellation.ts — deterministic geometry only.
 *
 * Strategy: inject fixed positions into `computeEdges` so every assertion is
 * reproducible. `buildParticles` / `buildConstellation` contain Math.random
 * and are tested only for shape/invariants, not for exact values.
 */

import { describe, it, expect } from "vitest";
import {
	computeEdges,
	euclideanDist,
	buildParticles,
	buildConstellation,
	edgesToConnections,
	DEFAULT_PARTICLE_COUNT,
	DEFAULT_SPHERE_RADIUS,
	DEFAULT_CONNECTION_DIST,
	type Vec3,
	type EdgeCandidate,
} from "../constellation.js";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Build a simple grid of positions on a line for deterministic tests. */
function linePositions(count: number, spacing: number): Vec3[] {
	return Array.from({ length: count }, (_, i) => ({
		x: i * spacing,
		y: 0,
		z: 0,
	}));
}

// ---------------------------------------------------------------------------
// euclideanDist
// ---------------------------------------------------------------------------

describe("euclideanDist", () => {
	it("returns 0 for identical points", () => {
		expect(euclideanDist({ x: 1, y: 2, z: 3 }, { x: 1, y: 2, z: 3 })).toBe(0);
	});

	it("returns correct distance for axis-aligned pair", () => {
		expect(euclideanDist({ x: 0, y: 0, z: 0 }, { x: 3, y: 4, z: 0 })).toBeCloseTo(5, 10);
	});

	it("returns correct 3-D diagonal distance", () => {
		// sqrt(1+1+1) = sqrt(3)
		expect(euclideanDist({ x: 0, y: 0, z: 0 }, { x: 1, y: 1, z: 1 })).toBeCloseTo(Math.sqrt(3), 10);
	});

	it("is symmetric", () => {
		const a = { x: 1, y: 2, z: 3 };
		const b = { x: 4, y: 6, z: 3 };
		expect(euclideanDist(a, b)).toBeCloseTo(euclideanDist(b, a), 10);
	});
});

// ---------------------------------------------------------------------------
// computeEdges — deterministic given positions
// ---------------------------------------------------------------------------

describe("computeEdges", () => {
	it("returns empty array for empty positions", () => {
		expect(computeEdges([], 7, 50)).toEqual([]);
	});

	it("returns empty array for single position (no pairs possible)", () => {
		expect(computeEdges([{ x: 0, y: 0, z: 0 }], 7, 50)).toEqual([]);
	});

	it("includes a pair whose distance is strictly less than threshold", () => {
		const positions: Vec3[] = [
			{ x: 0, y: 0, z: 0 },
			{ x: 3, y: 0, z: 0 }, // dist = 3, threshold = 7 → include
		];
		const edges = computeEdges(positions, 7, 50);
		expect(edges).toHaveLength(1);
		expect(edges[0]).toMatchObject({ a: 0, b: 1 });
		expect(edges[0].dist).toBeCloseTo(3, 10);
	});

	it("excludes a pair whose distance equals the threshold", () => {
		const positions: Vec3[] = [
			{ x: 0, y: 0, z: 0 },
			{ x: 7, y: 0, z: 0 }, // dist = 7 exactly → NOT < 7
		];
		expect(computeEdges(positions, 7, 50)).toHaveLength(0);
	});

	it("excludes a pair whose distance exceeds the threshold", () => {
		const positions: Vec3[] = [
			{ x: 0, y: 0, z: 0 },
			{ x: 10, y: 0, z: 0 },
		];
		expect(computeEdges(positions, 7, 50)).toHaveLength(0);
	});

	it("produces only a-b pairs with a < b (no duplicates, no self-edges)", () => {
		// 5 positions spaced 1 apart — all pairs within dist=7
		const positions = linePositions(5, 1);
		const edges = computeEdges(positions, 7, 50);

		for (const e of edges) {
			expect(e.a).toBeLessThan(e.b); // always a < b
		}

		// Check dedup: no pair appears more than once
		const seen = new Set<string>();
		for (const e of edges) {
			const key = `${e.a}-${e.b}`;
			expect(seen.has(key)).toBe(false);
			seen.add(key);
		}
	});

	it("no self-edges (a === b) are emitted", () => {
		const positions = linePositions(5, 1);
		const edges = computeEdges(positions, 7, 50);
		for (const e of edges) {
			expect(e.a).not.toBe(e.b);
		}
	});

	it("returns edges sorted by distance ascending (nearest first)", () => {
		// 4 positions: 0→1 dist=1, 0→2 dist=2, 1→2 dist=1, etc.
		const positions = linePositions(4, 1);
		const edges = computeEdges(positions, 7, 50);

		for (let i = 1; i < edges.length; i++) {
			expect(edges[i].dist).toBeGreaterThanOrEqual(edges[i - 1].dist);
		}
	});

	it("respects maxSlots — returns at most maxSlots edges", () => {
		// 10 positions spaced 1 apart → lots of pairs within dist=10
		const positions = linePositions(10, 1);
		const edges = computeEdges(positions, 10, 5);
		expect(edges.length).toBeLessThanOrEqual(5);
	});

	it("selects the nearest pairs when maxSlots constrains output", () => {
		// 4 positions on a line spaced 1 apart: pair distances are 1, 1, 1, 2, 2, 3
		// With maxSlots=3, we expect the three dist=1 adjacent pairs.
		const positions = linePositions(4, 1);
		const edges = computeEdges(positions, 10, 3);
		expect(edges).toHaveLength(3);
		for (const e of edges) {
			expect(e.dist).toBeCloseTo(1, 10);
		}
	});

	it("skips pairs in existingPairs", () => {
		const positions: Vec3[] = [
			{ x: 0, y: 0, z: 0 },
			{ x: 1, y: 0, z: 0 },
			{ x: 2, y: 0, z: 0 },
		];
		const existing = new Set(["0-1"]);
		const edges = computeEdges(positions, 5, 50, existing);

		const keys = edges.map((e) => `${e.a}-${e.b}`);
		expect(keys).not.toContain("0-1");
		// 0-2 and 1-2 should still be present
		expect(keys).toContain("0-2");
		expect(keys).toContain("1-2");
	});

	it("picks the exact nearest pair among multiple candidates", () => {
		// 4 points on a line: 0, 2, 5, 6.5
		// All pair distances (threshold=7):
		//   0↔1: 2,  0↔2: 5,  0↔3: 6.5,  1↔2: 3,  1↔3: 4.5,  2↔3: 1.5  ← nearest
		const positions: Vec3[] = [
			{ x: 0, y: 0, z: 0 },
			{ x: 2, y: 0, z: 0 },
			{ x: 5, y: 0, z: 0 },
			{ x: 6.5, y: 0, z: 0 },
		];
		const edges = computeEdges(positions, 7, 1);
		expect(edges).toHaveLength(1);
		// Nearest pair is 2↔3 at dist=1.5
		expect(edges[0]).toMatchObject({ a: 2, b: 3 });
		expect(edges[0].dist).toBeCloseTo(1.5, 10);
	});
});

// ---------------------------------------------------------------------------
// edgesToConnections
// ---------------------------------------------------------------------------

describe("edgesToConnections", () => {
	it("preserves a and b indices", () => {
		const candidates: EdgeCandidate[] = [{ a: 2, b: 5, dist: 3.14 }];
		const conns = edgesToConnections(candidates);
		expect(conns).toHaveLength(1);
		expect(conns[0].a).toBe(2);
		expect(conns[0].b).toBe(5);
	});

	it("starts each connection at age 0", () => {
		const candidates: EdgeCandidate[] = [{ a: 0, b: 1, dist: 1 }];
		const conns = edgesToConnections(candidates);
		expect(conns[0].age).toBe(0);
	});

	it("assigns lifespan in range [3, 8)", () => {
		const candidates: EdgeCandidate[] = Array.from({ length: 200 }, (_, i) => ({
			a: i,
			b: i + 1,
			dist: 1,
		}));
		const conns = edgesToConnections(candidates);
		for (const c of conns) {
			expect(c.lifespan).toBeGreaterThanOrEqual(3);
			expect(c.lifespan).toBeLessThan(8);
		}
	});

	it("returns empty array for empty input", () => {
		expect(edgesToConnections([])).toEqual([]);
	});
});

// ---------------------------------------------------------------------------
// buildParticles — shape/invariant tests (Math.random inside)
// ---------------------------------------------------------------------------

describe("buildParticles", () => {
	it("returns the requested number of particles", () => {
		expect(buildParticles(10)).toHaveLength(10);
		expect(buildParticles(0)).toHaveLength(0);
	});

	it("places all particles inside the sphere boundary", () => {
		const r = DEFAULT_SPHERE_RADIUS;
		const particles = buildParticles(200, r);
		for (const p of particles) {
			const dist = Math.sqrt(p.x * p.x + p.y * p.y + p.z * p.z);
			expect(dist).toBeLessThanOrEqual(r + 1e-9);
		}
	});

	it("assigns positive radius to every particle", () => {
		const particles = buildParticles(50);
		for (const p of particles) {
			expect(p.radius).toBeGreaterThan(0);
		}
	});

	it("assigns colorIdx in [0, colorCount)", () => {
		const colorCount = 5;
		const particles = buildParticles(100, DEFAULT_SPHERE_RADIUS, 0.004, colorCount);
		for (const p of particles) {
			expect(p.colorIdx).toBeGreaterThanOrEqual(0);
			expect(p.colorIdx).toBeLessThan(colorCount);
			expect(Number.isInteger(p.colorIdx)).toBe(true);
		}
	});

	it("assigns phase in [0, 2π)", () => {
		const particles = buildParticles(100);
		for (const p of particles) {
			expect(p.phase).toBeGreaterThanOrEqual(0);
			expect(p.phase).toBeLessThan(Math.PI * 2 + 1e-9);
		}
	});
});

// ---------------------------------------------------------------------------
// buildConstellation — integration / shape tests
// ---------------------------------------------------------------------------

describe("buildConstellation", () => {
	it("returns particle count matching opts", () => {
		const { particles } = buildConstellation({ particleCount: 20 });
		expect(particles).toHaveLength(20);
	});

	it("returns empty constellation for zero particles", () => {
		const { particles, connections } = buildConstellation({ particleCount: 0 });
		expect(particles).toHaveLength(0);
		expect(connections).toHaveLength(0);
	});

	it("connections respect maxConnections cap", () => {
		const { connections } = buildConstellation({
			particleCount: 50,
			maxConnections: 10,
		});
		expect(connections.length).toBeLessThanOrEqual(10);
	});

	it("all connection indices are valid particle indices", () => {
		const count = 30;
		const { connections } = buildConstellation({ particleCount: count });
		for (const c of connections) {
			expect(c.a).toBeGreaterThanOrEqual(0);
			expect(c.a).toBeLessThan(count);
			expect(c.b).toBeGreaterThanOrEqual(0);
			expect(c.b).toBeLessThan(count);
		}
	});

	it("uses default constants when no opts are provided", () => {
		const { particles } = buildConstellation();
		expect(particles).toHaveLength(DEFAULT_PARTICLE_COUNT);
	});

	it("particles stay within custom sphereRadius", () => {
		const r = 5;
		const { particles } = buildConstellation({ particleCount: 100, sphereRadius: r });
		for (const p of particles) {
			const dist = Math.sqrt(p.x * p.x + p.y * p.y + p.z * p.z);
			expect(dist).toBeLessThanOrEqual(r + 1e-9);
		}
	});

	it("connected pairs are within connectionDist of each other", () => {
		const dist = DEFAULT_CONNECTION_DIST;
		const { particles, connections } = buildConstellation({ particleCount: 60 });
		for (const c of connections) {
			const d = euclideanDist(particles[c.a], particles[c.b]);
			expect(d).toBeLessThan(dist);
		}
	});
});
