/** Shared helpers for the pet panel's evidence citations ([^n] markers plus a
 *  trailing ```evidence JSON block) and for turning them into portable
 *  Markdown footnotes with in-app deep links when saving to a note. */

export interface EvidenceEntry {
  page?: number;
  exact: string;
}

/** JSON.parse that tolerates raw newlines/tabs INSIDE string literals — LLMs
 *  regularly emit them in `exact` quotes even though strict JSON forbids
 *  control characters there. Falls back to strict semantics on any other
 *  error (returns undefined). */
function tolerantJsonParse(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    // Escape control characters only inside quoted strings.
    const sanitized = text.replace(/"(?:[^"\\]|\\.)*"/g, (s) =>
      s.replace(/\n/g, '\\n').replace(/\r/g, '\\r').replace(/\t/g, '\\t'));
    try {
      return JSON.parse(sanitized);
    } catch {
      return undefined;
    }
  }
}

/** Strip the trailing ```evidence JSON block off an assistant reply and
 *  parse it into a citation-number → evidence map. An UNTERMINATED block
 *  (still streaming in — the closing fence hasn't arrived yet) is also
 *  stripped: the block is metadata by contract and always comes last, so
 *  cutting from its start fence to the end is safe and avoids flashing the
 *  raw JSON as a code block during streaming. */
export function parseEvidence(content: string): { clean: string; evidence: Map<number, EvidenceEntry> } {
  const evidence = new Map<number, EvidenceEntry>();
  const closed = /```evidence\s*\n?([\s\S]*?)```/.exec(content);
  if (closed) {
    const clean = (content.slice(0, closed.index) + content.slice(closed.index + closed[0].length)).trimEnd();
    try {
      const arr = tolerantJsonParse(closed[1]);
      if (Array.isArray(arr)) {
        for (const e of arr) {
          if (e && typeof e.n === 'number' && typeof e.exact === 'string' && e.exact.trim()) {
            // Models sometimes emit the page as a string ("3") — coerce it.
            const page = typeof e.page === 'number' && Number.isFinite(e.page)
              ? e.page
              : typeof e.page === 'string' && /^\d+$/.test(e.page.trim())
                ? Number(e.page.trim())
                : undefined;
            evidence.set(e.n, { page, exact: e.exact });
          }
        }
      }
    } catch {
      // Malformed JSON: the block is hidden but citations stay inert.
    }
    return { clean, evidence };
  }
  // Unterminated block mid-stream: hide it, keep citations inert until the
  // closing fence arrives and the JSON parses.
  const open = /```evidence\s*\n?[\s\S]*$/.exec(content);
  if (open) {
    return { clean: content.slice(0, open.index).trimEnd(), evidence };
  }
  return { clean: content, evidence };
}

// ── Deep links (siku-reader://<paperId>?page=N&exact=…) ──

export interface ReaderLinkTarget {
  paperId: string;
  page?: number;
  exact: string;
}

/** Build the in-app deep link carried by a saved note's evidence footnote.
 *  Clicking it in the note's reading view jumps to the reader and highlights
 *  the quoted passage. Self-contained: no hidden metadata block needed. */
export function evidenceReaderUrl(paperId: string, entry: EvidenceEntry): string {
  const params = new URLSearchParams();
  if (entry.page != null) params.set('page', String(entry.page));
  params.set('exact', entry.exact);
  return `siku-reader://${paperId}?${params.toString()}`;
}

/** Parse a siku-reader:// link back into its target. Null when malformed. */
export function parseReaderUrl(href: string): ReaderLinkTarget | null {
  const m = /^siku-reader:\/\/([^?]+)\?(.*)$/.exec(href);
  if (!m) return null;
  let exact = '';
  let page: number | undefined;
  try {
    const params = new URLSearchParams(m[2]);
    exact = params.get('exact') ?? '';
    const p = params.get('page');
    if (p && /^\d+$/.test(p)) page = Number(p);
  } catch {
    return null;
  }
  if (!m[1] || !exact.trim()) return null;
  return { paperId: m[1], page, exact };
}

// ── Note conversion ──

/** Collapse whitespace (quotes come from chunked PDF text and may contain
 *  embedded newlines, which would break a single-line footnote definition). */
function flattenQuote(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

/** Convert an assistant reply into portable note Markdown: the evidence
 *  block is stripped and each citation becomes a standard GFM footnote whose
 *  text is the quoted passage, prefixed by a clickable deep link back to the
 *  PDF page (when the paper id is known). The result stays readable in any
 *  Markdown editor — the link is in-app sugar, not the content itself. */
export function buildNoteMarkdown(content: string, paperId?: string): string {
  const { clean, evidence } = parseEvidence(content);
  if (evidence.size === 0) return clean;
  const defs: string[] = [];
  for (const [n, ev] of [...evidence.entries()].sort((a, b) => a[0] - b[0])) {
    const quote = `「${flattenQuote(ev.exact)}」`;
    if (paperId) {
      const label = ev.page != null ? `第 ${ev.page} 页` : 'PDF 原文';
      defs.push(`[^${n}]: [${label}](${evidenceReaderUrl(paperId, ev)}) · ${quote}`);
    } else {
      const label = ev.page != null ? `第 ${ev.page} 页 · ` : '';
      defs.push(`[^${n}]: ${label}${quote}`);
    }
  }
  return `${clean}\n\n${defs.join('\n')}\n`;
}
