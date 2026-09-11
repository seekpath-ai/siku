import { unified } from 'unified';
import remarkParse from 'remark-parse';

interface MdNode {
  type: string;
  position?: { start: { offset?: number }; end: { offset?: number } };
  children?: MdNode[];
}

/** Locate fenced/indented code blocks and inline code spans with the real
 *  markdown parser — these ranges must never see delimiter conversion
 *  (`\(` in a shell regex like `sed 's/\(x\)/\1/'` is not math). */
function codeRanges(src: string): [number, number][] {
  const tree = unified().use(remarkParse).parse(src) as unknown as MdNode;
  const ranges: [number, number][] = [];
  const walk = (node: MdNode) => {
    if (node.type === 'code' || node.type === 'inlineCode') {
      const s = node.position?.start.offset;
      const e = node.position?.end.offset;
      if (s != null && e != null) ranges.push([s, e]);
      return; // code nodes hold no children to scan
    }
    node.children?.forEach(walk);
  };
  walk(tree);
  return ranges.sort((a, b) => a[0] - b[0]);
}

// `(?<!\\)` guards against an escaped backslash: `\\(` is a literal backslash
// followed by `(`, not a math opener.
function convertSegment(text: string): string {
  return text
    .replace(/(?<!\\)\\\[([\s\S]*?)(?<!\\)\\\]/g, (_m, inner) => `$$${inner}$$`)
    .replace(/(?<!\\)\\\(([\s\S]*?)(?<!\\)\\\)/g, (_m, inner) => `$${inner}$`);
}

/** Convert LLM-style math delimiters `\(...\)` / `\[...\]` (the default output
 *  of GPT-/DeepSeek-style models) into the `$...$` / `$$...$$` that
 *  remark-math understands. Code blocks and inline code are located with the
 *  real parser and copied byte-for-byte, so normal content is not corrupted. */
export function normalizeMathDelimiters(src: string): string {
  if (!src.includes('\\(') && !src.includes('\\[')) return src;
  const ranges = codeRanges(src);
  let out = '';
  let pos = 0;
  for (const [s, e] of ranges) {
    if (s < pos) continue; // nested/overlapping ranges can't occur, stay safe
    out += convertSegment(src.slice(pos, s));
    out += src.slice(s, e);
    pos = e;
  }
  out += convertSegment(src.slice(pos));
  return out;
}
