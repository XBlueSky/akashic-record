import { describe, it, expect } from "vitest";
import { normalizeLang } from "../highlight.js";

describe("normalizeLang", () => {
	it("passes through subset languages", () => {
		for (const l of [
			"c",
			"cpp",
			"rust",
			"python",
			"typescript",
			"javascript",
			"json",
			"toml",
			"yaml",
			"bash",
			"cmake",
			"make",
			"sql",
		]) {
			expect(normalizeLang(l)).toBe(l);
		}
	});
	it("maps aliases", () => {
		expect(normalizeLang("ts")).toBe("typescript");
		expect(normalizeLang("js")).toBe("javascript");
		expect(normalizeLang("sh")).toBe("bash");
		expect(normalizeLang("shell")).toBe("bash");
		expect(normalizeLang("c++")).toBe("cpp");
		expect(normalizeLang("yml")).toBe("yaml");
		expect(normalizeLang("mk")).toBe("make");
		expect(normalizeLang("console")).toBe("bash");
	});
	it("unknown → null (stay plain)", () => {
		expect(normalizeLang("cobol")).toBeNull();
		expect(normalizeLang("")).toBeNull();
	});
	it("case-insensitive", () => expect(normalizeLang("CPP")).toBe("cpp"));
});
