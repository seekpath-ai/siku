/**
 * Locating a stored quote inside the pdf.js text layer.
 *
 * The quote in a citation comes from OUR indexed text (`chunks.content`), and
 * that text is not byte-identical to what pdf.js hands back for the same page:
 *
 *  - the chunker joins line-final hyphenation ("depen-\ndencies" →
 *    "dependencies"), and pdfium even reports that break as U+0002 while the
 *    text layer keeps a glyph that pdf.js renders as a hyphen;
 *  - column layout inserts spaces the words never had, and hard line breaks
 *    split sentences mid-phrase;
 *  - small-caps / typeset headings change case.
 *
 * Matching on raw characters therefore fails for a large share of real quotes —
 * the reader jumped to the page and then highlighted nothing. This module
 * builds a normalised view (no whitespace, no hyphens, folded case) with an
 * index map back to the raw string, and tries the whole quote first, then its
 * head, so a partially-mangled quote still anchors on the right passage.
 */

/** Whitespace, pdfium's hyphen marker (U+0002), soft and typographic hyphens. */
// eslint-disable-next-line no-control-regex
const STRIP = /[\s\u0002\u00AD\u2010\u2011\u2013-]/;

export interface Normalised {
  /** The folded text used for comparison. */
  norm: string;
  /** `map[i]` = index in the ORIGINAL string of the character that produced
   *  `norm[i]`. Lets a match in `norm` address the raw text for highlight
   *  rectangles. */
  map: number[];
}

/** Fold a string for comparison, keeping a map back to the raw characters. */
export function normaliseForMatch(raw: string): Normalised {
  let norm = '';
  const map: number[] = [];
  for (let i = 0; i < raw.length; i++) {
    const ch = raw[i];
    if (STRIP.test(ch)) continue;
    for (const c of ch.toLowerCase()) {
      norm += c;
      map.push(i);
    }
  }
  return { norm, map };
}

/** Needle forms for one quote, longest first: the whole quote, then its head.
 *  A quote that cannot be found whole is still worth anchoring on its start. */
export function quoteNeedles(exact: string): string[] {
  const { norm } = normaliseForMatch(exact);
  const out = [norm];
  for (const len of [40, 24]) {
    if (norm.length > len) out.push(norm.slice(0, len));
  }
  return out.filter((n, i) => n.length >= 8 && out.indexOf(n) === i);
}

/** How well a page matches: 3 = whole quote, 2 = its head, 1 = short head,
 *  0 = nothing. The page scan uses it so a weak match never beats a strong one
 *  found a page later. */
export function matchStrength(pageText: string, needles: string[]): number {
  if (needles.length === 0) return 0;
  const { norm } = normaliseForMatch(pageText);
  for (let k = 0; k < needles.length; k++) {
    if (norm.includes(needles[k])) return k === 0 ? 3 : k === 1 ? 2 : 1;
  }
  return 0;
}

export interface RawRange {
  /** Start index in the RAW page text (inclusive). */
  start: number;
  /** End index in the RAW page text (exclusive). */
  end: number;
  /** Length of the needle that matched, for diagnostics/tests. */
  matchedLength: number;
}

/** Locate the quote inside `pageText`; raw indices suitable for rect mapping. */
export function findQuote(pageText: string, needles: string[]): RawRange | null {
  if (needles.length === 0) return null;
  const { norm, map } = normaliseForMatch(pageText);
  if (map.length === 0) return null;
  for (const needle of needles) {
    const pos = norm.indexOf(needle);
    if (pos < 0) continue;
    const start = map[pos];
    const end = map[Math.min(pos + needle.length - 1, map.length - 1)] + 1;
    return { start, end, matchedLength: needle.length };
  }
  return null;
}
