import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { resolve, dirname } from "node:path";
import GithubSlugger from "github-slugger";

interface SingleCase {
  input: string;
  expected: string;
}
interface SequenceCase {
  inputs: string[];
  expected: string[];
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const goldenPath = resolve(__dirname, "../../../../../backend/crates/akashic-domain/tests/fixtures/corpus/slug_golden.json");

const golden = JSON.parse(readFileSync(goldenPath, "utf8")) as {
  single: SingleCase[];
  sequence: SequenceCase[];
};

describe("slug golden alignment with server github_slug/SlugCounter", () => {
  it("single slugs match", () => {
    for (const c of golden.single) {
      expect(new GithubSlugger().slug(c.input), c.input).toBe(c.expected);
    }
  });
  it("dedup sequences match", () => {
    for (const c of golden.sequence) {
      const s = new GithubSlugger();
      expect(c.inputs.map((i) => s.slug(i))).toEqual(c.expected);
    }
  });
});
