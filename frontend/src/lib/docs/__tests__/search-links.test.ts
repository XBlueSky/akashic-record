import { describe, it, expect } from "vitest";
import { docsHitHref, type DocsIndexInfo } from "../search-links.js";

const push: DocsIndexInfo = { indexPath: "index.md", indexDir: "" };
const pull: DocsIndexInfo = { indexPath: "docs/README.md", indexDir: "docs" };

describe("docsHitHref", () => {
  it("page with anchor (push corpus)", () =>
    expect(
      docsHitHref({ repo: "acme", path: "guide/setup.md", anchor: "quick-start" }, push),
    ).toBe("/docs/acme/latest/guide/setup#quick-start"));
  it("index page hit", () =>
    expect(docsHitHref({ repo: "acme", path: "index.md", anchor: "" }, push)).toBe(
      "/docs/acme/latest",
    ));
  it("pull corpus strips prefix", () =>
    expect(
      docsHitHref({ repo: "libx", path: "docs/guide/setup.md", anchor: "a" }, pull),
    ).toBe("/docs/libx/latest/guide/setup#a"));
  it("unknown repo info → repo root fallback", () =>
    expect(docsHitHref({ repo: "libx", path: "docs/x.md", anchor: "" }, undefined)).toBe(
      "/docs/libx/latest",
    ));
});
