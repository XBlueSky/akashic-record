import { describe, it, expect } from "vitest";
import {
  fileDir,
  resolveRelative,
  fullKeyToUrlPath,
  urlPathToFullKey,
  encodeUrlPath,
  splitAnchor,
  isInCorpus,
} from "../paths.js";

describe("fileDir", () => {
  it("root file has empty dir", () => expect(fileDir("index.md")).toBe(""));
  it("nested", () => expect(fileDir("docs/guide/setup.md")).toBe("docs/guide"));
});

describe("resolveRelative", () => {
  it("sibling", () =>
    expect(resolveRelative("guide", "advanced.md")).toEqual({ fullKey: "guide/advanced.md", escaped: false }));
  it("climb", () =>
    expect(resolveRelative("guide", "../index.md")).toEqual({ fullKey: "index.md", escaped: false }));
  it("dot segment", () =>
    expect(resolveRelative("guide", "./sub/x.md")).toEqual({ fullKey: "guide/sub/x.md", escaped: false }));
  it("escape above root flags escaped", () =>
    expect(resolveRelative("", "../outside.md").escaped).toBe(true));
  it("pull prefix climb stays in corpus", () =>
    expect(resolveRelative("docs/guide", "../README.md")).toEqual({ fullKey: "docs/README.md", escaped: false }));
  it("pull corpus escape to out-of-bounds (regression: escaped flag insufficient)", () => {
    const res = resolveRelative("docs/guide", "../../CONTRIBUTING.md");
    expect(res).toEqual({ fullKey: "CONTRIBUTING.md", escaped: false });
    // Caller MUST also check isInCorpus(fullKey, indexDir) — escaped alone is insufficient.
    expect(isInCorpus("CONTRIBUTING.md", "docs")).toBe(false);
  });
});

describe("url mapping", () => {
  it("push corpus index → empty path", () =>
    expect(fullKeyToUrlPath("index.md", "", "index.md")).toBe(""));
  it("push corpus page", () =>
    expect(fullKeyToUrlPath("guide/setup.md", "", "index.md")).toBe("guide/setup"));
  it("pull corpus strips prefix", () =>
    expect(fullKeyToUrlPath("docs/guide/setup.md", "docs", "docs/README.md")).toBe("guide/setup"));
  it("pull corpus index → empty path", () =>
    expect(fullKeyToUrlPath("docs/README.md", "docs", "docs/README.md")).toBe(""));
  it("roundtrip push", () =>
    expect(urlPathToFullKey("guide/setup", "", "index.md")).toBe("guide/setup.md"));
  it("roundtrip pull", () =>
    expect(urlPathToFullKey("guide/setup", "docs", "docs/README.md")).toBe("docs/guide/setup.md"));
  it("empty url path → index full key", () =>
    expect(urlPathToFullKey("", "docs", "docs/README.md")).toBe("docs/README.md"));
});

describe("encodeUrlPath / splitAnchor / isInCorpus", () => {
  it("encodes per segment, keeps slashes", () =>
    expect(encodeUrlPath("zh-TW/指南.md")).toBe("zh-TW/%E6%8C%87%E5%8D%97.md"));
  it("splits anchor", () =>
    expect(splitAnchor("a.md#sec")).toEqual({ path: "a.md", anchor: "#sec" }));
  it("no anchor", () => expect(splitAnchor("a.md")).toEqual({ path: "a.md", anchor: "" }));
  it("in corpus with empty prefix", () => expect(isInCorpus("x.md", "")).toBe(true));
  it("in corpus with prefix", () => expect(isInCorpus("docs/x.md", "docs")).toBe(true));
  it("sibling of prefix is out", () => expect(isInCorpus("docsother/x.md", "docs")).toBe(false));
});
