// ── Note outline headings ───────────────────────────────────────────────────

export interface HeadingItem {
  level: number;
  /** Raw heading text (inline markdown markers kept; use normHeading to
   *  compare against rendered text). */
  text: string;
  /** Char offset of the heading line's start in the source document — the
   *  jump target for edit/source/split views. */
  offset: number;
}

/** Scan ATX headings (`#`–`######`) from raw markdown, skipping fenced code
 *  blocks (same fence rules as rewritePasswordTokens in passwordToken.ts).
 *  Setext headings (===/--- underlines) are deliberately not detected — rare
 *  in this app. Line scan (not the lezer tree) so reading view and edit view
 *  share one data source. */
export function scanHeadings(content: string): HeadingItem[] {
  const out: HeadingItem[] = [];
  let fence: { ch: string; len: number } | null = null;
  let offset = 0;
  for (const line of content.split('\n')) {
    const fm = /^ {0,3}(`{3,}|~{3,})/.exec(line);
    if (fm) {
      const ch = fm[1][0];
      if (fence === null) fence = { ch, len: fm[1].length };
      else if (ch === fence.ch && fm[1].length >= fence.len) fence = null;
    } else if (!fence) {
      // Closing sequence (`# Title ##`) requires whitespace before it, per
      // CommonMark — so `# C#` keeps its `#`.
      const hm = /^ {0,3}(#{1,6})[ \t]+(.+?)(?:[ \t]+#+)?[ \t]*$/.exec(line);
      if (hm) out.push({ level: hm[1].length, text: hm[2], offset });
    }
    offset += line.length + 1;
  }
  return out;
}

/** Normalize a heading for raw-source ↔ rendered-DOM comparison: strips
 *  whitespace and inline markdown punctuation. Known limitation: headings
 *  containing `!pw[...]` tokens or images render differently than the raw
 *  text and will not match — accepted, both are absurd in a heading. */
export function normHeading(s: string): string {
  return s.replace(/[\s*_~`#>|[\]().\\-]/g, '');
}

/** Align outline items (raw scan) with rendered heading elements in reading
 *  view: forward-only text matching with one-use-per-element. Embedded notes
 *  (![[...]]) may inject extra headings into the DOM; matching by normalized
 *  text tolerates them as long as they don't duplicate a heading text that
 *  appears earlier in the note. Returns one element (or null) per item. */
export function mapHeadingsToEls(container: HTMLElement, items: HeadingItem[]): (HTMLElement | null)[] {
  const els = Array.from(container.querySelectorAll('h1,h2,h3,h4,h5,h6')) as HTMLElement[];
  const used = new Uint8Array(els.length);
  const norms = els.map((el) => normHeading(el.textContent ?? ''));
  return items.map((it) => {
    const t = normHeading(it.text);
    for (let i = 0; i < els.length; i++) {
      if (!used[i] && norms[i] === t) {
        used[i] = 1;
        return els[i];
      }
    }
    return null;
  });
}
