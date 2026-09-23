/**
 * remark plugin: turn bare local file paths in assistant prose into
 * `siku-path:` links, which the markdown renderer shows as clickable
 * open/reveal chips (see chat/FilePathLink).
 *
 * Only `text` nodes in plain prose are scanned — link/image/code/html
 * subtrees are left alone, so markdown syntax and URLs are never rewritten.
 * Candidates are GREEDY: spaces and CJK prose glued to the path stay inside
 * the match, and the backend (`resolve_existing_path`) truncates back to a
 * path that actually exists; the chip renders any swallowed remainder as
 * plain text again.
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

const ROOT = '(?:[A-Za-z]:[\\\\/]|\\\\\\\\|\\/(?!\\/))';

// Greedy candidate: runs to end of line or a terminator (ASCII/CJK
// punctuation, quotes, markdown-structural chars). Spaces are allowed inside
// so `C:\Program Files\x.pdf` and `/home/x/my docs/a.png` match whole.
const CANDIDATE_RE = new RegExp(
  `${ROOT}[^\\n\\r<>"'|*^\`,;:!?。，；：！？、()（）\\[\\]【】{}《》「」]+`,
  'g'
);

// Quoted path: quotes give exact boundaries and may enclose terminators
// (`"C:\a, b\c.pdf"`); the quotes themselves never enter the path.
const QUOTED_RE = new RegExp(`(["'])(${ROOT}[^\\n\\r"']+)\\1`, 'g');

// The char before a candidate must not look like the middle of a word, URL
// scheme or number — otherwise `https://…`, `1/2/3` and `//cdn…` match.
const BAD_PREFIX_RE = /[A-Za-z0-9+./:\\-]/;

function isPlausiblePath(candidate: string): boolean {
  // A path ends at a file name, not a separator.
  const p = candidate.replace(/[\\/]+$/, '');
  if (/^[A-Za-z]:[\\/]/.test(p)) return p.length > 3; // C:\x
  if (p.startsWith('\\\\')) return /^\\\\[^\\/]+[\\/]+[^\\/]+/.test(p); // \\server\share
  if (p.startsWith('/')) return p.indexOf('/', 1) > 1; // /seg/seg, not /word
  return false;
}

interface PathMatch {
  start: number;
  end: number;
  path: string;
}

/** Quoted matches first (precise boundaries), then greedy candidates;
 *  overlaps resolve in favor of the earlier/longer match. */
function collectMatches(text: string): PathMatch[] {
  const matches: PathMatch[] = [];

  QUOTED_RE.lastIndex = 0;
  for (let m = QUOTED_RE.exec(text); m; m = QUOTED_RE.exec(text)) {
    const path = m[2];
    if (isPlausiblePath(path)) {
      matches.push({ start: m.index + 1, end: m.index + 1 + path.length, path });
    }
  }

  CANDIDATE_RE.lastIndex = 0;
  for (let m = CANDIDATE_RE.exec(text); m; m = CANDIDATE_RE.exec(text)) {
    const before = text[m.index - 1];
    if (before !== undefined && BAD_PREFIX_RE.test(before)) continue;
    const path = m[0].replace(/[\s.~]+$/, '');
    if (isPlausiblePath(path)) {
      matches.push({ start: m.index, end: m.index + path.length, path });
    }
  }

  matches.sort((a, b) => a.start - b.start || b.end - a.end);
  const out: PathMatch[] = [];
  let cursor = -1;
  for (const m of matches) {
    if (m.start >= cursor) {
      out.push(m);
      cursor = m.end;
    }
  }
  return out;
}

/** Split prose into text nodes and `siku-path:` link nodes. */
function splitTextNode(node: MdNode): MdNode[] {
  const text = node.value ?? '';
  const matches = collectMatches(text);
  if (matches.length === 0) return [node];

  const out: MdNode[] = [];
  let last = 0;
  for (const m of matches) {
    if (m.start > last) out.push({ type: 'text', value: text.slice(last, m.start) });
    out.push({
      type: 'link',
      url: SIKU_PATH_SCHEME + encodeURIComponent(m.path),
      children: [{ type: 'text', value: m.path }],
    });
    last = m.end;
  }
  if (last < text.length) out.push({ type: 'text', value: text.slice(last) });
  return out;
}

/** `` `/abs/path` `` (whole inline-code span, optional quotes/whitespace) is
 *  a path the author delimited themselves — render it as a chip too. */
function inlineCodePath(node: MdNode): string | null {
  const v = (node.value ?? '').trim().replace(/^["']+|["']+$/g, '');
  return isPlausiblePath(v) ? v : null;
}

function transformChildren(parent: MdNode): void {
  if (!parent.children) return;
  const out: MdNode[] = [];
  for (const child of parent.children) {
    if (child.type === 'text' && child.value) {
      out.push(...splitTextNode(child));
      continue;
    }
    if (child.type === 'inlineCode') {
      const path = inlineCodePath(child);
      if (path) {
        out.push({
          type: 'link',
          url: SIKU_PATH_SCHEME + encodeURIComponent(path),
          children: [{ type: 'text', value: path }],
        });
        continue;
      }
    }
    if (!SKIP_TYPES.has(child.type)) transformChildren(child);
    out.push(child);
  }
  parent.children = out;
}

export function remarkFilePaths() {
  return (tree: MdNode) => {
    transformChildren(tree);
  };
}
