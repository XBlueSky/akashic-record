import { describe, it, expect } from "vitest";
import { extractLanguageLine } from "../language.js";

describe("extractLanguageLine", () => {
  it("extracts links and plain current-language segment", () => {
    const src = "# Title\n\n**Language:** English | [繁體中文](zh-TW/guide.md)\n\nbody";
    const r = extractLanguageLine(src);
    expect(r.chips).toEqual([
      { label: "English", href: null },
      { label: "繁體中文", href: "zh-TW/guide.md" },
    ]);
    expect(r.markdown).not.toContain("**Language:**");
    expect(r.markdown).toContain("body");
  });
  it("no language line → chips null, markdown untouched", () => {
    const src = "# Title\n\nbody";
    expect(extractLanguageLine(src)).toEqual({ chips: null, markdown: src });
  });
  it("only scans the first 10 non-empty lines", () => {
    const filler = Array.from({ length: 12 }, (_, i) => `line ${i}`).join("\n\n");
    const src = `${filler}\n\n**Language:** [EN](a.md)`;
    expect(extractLanguageLine(src).chips).toBeNull();
  });
});
