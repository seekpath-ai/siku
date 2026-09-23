/**
 * remark plugin: turn bare local file paths in assistant prose into
 * `siku-path:` links, which the markdown renderer shows as clickable
 * open/reveal chips (see chat/FilePathLink).
 *
 * Only `text` nodes in plain prose are scanned — link/image/code/html
 * subtrees are left alone, so markdown syntax and URLs are never rewritten.
 */

export const SIKU_PATH_SCHEME = 'siku-path://';

/** Minimal mdast shape (the project doesn't depend on @types/mdast). */
interface MdNode {
  type: string;
  value?: string;
  url?: string;
  children?: MdNode[];
}

const SKIP_TYPES = new Set([
  'link',
  'linkReference',
  'image',
  'imageReference',
  'inlineCode',
  'code',
  'html',
  'definition',
]);

// Candidate root: Windows drive, UNC share, or Unix root (not `//`, which is
// a protocol-relative URL). The body is greedy over everything but
// whitespace, CJK punctuation and markdown-structural characters; trailing
// ASCII punctuation is stripped afterwards.
const CANDIDATE_RE = /(?:[A-Za-z]:[\\/]|\\\\|\/(?!\/))[^\s<>"'|*^`，。；：！？、（）【】《》「」“”‘’]+/g;

// The char before a candidate must not look like the middle of a word, URL
// scheme or number — otherwise `https://…`, `1/2/3` and `//cdn…` match.
const BAD_PREFIX_RE = /[A-Za-z0-9+./:\\-]/;

const TRAILING_PUNCT = new Set([...'.,;:!?)]}…']);

function isPlausiblePath(candidate: string): boolean {
  // A path ends at a file name, not a separator.
  const p = candidate.replace(/[\\/]+$/, '');
  if (/^[A-Za-z]:[\\/]/.test(p)) return p.length > 3; // C:\x
  if (p.startsWith('\\\\')) return /^\\\\[^\\/]+[\\/]+[^\\/]+/.test(p); // \\server\share
  if (p.startsWith('/')) return p.indexOf('/', 1) > 1; // /seg/seg, not /word
  return false;
}

/** Split prose into text nodes and `siku-path:` link nodes. */
function splitTextNode(node: MdNode): MdNode[] {
  const text = node.value ?? '';
  const out: MdNode[] = [];
  let last = 0;
  CANDIDATE_RE.lastIndex = 0;

  for (let m = CANDIDATE_RE.exec(text); m; m = CANDIDATE_RE.exec(text)) {
    const before = text[m.index - 1];
    if (before !== undefined && BAD_PREFIX_RE.test(before)) continue;

    let path = m[0];
    while (path.length > 0 && TRAILING_PUNCT.has(path[path.length - 1])) {
      path = path.slice(0, -1);
    }
    // Normalize `dir/` → `dir`; the plausibility check also rejects roots
    // left bare by the strip (`C:\` alone is not a file path).
    path = path.replace(/[\\/]+$/, '');
    if (!isPlausiblePath(path)) continue;

    if (m.index > last) out.push({ type: 'text', value: text.slice(last, m.index) });
    out.push({
      type: 'link',
      url: SIKU_PATH_SCHEME + encodeURIComponent(path),
      children: [{ type: 'text', value: path }],
    });
    last = m.index + path.length;
  }

  if (out.length === 0) return [node];
  if (last < text.length) out.push({ type: 'text', value: text.slice(last) });
  return out;
}

function transformChildren(parent: MdNode): void {
  if (!parent.children) return;
  const out: MdNode[] = [];
  for (const child of parent.children) {
    if (child.type === 'text' && child.value) {
      out.push(...splitTextNode(child));
    } else {
      if (!SKIP_TYPES.has(child.type)) transformChildren(child);
      out.push(child);
    }
  }
  parent.children = out;
}

export function remarkFilePaths() {
  return (tree: MdNode) => {
    transformChildren(tree);
  };
}
