import { describe, it, expect } from "vitest";
import { resolveNav, prevNext, resolveVersionEntry } from "../nav.js";
import type { DocsNav, DocsVersionEntry } from "$lib/api/docs.js";

const nav: DocsNav = {
  description: "A test corpus",
  groups: [
    {
      title: "Guide",
      pages: [
        { title: "Setup", path: "guide/setup.md", description: "" },
        { title: "Advanced", path: "guide/advanced.md", description: "deep" },
      ],
    },
    { title: "Reference", pages: [{ title: "API", path: "api.md", description: "" }] },
  ],
};

describe("resolveNav", () => {
  it("resolves nav paths against index dir (pull corpus)", () => {
    const r = resolveNav(nav, "docs", "docs/README.md");
    expect(r.flat.map((p) => p.fullKey)).toEqual([
      "docs/guide/setup.md",
      "docs/guide/advanced.md",
      "docs/api.md",
    ]);
    expect(r.flat[0].urlPath).toBe("guide/setup");
    expect(r.groups[1].title).toBe("Reference");
  });
  it("push corpus keeps keys verbatim", () => {
    const r = resolveNav(nav, "", "index.md");
    expect(r.flat[2].fullKey).toBe("api.md");
  });
});

describe("prevNext", () => {
  const flat = resolveNav(nav, "", "index.md").flat;
  it("middle page has both", () => {
    const { prev, next } = prevNext(flat, "guide/advanced.md", "index.md");
    expect(prev?.fullKey).toBe("guide/setup.md");
    expect(next?.fullKey).toBe("api.md");
  });
  it("index page: no prev, next = first nav page", () => {
    const { prev, next } = prevNext(flat, "index.md", "index.md");
    expect(prev).toBeUndefined();
    expect(next?.fullKey).toBe("guide/setup.md");
  });
  it("orphan page: neither", () => {
    const { prev, next } = prevNext(flat, "orphan.md", "index.md");
    expect(prev).toBeUndefined();
    expect(next).toBeUndefined();
  });
  it("last page has no next", () => {
    expect(prevNext(flat, "api.md", "index.md").next).toBeUndefined();
  });
});

describe("resolveVersionEntry", () => {
  const mk = (v: Partial<DocsVersionEntry>): DocsVersionEntry => ({
    version: "v1",
    sha: "a".repeat(40),
    is_tagged: false,
    is_latest: false,
    ingested_at: "2026-07-01T00:00:00Z",
    derive_status: "complete",
    index: "index.md",
    ...v,
  });
  const versions = [
    mk({ version: "v2", sha: "b".repeat(40), is_latest: true, ingested_at: "2026-07-02T00:00:00Z" }),
    mk({ version: "v1" }),
  ];
  it("latest selector", () => expect(resolveVersionEntry(versions, "latest")?.version).toBe("v2"));
  it("exact version", () => expect(resolveVersionEntry(versions, "v1")?.version).toBe("v1"));
  it("sha prefix ≥7", () =>
    expect(resolveVersionEntry(versions, "bbbbbbb")?.version).toBe("v2"));
  it("short prefix rejected", () =>
    expect(resolveVersionEntry(versions, "bbb")).toBeUndefined());
  it("unknown", () => expect(resolveVersionEntry(versions, "nope")).toBeUndefined());
});
