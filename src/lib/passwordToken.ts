// ── !pw[...] password token ─────────────────────────────────────────────────
// Source syntax: `!pw[<secret>]` — rendered masked (dots + eye toggle + copy)
// in the reading view and the editor's live preview. The secret is stored as
// plaintext in the note; this is visual masking only, not encryption.
//
// Escape contract INSIDE the brackets (every view parses the raw source, so
// the rules are identical everywhere): `\\` → `\`, `\]` → `]`, `\|` → `|`
// (the pipe escape only matters inside table cells, where GFM reserves `|`).
// A backslash before any other character is literal, so `C:\path` needs no
// escaping. `]` must always be escaped: downstream markdown decoding makes an
// escaped `\]` indistinguishable from a real `]`, which is why the token is
// parsed from raw source (see rewritePasswordTokens) instead of a remark
// plugin over mdast text nodes.

const TOKEN_RE_SOURCE = '!pw\\[((?:\\\\[\\]\\\\|]|[^\\]\\n])+)\\]';

/** Fresh global regex matching one token; group 1 is the escaped secret. */
export function pwTokenRe(): RegExp {
  return new RegExp(TOKEN_RE_SOURCE, 'g');
}

/** Strip pw-token escapes: `\\` → `\`, `\]` → `]`, `\|` → `|`. */
export function unescapePw(s: string): string {
  return s.replace(/\\([\]\\|])/g, '$1');
}

/** Apply pw-token escapes to a raw secret (for insertion into note source). */
export function escapePw(s: string): string {
  return s.replace(/([\]\\|])/g, '\\$1');
}

/** Markdown-safe link carrying the secret percent-encoded; rendered as a
 *  masked PasswordField by WikiMarkdown's link component. */
function pwLink(secret: string): string {
  return `[••••••••](pw://${encodeURIComponent(secret)})`;
}

/** Rewrite the non-code segments of a single line. Backtick runs toggle
 *  inline-code state (same run-length matching as splitTableRowSpans). */
function rewriteLine(line: string): string {
  let out = '';
  let pos = 0;
  let fence: string | null = null;
  while (pos < line.length) {
    if (line[pos] === '`') {
      let n = 1;
      while (line[pos + n] === '`') n += 1;
      const run = line.slice(pos, pos + n);
      if (fence === null) fence = run;
      else if (run === fence) fence = null;
      out += run;
      pos += n;
      continue;
    }
    let next = line.indexOf('`', pos);
    if (next === -1) next = line.length;
    const seg = line.slice(pos, next);
    out += fence === null
      ? seg.replace(pwTokenRe(), (_m, escaped: string) => pwLink(unescapePw(escaped)))
      : seg;
    pos = next;
  }
  return out;
}

/** Rewrite `!pw[...]` tokens in raw note source into markdown links, which the
 *  reading view renders as masked password fields. Fenced code blocks and
 *  inline code spans are left untouched (a token inside code stays literal).
 *  Must run BEFORE markdown parsing (and before wiki-link rewriting, so a
 *  secret containing `[[` is never linkified): the percent-encoded URL keeps
 *  the secret opaque to markdown's own backslash/entity decoding, which would
 *  otherwise corrupt secrets and blur the token terminator. */
export function rewritePasswordTokens(text: string): string {
  if (!text.includes('!pw[')) return text;
  let fence: { ch: string; len: number } | null = null;
  return text
    .split('\n')
    .map((line) => {
      const m = /^ {0,3}(`{3,}|~{3,})/.exec(line);
      if (m) {
        const ch = m[1][0];
        // A fence closes only on the same char with at least the opening run
        // length (CommonMark rule).
        if (fence === null) fence = { ch, len: m[1].length };
        else if (ch === fence.ch && m[1].length >= fence.len) fence = null;
        return line;
      }
      return fence === null ? rewriteLine(line) : line;
    })
    .join('\n');
}
