import { describe, it, expect } from "vitest";
import {
	isClassType,
	isEnumType,
	getColorBucket,
	getChunkColors,
	getSectionColors,
	microNodeLabel,
	pillWidth,
	ASTRAL_COLORS,
	SECTION_DEPTH_COLORS,
} from "../types";
import type { MicroNode } from "../types";

const node = (over: Partial<MicroNode> = {}): MicroNode => ({
	id: "n",
	name: "thing",
	chunk_type: "function",
	line_count: 1,
	x: 0,
	y: 0,
	inDegree: 0,
	outDegree: 0,
	...over,
});

describe("type helpers", () => {
	it("isClassType / isEnumType buckets", () => {
		expect(isClassType("class")).toBe(true);
		expect(isClassType("struct")).toBe(true);
		expect(isClassType("function")).toBe(false);
		expect(isEnumType("enum")).toBe(true);
		expect(isEnumType("config")).toBe(true);
		expect(isEnumType("function")).toBe(false);
	});

	it("getColorBucket maps types and ghost", () => {
		expect(getColorBucket(node({ chunk_type: "method" }))).toBe("function");
		expect(getColorBucket(node({ chunk_type: "class" }))).toBe("class");
		expect(getColorBucket(node({ chunk_type: "enum" }))).toBe("enum");
		expect(getColorBucket(node({ chunk_type: "unknown" }))).toBe("function"); // fallback
		expect(getColorBucket(node({ isGhost: true }))).toBe("ghost");
	});

	it("getChunkColors returns the Astral palette for the bucket", () => {
		expect(getChunkColors(node({ chunk_type: "class" }))).toEqual(ASTRAL_COLORS.class);
		expect(getChunkColors(node({ isGhost: true }))).toEqual(ASTRAL_COLORS.ghost);
	});

	it("getSectionColors clamps depth to palette length", () => {
		expect(getSectionColors(0, false)).toEqual(SECTION_DEPTH_COLORS[0]);
		expect(getSectionColors(99, false)).toEqual(
			SECTION_DEPTH_COLORS[SECTION_DEPTH_COLORS.length - 1],
		);
	});

	it("microNodeLabel truncates long names with ellipsis", () => {
		expect(microNodeLabel(node({ name: "short" }))).toBe("short");
		const long = "x".repeat(50);
		const out = microNodeLabel(node({ name: long }), 36);
		expect(out.endsWith("…")).toBe(true);
		expect(out.length).toBe(37); // 36 chars + ellipsis
	});

	it("microNodeLabel uses ghost module path basename", () => {
		const out = microNodeLabel(node({ isGhost: true, name: "foo", ghostModulePath: "a/b/mod.rs" }));
		expect(out).toBe("mod.rs/foo");
	});

	it("pillWidth respects min width and scales with label length", () => {
		expect(pillWidth("a", 56)).toBe(56); // min floor
		expect(pillWidth("x".repeat(20))).toBe(20 * 7 + 20);
	});
});
