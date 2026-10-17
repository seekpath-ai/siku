/** Export-time citation rewrite for notes.
 *
 *  AI 综述 / 文献分析 notes cite with an in-app deep link that jumps straight to
 *  the evidence passage in the PDF reader:
 *
 *      [[9]](siku-reader://<paperId>?page=7&exact=…)      single citation
 *      [[9](u9),[13](u13)]                                citation group
 *
 *  That link is meaningless inside an exported PDF (the reader intercepts the
 *  click; the print copy renders `href="#"`), so the export can rewrite each
 *  citation into an intra-document link to the note's own reference list — the
 *  way a real paper's `[9]` jumps to its bibliography entry.
 *
 *  Pure functions only: the note content is never modified, the rewrite happens
 *  on the copy that is printed.
 */

/** Anchor id prefix used for reference-list entries (`siku-ref-9`). */
export const REF_ANCHOR_PREFIX = 'siku-ref-';

/** Class put on every paragraph of the 参考文献 section so the print CSS can
 *  drop the first-line indent there (a bibliography is not body prose). */
export const REF_ENTRY_CLASS = 'pdf-ref-entry';

const REF_HEADING = /^#{1,6}\s+参考文献\s*$/;

/** True when this note has citations that the rewrite could act on: at least
 *  one `siku-reader://` citation link AND a 参考文献 section to point at. */
export function hasFixableCitations(content: string): boolean {
  if (!content) return false;
  const hasCitation = /\[\[?\d+\]?\]\(siku-reader:\/\//.test(content);
  if (!hasCitation) return false;
  return content.split('\n').some((line) => REF_HEADING.test(line.trim()));
}

/** Rewrite citation deep links into in-note anchors.
 *
 *  Order matters: the single-citation form `[[9]](…)` is rewritten first —
 *  rewriting the inner `[9](…)` of that form would leave the outer brackets
 *  wrapping a half-parsed link. */
export function rewriteCitationsToAnchors(markdown: string): string {
  return markdown
    .replace(/\[\[(\d+)\]\]\(siku-reader:\/\/[^)]*\)/g, (_m, n: string) => `[[${n}]](#${REF_ANCHOR_PREFIX}${n})`)
    .replace(/\[(\d+)\]\(siku-reader:\/\/[^)]*\)/g, (_m, n: string) => `[${n}](#${REF_ANCHOR_PREFIX}${n})`);
}

/** Give every reference entry its own paragraph.
 *
 *  `reference_line` writes them on consecutive lines, so CommonMark folds the
 *  whole bibliography into a single `<p>` (and with 强换行 on, into `<br>`s of
 *  that same paragraph) — one paragraph can carry only one anchor id, and the
 *  printed result reads as one blob. A blank line between entries fixes both. */
export function splitReferenceEntries(markdown: string): string {
  const lines = markdown.split('\n');
  const out: string[] = [];
  let inRefs = false;
  for (const line of lines) {
    const trimmed = line.trim();
    if (REF_HEADING.test(trimmed)) {
      inRefs = true;
      out.push(line);
      continue;
    }
    if (inRefs) {
      if (/^#{1,6}\s+/.test(trimmed)) {
        inRefs = false;
        out.push(line);
        continue;
      }
      // A new numbered entry starts: keep it visually separate.
      if (/^\[\d+\]/.test(trimmed) && out.length > 0 && out[out.length - 1].trim() !== '') {
        out.push('');
      }
    }
    out.push(line);
  }
  return out.join('\n');
}

/** Full export prep: in-note citation targets + one paragraph per entry. */
export function toReferenceCitations(markdown: string): string {
  return splitReferenceEntries(rewriteCitationsToAnchors(markdown));
}

// ── rehype plugin: id (and indent-exemption class) for reference entries ──

interface HastNode {
  type: string;
  tagName?: string;
  value?: string;
  properties?: Record<string, unknown>;
  children?: HastNode[];
}

function nodeText(node: HastNode): string {
  if (node.type === 'text') return node.value ?? '';
  return (node.children ?? []).map(nodeText).join('');
}

/** Stamp `id="siku-ref-N"` on each `[N] …` entry under the 参考文献 heading and
 *  tag all paragraphs of that section with {@link REF_ENTRY_CLASS}.
 *
 *  Raw HTML is not allowed through react-markdown (no rehype-raw), so this is
 *  the only place the anchor can be attached. Runs only in the export path. */
export function rehypeReferenceAnchors() {
  return (tree: HastNode) => {
    let inRefs = false;
    for (const node of tree.children ?? []) {
      if (node.type === 'element' && node.tagName && /^h[1-6]$/.test(node.tagName)) {
        inRefs = nodeText(node).trim() === '参考文献';
        continue;
      }
      if (!inRefs) continue;
      // hast keeps whitespace text nodes between block elements — they do NOT
      // end the section (treating them as the end made the plugin a no-op).
      if (node.type === 'text') {
        if ((node.value ?? '').trim()) inRefs = false;
        continue;
      }
      if (node.type !== 'element') continue;
      if (node.tagName !== 'p') {
        // Section ends at the next non-paragraph block (a list, another heading…).
        inRefs = false;
        continue;
      }
      const properties = { ...(node.properties ?? {}) };
      const classes = Array.isArray(properties.className) ? (properties.className as string[]) : [];
      properties.className = [...classes, REF_ENTRY_CLASS];
      const match = /^\[(\d+)\]/.exec(nodeText(node));
      if (match) properties.id = `${REF_ANCHOR_PREFIX}${match[1]}`;
      node.properties = properties;
    }
  };
}
