export interface LanguageChipEntry {
  label: string;
  /** null = plain-text segment = the page's current language. */
  href: string | null;
}

export interface LanguageExtract {
  chips: LanguageChipEntry[] | null;
  markdown: string;
}

const LINE = /^\*\*Language:\*\*\s*(.+)$/;
const LINK = /^\[([^\]]+)\]\(([^)\s]+)\)$/;

/** Detect a `**Language:** …` switcher line in the first 10 non-empty
 * lines, lift it out of the prose (D2 LanguageChip). Segments are
 * `|`-separated; markdown links become linked chips, plain text becomes
 * the active (current-language) chip. Not found → render untouched. */
export function extractLanguageLine(markdown: string): LanguageExtract {
  const lines = markdown.split("\n");
  let seen = 0;
  for (let i = 0; i < lines.length && seen < 10; i++) {
    const line = lines[i].trim();
    if (line === "") continue;
    seen++;
    const m = LINE.exec(line);
    if (!m) continue;
    const chips: LanguageChipEntry[] = [];
    for (const seg of m[1].split("|")) {
      const s = seg.trim();
      if (s === "") continue;
      const lm = LINK.exec(s);
      chips.push(lm ? { label: lm[1], href: lm[2] } : { label: s, href: null });
    }
    if (chips.length === 0) return { chips: null, markdown };
    const out = lines.slice();
    out.splice(i, 1);
    return { chips, markdown: out.join("\n") };
  }
  return { chips: null, markdown };
}
