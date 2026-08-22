import { useMemo } from 'react';
import CodeMirror from '@uiw/react-codemirror';
import { markdown, markdownLanguage } from '@codemirror/lang-markdown';
import { languages } from '@codemirror/language-data';
import { oneDark } from '@codemirror/theme-one-dark';
import { EditorView, Decoration, DecorationSet, WidgetType, ViewPlugin, type ViewUpdate } from '@codemirror/view';
import { RangeSetBuilder, StateField, Facet, type EditorState, type Extension } from '@codemirror/state';
import { syntaxTree } from '@codemirror/language';
import { autocompletion, type CompletionContext } from '@codemirror/autocomplete';
import { open as shellOpen } from '@tauri-apps/plugin-shell';
import katex from 'katex';
import { saveAttachmentBytes } from '@/lib/tauri';
import { resolveImageUrl, resolveLocalImageUrl, type ResolveImageOptions } from '@/lib/imageCache';
import type { Note } from '@/lib/types';

// ── Obsidian-style Live Preview extensions ─────────────────────────────

const mark = (cls: string) => Decoration.mark({ class: cls });

// Syntax markers stay visible (dimmed) so markdown stays editable normally.
// Note: the blockquote `>` marker node is `QuoteMark` in @lezer/markdown
// (there is no `BlockquoteMark` node — that typo used to leave `>` visible).
const markerMarks = new Set(['HeaderMark', 'EmphasisMark', 'StrongEmphasisMark', 'StrikethroughMark', 'CodeMark', 'LinkMark', 'QuoteMark', 'ImageMark']);
const styleMarks: Record<string, string> = {
  ATXHeading1: 'cm-live-h1',
  ATXHeading2: 'cm-live-h2',
  ATXHeading3: 'cm-live-h3',
  ATXHeading4: 'cm-live-h4',
  ATXHeading5: 'cm-live-h5',
  ATXHeading6: 'cm-live-h6',
  StrongEmphasis: 'cm-live-strong',
  Strikethrough: 'cm-live-strike',
  InlineCode: 'cm-live-code',
  Blockquote: 'cm-live-quote',
  Link: 'cm-live-link',
  Emphasis: 'cm-live-em',
};

// ── Rendered widgets: math (KaTeX) and tables (GFM) ──

// KaTeX rendering is the most expensive widget operation; cache the HTML so
// scrolling a formula out of and back into the viewport does not re-render it.
const KATEX_CACHE_LIMIT = 200;
const katexCache = new Map<string, string>();

function renderKatex(latex: string, displayMode: boolean): string {
  const key = (displayMode ? 'D' : 'I') + latex;
  const hit = katexCache.get(key);
  if (hit !== undefined) return hit;
  const html = katex.renderToString(latex, { throwOnError: false, displayMode });
  if (katexCache.size >= KATEX_CACHE_LIMIT) katexCache.clear();
  katexCache.set(key, html);
  return html;
}

/** Renders a LaTeX formula via KaTeX. Clicking the rendered formula places the
 *  caret at its source offset so the raw `$...$` becomes editable again.
 *  The position is resolved from the DOM (posAtDOM) rather than stored, so the
 *  widget instance can be reused across document edits. */
class MathWidget extends WidgetType {
  constructor(
    readonly latex: string,
    readonly displayMode: boolean
  ) {
    super();
  }

  toDOM(view: EditorView) {
    const span = document.createElement('span');
    // The block class kills KaTeX's `.katex-display` vertical margins (see
    // index.css): margins are invisible to CM's block-widget height
    // measurement and desync click coordinates.
    span.className = this.displayMode ? 'cm-live-math cm-live-math-block' : 'cm-live-math';
    span.innerHTML = renderKatex(this.latex, this.displayMode);
    span.addEventListener('mousedown', (e) => {
      e.preventDefault();
      e.stopPropagation();
      view.dispatch({ selection: { anchor: view.posAtDOM(span) }, scrollIntoView: true });
    });
    return span;
  }

  // Rough height for not-yet-measured offscreen block widgets.
  get estimatedHeight() {
    return this.displayMode ? 60 : -1;
  }

  // eq() already guarantees identical content, so the existing DOM is reusable.
  updateDOM() {
    return true;
  }

  ignoreEvent(event: Event) {
    return event.type === 'mousedown';
  }

  eq(other: MathWidget) {
    return other.latex === this.latex && other.displayMode === this.displayMode;
  }
}

/** Renders a GFM table block as an HTML <table>. Clicking the rendered table
 *  places the caret at its first source line so the raw cells become editable. */
// Lucide "copy" / "check" glyphs, inlined because widget DOM is built outside React.
const COPY_ICON = '<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
const CHECK_ICON = '<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>';

/** Copy `text` to the clipboard and flash a check glyph on the button. */
function copyWithFeedback(text: string, btn: HTMLElement) {
  if (!text) return;
  navigator.clipboard.writeText(text).then(() => {
    btn.innerHTML = CHECK_ICON;
    btn.classList.add('copied');
    setTimeout(() => {
      btn.innerHTML = COPY_ICON;
      btn.classList.remove('copied');
    }, 1500);
  }).catch(() => { /* ignore */ });
}

class TableWidget extends WidgetType {
  constructor(readonly html: string, readonly markdown: string) {
    super();
  }

  toDOM(view: EditorView) {
    const wrap = document.createElement('div');
    wrap.className = 'cm-live-table';
    wrap.innerHTML = this.html;

    // Corner button: copy the whole table as its original markdown source.
    const tableBtn = document.createElement('button');
    tableBtn.className = 'cm-live-table-copy';
    tableBtn.title = '复制表格 (Markdown)';
    tableBtn.innerHTML = COPY_ICON;
    wrap.appendChild(tableBtn);

    wrap.addEventListener('mousedown', (e) => {
      e.preventDefault();
      e.stopPropagation();
      // Copy buttons must be handled HERE, on mousedown: the default path
      // below moves the caret into the table, which tears the widget down
      // and restores the raw source before a click event would ever fire.
      const target = e.target as HTMLElement;
      if (target.closest('.cm-live-table-copy')) {
        copyWithFeedback(this.markdown, tableBtn);
        return;
      }
      const cellBtn = target.closest('.cm-live-table-cellcopy');
      if (cellBtn) {
        // The button holds only an SVG (no text), so the cell's innerText is
        // exactly the cell content.
        const cell = cellBtn.closest('td, th') as HTMLElement | null;
        copyWithFeedback(cell?.innerText.trim() ?? '', cellBtn as HTMLElement);
        return;
      }
      view.dispatch({ selection: { anchor: view.posAtDOM(wrap) }, scrollIntoView: true });
    });
    return wrap;
  }

  updateDOM() {
    return true;
  }

  // Rough height for not-yet-measured offscreen tables: rows + padding.
  get estimatedHeight() {
    return (this.html.split('<tr').length - 1) * 34 + 12;
  }

  ignoreEvent(event: Event) {
    return event.type === 'mousedown';
  }

  eq(other: TableWidget) {
    return other.html === this.html && other.markdown === this.markdown;
  }
}

/** Bullet glyph replacing the raw `-`/`*`/`+` list marker when the caret is
 *  away from the list item (Obsidian renders a dot instead of the marker). */
class ListBulletWidget extends WidgetType {
  constructor(readonly bullet: string) {
    super();
  }
  toDOM() {
    const span = document.createElement('span');
    span.className = 'cm-live-bullet';
    span.textContent = this.bullet;
    return span;
  }
  eq(other: ListBulletWidget) {
    return other.bullet === this.bullet;
  }
}

/** Checkbox glyph for task list markers (`[ ]` / `[x]`). Clicking toggles the
 *  source marker, like Obsidian. */
class TaskCheckWidget extends WidgetType {
  constructor(readonly checked: boolean) {
    super();
  }
  toDOM(view: EditorView) {
    const span = document.createElement('span');
    span.className = 'cm-live-task';
    span.textContent = this.checked ? '☑' : '☐';
    span.addEventListener('mousedown', (e) => {
      e.preventDefault();
      e.stopPropagation();
      // The widget replaces the 3-char TaskMarker ([ ]/[x]); posAtDOM gives
      // its current position even after earlier edits shifted it.
      const pos = view.posAtDOM(span);
      view.dispatch({
        changes: { from: pos, to: pos + 3, insert: this.checked ? '[ ]' : '[x]' },
      });
    });
    return span;
  }
  updateDOM() {
    return true;
  }
  ignoreEvent(event: Event) {
    return event.type === 'mousedown';
  }
  eq(other: TaskCheckWidget) {
    return other.checked === this.checked;
  }
}

function isRemoteImageSrc(src: string): boolean {
  return /^https?:\/\//.test(src);
}

/** Facet that carries image-resolution context (attachments base directory). */
const imageOptionsFacet = Facet.define<ResolveImageOptions, ResolveImageOptions>({
  combine: (values) => values[0] ?? {},
});

/** Rendered image (`![alt](src)`), shown while the caret is elsewhere. */
class ImageWidget extends WidgetType {
  constructor(
    readonly src: string,
    readonly alt: string,
    readonly title?: string,
    readonly isRemote = false
  ) {
    super();
  }
  toDOM() {
    const img = document.createElement('img');
    img.className = 'cm-live-image';
    img.alt = this.alt;
    if (this.title) img.title = this.title;
    if (this.isRemote) {
      // Remote images must be cached by the Rust backend and loaded through
      // the asset protocol to comply with the CSP.
      resolveImageUrl(this.src)
        .then((url) => {
          img.src = url;
        })
        .catch(() => {
          img.style.display = 'none';
        });
    } else {
      img.src = this.src;
    }
    img.onerror = () => {
      img.style.display = 'none';
    };
    return img;
  }
  // Same src/alt/title → same image; reuse the DOM instead of re-fetching.
  updateDOM() {
    return true;
  }
  eq(other: ImageWidget) {
    return other.src === this.src && other.alt === this.alt && other.title === this.title;
  }
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

/** Render a GFM table block (lines of `|`-separated cells) as HTML.
 *  Each cell gets a hover-only copy button (rendered tables swallow
 *  mousedown for click-to-edit, so without it cell text cannot be copied
 *  at all). */
function parseMarkdownTable(text: string): string {
  const lines = text.split('\n').map((l) => l.trim());
  const cells = (line: string) =>
    line.replace(/^\||\|$/g, '').split('|').map((c) => c.trim());
  const header = cells(lines[0]);
  const isAlignRow = lines.length > 1 && /^[\s:|-]+$/.test(lines[1]) && lines[1].includes('-');
  const body = lines.slice(isAlignRow ? 2 : 1);
  const cellBtn = `<button class="cm-live-table-cellcopy" title="复制单元格">${COPY_ICON}</button>`;

  let html = '<table><thead><tr>';
  html += header.map((c) => `<th>${escapeHtml(c)}${cellBtn}</th>`).join('');
  html += '</tr></thead>';
  if (body.length > 0) {
    html += '<tbody>';
    for (const row of body) {
      html += '<tr>' + cells(row).map((c) => `<td>${escapeHtml(c)}${cellBtn}</td>`).join('') + '</tr>';
    }
    html += '</tbody>';
  }
  html += '</table>';
  return html;
}

/** Renders markdown semantics inline; syntax markers are hidden unless the
 *  caret is within/adjacent to their content (Obsidian-style live preview).
 *
 *  Implemented as a ViewPlugin over `view.visibleRanges`: only the visible
 *  part of the document is decorated. The previous full-document StateField
 *  scanned the entire syntax tree and ran several full-text regexes on every
 *  keystroke and caret move, which did not scale to long notes. */
const livePreviewPlugin = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;

    constructor(view: EditorView) {
      this.decorations = buildLivePreview(view);
    }

    update(u: ViewUpdate) {
      // While a non-empty selection is being dragged (mouse) or extended
      // (shift+arrows), the preview layout does not depend on it. Rebuilding on
      // every selection transaction would tear down and recreate rendered
      // widgets (images/math/tables) as the selection boundary sweeps across
      // them, causing visible flicker. Reuse the last set until the selection
      // collapses; doc/viewport changes always rebuild.
      if (
        !u.docChanged &&
        !u.viewportChanged &&
        u.state.selection.ranges.some((r) => !r.empty)
      ) {
        return;
      }
      if (u.docChanged || u.selectionSet || u.viewportChanged) {
        this.decorations = buildLivePreview(u.view);
      }
    }
  },
  { decorations: (v) => v.decorations }
);

type SyntaxNodeInfo = { name: string; from: number; to: number };
/** Structural subset of lezer's SyntaxNode (which is not a direct dependency). */
interface SyntaxNodeLike { name: string; from: number; to: number; parent: SyntaxNodeLike | null }
type DecoItem = {
  from: number;
  to: number;
  /** 'line' = Decoration.line, 'widget' = point widget; both are exempt from
   *  the "covered by a replace range" cleanup (they carry no text styling). */
  kind: 'mark' | 'replace' | 'line' | 'widget';
  deco: Decoration;
};

/** Language badge + copy button floated at the top-right of a fenced code
 *  block (Obsidian style). The widget is recreated on every rebuild (default
 *  eq), so the stored source range is always fresh when copying. */
class CodeHeaderWidget extends WidgetType {
  constructor(
    readonly lang: string,
    readonly from: number,
    readonly to: number
  ) {
    super();
  }
  toDOM(view: EditorView) {
    const span = document.createElement('span');
    span.className = 'cm-live-code-header';
    if (this.lang) {
      const label = document.createElement('span');
      label.className = 'cm-live-code-lang';
      label.textContent = this.lang;
      span.appendChild(label);
    }
    const btn = document.createElement('button');
    btn.className = 'cm-live-code-copy';
    btn.type = 'button';
    btn.textContent = '复制';
    btn.addEventListener('mousedown', (e) => {
      e.preventDefault();
      e.stopPropagation();
      const code = view.state.doc.sliceString(this.from, this.to);
      navigator.clipboard.writeText(code).then(() => {
        btn.textContent = '已复制';
        setTimeout(() => { btn.textContent = '复制'; }, 1200);
      }).catch(() => { /* ignore */ });
    });
    span.appendChild(btn);
    return span;
  }
  ignoreEvent(event: Event) {
    return event.type === 'mousedown';
  }
}

const INLINE_PARENTS = new Set([
  'Emphasis',
  'StrongEmphasis',
  'Strikethrough',
  'InlineCode',
  'Link',
  'Image',
]);

function buildLivePreview(view: EditorView): DecorationSet {
  const { state } = view;
  const builder = new RangeSetBuilder<Decoration>();

  const ranges = state.selection.ranges;
  const near = (from: number, to: number) =>
    ranges.some((r) => r.from <= to && r.to >= from);

  const markerNear = (n: { from: number; to: number }) =>
    near(n.from - 1, n.to + 1);

  // Obsidian-style behaviour: only the line(s) that contain the cursor show raw
  // markdown markers; block-level elements that span multiple lines do not force
  // adjacent lines into source mode.
  const selectedLineRanges = ranges.map((r) => {
    const startLine = state.doc.lineAt(r.from);
    const endLine = state.doc.lineAt(r.to);
    return { from: startLine.from, to: endLine.to };
  });
  const onSelectedLine = (pos: number) =>
    selectedLineRanges.some((l) => pos >= l.from && pos <= l.to);

  const markItem = (from: number, to: number, cls: string): DecoItem => ({
    from, to, kind: 'mark', deco: mark(cls),
  });
  const replaceItem = (from: number, to: number, spec: Parameters<typeof Decoration.replace>[0]): DecoItem => ({
    from, to, kind: 'replace', deco: Decoration.replace(spec),
  });

  let adds: DecoItem[] = [];
  // Block-level constructs (tables, $$ math, horizontal rules) are rendered
  // by livePreviewBlockField — CodeMirror forbids block decorations from
  // view plugins. Their ranges are still collected here so overlapping
  // inline decorations get dropped.
  const tableRanges: { from: number; to: number }[] = [];
  const blockMathRanges: { from: number; to: number }[] = [];

  // Only visible ranges are decorated. A block starting just above the
  // viewport (fenced code, table) may be rendered once scrolled to — that is
  // the same trade-off Obsidian makes.
  for (const vr of view.visibleRanges) {
    const codeRanges: { from: number; to: number }[] = [];
    const stack: SyntaxNodeInfo[] = [];

    syntaxTree(state).iterate({
      from: vr.from,
      to: vr.to,
      enter(node) {
        // Nearest enclosing construct (skipping Document/Paragraph) — the
        // iteration stack replaces the old O(n) scopeFor search.
        let parent: SyntaxNodeInfo | undefined;
        for (let k = stack.length - 1; k >= 0; k--) {
          if (stack[k].name !== 'Document' && stack[k].name !== 'Paragraph') {
            parent = stack[k];
            break;
          }
        }
        const n: SyntaxNodeInfo = { name: node.name, from: node.from, to: node.to };

        if (n.name === 'FencedCode' || n.name === 'InlineCode') {
          codeRanges.push({ from: n.from, to: n.to });
        }

        if (n.name === 'FencedCode') {
          // Obsidian-style code block: content lines get a full-width
          // background (also while editing inside). While the caret is
          // anywhere inside the block the fence lines stay expanded with
          // their ``` markers visible so the block remains editable; with
          // the caret away the fences collapse to a slim gap and a language
          // badge + copy button floats at the top-right.
          const caretInside = near(n.from, n.to);
          const openLine = state.doc.lineAt(n.from);
          const closeLine = state.doc.lineAt(n.to);
          const fenceRe = /^\s*(`{3,}|~{3,})\s*$/;
          // An unclosed fence (user is still typing) ends at the doc: then
          // the last line is content, not a fence.
          const hasClose = closeLine.number > openLine.number && fenceRe.test(closeLine.text);
          const lastContentLine = hasClose ? closeLine.number - 1 : closeLine.number;

          for (let ln = openLine.number + 1; ln <= lastContentLine; ln++) {
            const line = state.doc.line(ln);
            let cls = 'cm-live-codeblock-line';
            if (ln === openLine.number + 1) cls += ' cm-live-codeblock-first';
            if (ln === lastContentLine) cls += ' cm-live-codeblock-last';
            adds.push({ from: line.from, to: line.from, kind: 'line', deco: Decoration.line({ class: cls }) });
          }
          // With no content lines (an empty block) the fences stay expanded —
          // collapsing both would make the block zero-height and unclickable.
          const hasContent = lastContentLine >= openLine.number + 1;
          if (!caretInside && hasContent) {
            adds.push({ from: openLine.from, to: openLine.from, kind: 'line', deco: Decoration.line({ class: 'cm-live-fence' }) });
          }
          if (hasClose && !caretInside && hasContent) {
            adds.push({ from: closeLine.from, to: closeLine.from, kind: 'line', deco: Decoration.line({ class: 'cm-live-fence' }) });
          }
          if (hasContent && !caretInside) {
            const contentFrom = openLine.to + 1;
            const contentTo = state.doc.line(lastContentLine).to;
            const lang = openLine.text.replace(/^\s*(`{3,}|~{3,})\s*/, '').trim();
            adds.push({
              from: contentFrom, to: contentFrom, kind: 'widget',
              deco: Decoration.widget({ widget: new CodeHeaderWidget(lang, contentFrom, contentTo), side: -1 }),
            });
          }
        } else if (styleMarks[n.name]) {
          adds.push(markItem(n.from, n.to, styleMarks[n.name]));
        } else if (markerMarks.has(n.name) || n.name === 'CodeInfo') {
          const inline = parent && INLINE_PARENTS.has(parent.name);
          // Inline markers keep their parent scope so both delimiters of a
          // wrapped emphasis/link stay visible while editing it. Fence markers
          // (``` / language info) follow the whole code block so the block
          // stays editable while the caret is anywhere inside it. Other block
          // markers (headings, blockquotes, list bullets) are tied to the
          // current line.
          const showSource = inline || parent?.name === 'FencedCode'
            ? near(parent!.from, parent!.to) || markerNear(n)
            : onSelectedLine(n.from) || markerNear(n);
          if (showSource) {
            adds.push(markItem(n.from, n.to, 'cm-live-marker'));
          } else {
            // QuoteMark covers only the `>` char; swallow the single space
            // after it too, so quoted text aligns flush with the quote bar.
            const hideTo = n.name === 'QuoteMark' && state.doc.sliceString(n.to, n.to + 1) === ' '
              ? n.to + 1
              : n.to;
            adds.push(replaceItem(n.from, hideTo, {}));
          }
        } else if (n.name === 'URL') {
          // Inside a Link, a URL node is usually the destination `(url)` part —
          // but when the link TEXT is itself a URL (`[https://a](https://b)`),
          // GFM autolink marks the text as a URL node too. Only the destination
          // (preceded by `(`) is hidden/dimmed; a text URL needs no decoration —
          // the enclosing Link's cm-live-link mark already styles it.
          const precededByParen = parent?.name === 'Link'
            && /\(\s*$/.test(state.doc.sliceString(Math.max(0, n.from - 10), n.from));
          if (precededByParen && parent) {
            // URL part of a `[text](url)` link: hide it when rendering the link.
            if (near(parent.from, parent.to) || markerNear(n)) {
              adds.push(markItem(n.from, n.to, 'cm-live-marker'));
            } else {
              adds.push(replaceItem(n.from, n.to, {}));
            }
          } else if (parent?.name !== 'Link' && !near(n.from - 1, n.to + 1)) {
            // Bare URL or <autolink>: keep it visible as a styled link while
            // the caret is away; plain text while editing.
            adds.push(markItem(n.from, n.to, 'cm-live-link'));
          }
        } else if (n.name === 'CodeText') {
          // Fenced code content is styled by the block's line decorations;
          // only indented code still needs the inline mark.
          if (parent?.name !== 'FencedCode') {
            adds.push(markItem(n.from, n.to, 'cm-live-codeblock'));
          }
        } else if (n.name === 'ListMark') {
          const inListItem = parent?.name === 'ListItem' || parent?.name === 'Task';
          if (inListItem && !onSelectedLine(n.from) && !markerNear(n)) {
            const text = state.doc.sliceString(n.from, n.to);
            if (/^\d+\./.test(text)) {
              // Ordered markers keep their number (Obsidian shows it).
              adds.push(markItem(n.from, n.to, 'cm-live-listmark'));
            } else {
              adds.push(replaceItem(n.from, n.to, { widget: new ListBulletWidget('•') }));
            }
          }
        } else if (n.name === 'TaskMarker') {
          if (!onSelectedLine(n.from) && !markerNear(n)) {
            const text = state.doc.sliceString(n.from, n.to).toLowerCase();
            adds.push(replaceItem(n.from, n.to, { widget: new TaskCheckWidget(text.includes('x')) }));
          }
        } else if (n.name === 'HorizontalRule') {
          // Rendered as a slim clickable line (text hidden via font-size: 0,
          // rule drawn with border-top). A line decoration — not a block
          // widget — keeps the `---` reachable by mouse click, and avoids a
          // block widget whose CSS margins would desync CM's height map.
          if (!onSelectedLine(n.from)) {
            const line = state.doc.lineAt(n.from);
            adds.push({ from: line.from, to: line.from, kind: 'line', deco: Decoration.line({ class: 'cm-live-hr-line' }) });
          }
        } else if (n.name === 'Image') {
          if (!onSelectedLine(n.from)) {
            const seg = state.doc.sliceString(n.from, n.to);
            // Support optional title: ![alt](url "title")
            const m = /!\[([^\]]*)\]\(([^)\s]+)(?:\s+"([^"]*)")?\)/.exec(seg);
            if (m) {
              const imageOptions = state.facet(imageOptionsFacet);
              const rawSrc = m[2];
              const isRemote = isRemoteImageSrc(rawSrc);
              const resolvedSrc = isRemote
                ? rawSrc
                : resolveLocalImageUrl(rawSrc, imageOptions);
              adds.push(replaceItem(n.from, n.to, {
                widget: new ImageWidget(resolvedSrc, m[1], m[3], isRemote),
              }));
            }
          }
        }

        stack.push(n);
      },
      leave() {
        stack.pop();
      },
    });

    // ── Regex-based decorations over the visible text slice ──
    const base = vr.from;
    const vtext = state.doc.sliceString(vr.from, vr.to);
    const inCode = (from: number, to: number) =>
      codeRanges.some((r) => from < r.to && to > r.from);
    let m: RegExpExecArray | null;

    // Wiki links: brackets hidden unless the caret is near the link. An alias
    // ([[target|alias]]) renders as just the alias, like Obsidian.
    const wikiRe = /\[\[([^\]\n]+)\]\]/g;
    while ((m = wikiRe.exec(vtext))) {
      const from = base + m.index;
      const to = from + m[0].length;
      if (inCode(from, to)) continue;
      if (onSelectedLine(from) || near(from, to)) {
        adds.push(markItem(from, to, 'cm-live-wiki'));
        continue;
      }
      const pipe = m[1].indexOf('|');
      const visibleFrom = pipe >= 0 ? from + 2 + pipe + 1 : from + 2;
      adds.push(replaceItem(from, visibleFrom, {}));
      adds.push(replaceItem(to - 2, to, {}));
      if (visibleFrom < to - 2) {
        adds.push(markItem(visibleFrom, to - 2, 'cm-live-wiki'));
      }
    }

    // ==highlight== (Obsidian flavour): markers hidden while the caret is away.
    const hlRe = /(?<![=])==([^=\n]+)==(?![=])/g;
    while ((m = hlRe.exec(vtext))) {
      const from = base + m.index;
      const to = from + m[0].length;
      if (inCode(from, to)) continue;
      if (onSelectedLine(from) || near(from, to)) {
        adds.push(markItem(from, to, 'cm-live-highlight'));
      } else {
        adds.push(replaceItem(from, from + 2, {}));
        adds.push(replaceItem(to - 2, to, {}));
        adds.push(markItem(from + 2, to - 2, 'cm-live-highlight'));
      }
    }

    // Block math $$...$$ is rendered by livePreviewBlockField; here we only
    // collect the ranges so inline math/marks inside them are skipped.
    const blockMathRe = /\$\$([\s\S]+?)\$\$/g;
    while ((m = blockMathRe.exec(vtext))) {
      const from = base + m.index;
      const to = from + m[0].length;
      if (inCode(from, to)) continue;
      blockMathRanges.push({ from, to });
    }

    // Inline math uses a wider tolerance: after clicking the rendered widget
    // the caret may land a few chars past the `$...$` range.
    const inlineMathRe = /(?<!\\)\$(?!\$)([^$\n]+?)\$(?!\$)/g;
    while ((m = inlineMathRe.exec(vtext))) {
      const from = base + m.index;
      const to = from + m[0].length;
      if (inCode(from, to)) continue;
      if (blockMathRanges.some((r) => from >= r.from && to <= r.to)) continue;
      if (near(from - 5, to + 5)) continue; // caret inside/adjacent → keep source
      adds.push(replaceItem(from, to, { widget: new MathWidget(m[1].trim(), false) }));
    }

    // Tables are rendered by livePreviewBlockField; here we only collect the
    // ranges so inline decorations inside them get dropped below. Lines inside
    // a code block are never table rows (a fenced block demonstrating markdown
    // table syntax must stay raw code).
    const tableLineRe = /^\s*\|/;
    const firstLine = state.doc.lineAt(vr.from).number;
    const lastLine = state.doc.lineAt(Math.min(vr.to, state.doc.length)).number;
    let row = firstLine;
    while (row <= lastLine) {
      if (!tableLineRe.test(state.doc.line(row).text)) {
        row += 1;
        continue;
      }
      let end = row;
      while (end < lastLine && tableLineRe.test(state.doc.line(end + 1).text)) end += 1;
      const from = state.doc.line(row).from;
      const to = state.doc.line(end).to;
      if (end > row && !inCode(from, to)) {
        tableRanges.push({ from, to });
      }
      row = end + 1;
    }
  }

  // Drop decorations that overlap a rendered table or block-math range
  // (markers/links/inline math inside), which the block field replaces.
  if (tableRanges.length > 0) {
    adds = adds.filter((a) => !tableRanges.some((t) => a.from < t.to && a.to > t.from));
  }
  if (blockMathRanges.length > 0) {
    adds = adds.filter(
      (a) => !blockMathRanges.some((t) => a.from >= t.from && a.to <= t.to)
    );
  }

  // Inner decorations that are fully covered by a replace widget (e.g. image
  // marks inside a rendered image) are redundant and can conflict with the outer
  // widget when they share the same `from` position. Drop them. Line and point
  // decorations carry no text styling and must survive.
  const replaceRanges = adds.filter((a) => a.kind === 'replace');
  adds = adds.filter(
    (a) =>
      (a.kind !== 'mark' && a.kind !== 'replace') ||
      !replaceRanges.some(
        (r) => r !== a && r.from <= a.from && r.to >= a.to && (r.from < a.from || r.to > a.to)
      )
  );

  // RangeSetBuilder requires ranges added in ascending `from` order. If two
  // decorations share the exact same range, keep the replace (which hides
  // content) over a mark so the startSide ordering is deterministic. Line
  // decorations live in their own key space: they legitimately share a
  // position with a point widget or mark at the same line start.
  const seen = new Map<string, number>();
  adds = adds.filter((a, idx) => {
    const key = `${a.kind === 'line' ? 'L' : ''}${a.from}:${a.to}`;
    if (seen.has(key)) {
      const firstIdx = seen.get(key)!;
      if (a.kind === 'replace' && adds[firstIdx].kind !== 'replace') {
        adds[firstIdx] = a;
      }
      return false;
    }
    seen.set(key, idx);
    return true;
  });

  adds.sort((a, b) => a.from - b.from || a.to - b.to);
  for (const { from, to, deco } of adds) {
    builder.add(from, to, deco);
  }

  return builder.finish();
}

/** Block-level decorations (tables, block math). These MUST come from a
 *  StateField — CodeMirror forbids block decorations from view plugins — so
 *  the heavy inline work lives in the viewport-scoped plugin above while this
 *  field runs a few linear full-text scans. Horizontal rules deliberately use
 *  a plugin line decoration instead: a block widget's CSS margins are
 *  invisible to CM's height map (desyncing click coordinates) and the widget
 *  swallowed clicks, making the `---` source unreachable by mouse. */
/** Selected-line ranges as a compact key: every block decoration depends on
 *  the selection only through the lines it touches, so an unchanged key means
 *  a caret move cannot alter the decorations and the full-document rescans
 *  below can be skipped. */
function selectedLinesKey(state: EditorState): string {
  return state.selection.ranges
    .map((r) => {
      const a = state.doc.lineAt(r.from);
      const b = state.doc.lineAt(r.to);
      return `${a.from}:${b.to}`;
    })
    .join(',');
}

type BlockDecoValue = { decorations: DecorationSet; selKey: string };

const livePreviewBlockField = StateField.define<BlockDecoValue>({
  create(state) {
    return { decorations: buildBlockDecorations(state), selKey: selectedLinesKey(state) };
  },
  update(value, tr) {
    if (!tr.docChanged && !tr.selection) {
      return { decorations: value.decorations.map(tr.changes), selKey: value.selKey };
    }
    const selKey = selectedLinesKey(tr.state);
    if (!tr.docChanged) {
      // Caret moved but stayed on the same line(s): decorations unchanged.
      if (selKey === value.selKey) return value;
      // Same drag guard as the inline plugin: keep widgets alive while a
      // non-empty selection is being dragged.
      if (tr.state.selection.ranges.some((r) => !r.empty)) {
        return { decorations: value.decorations, selKey };
      }
    }
    return { decorations: buildBlockDecorations(tr.state), selKey };
  },
  provide: (f) => EditorView.decorations.from(f, (v) => v.decorations),
});

function buildBlockDecorations(state: EditorState): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>();
  const adds: { from: number; to: number; deco: Decoration }[] = [];

  const selectedLineRanges = state.selection.ranges.map((r) => {
    const startLine = state.doc.lineAt(r.from);
    const endLine = state.doc.lineAt(r.to);
    return { from: startLine.from, to: endLine.to };
  });
  const overlapsSelection = (from: number, to: number) =>
    selectedLineRanges.some((l) => from <= l.to && to >= l.from);

  // Code ranges — math / table lines inside fenced or inline code stay raw.
  const codeRanges: { from: number; to: number }[] = [];
  syntaxTree(state).iterate({
    enter(node) {
      if (node.name === 'FencedCode' || node.name === 'InlineCode') {
        codeRanges.push({ from: node.from, to: node.to });
      }
    },
  });
  const inCode = (from: number, to: number) =>
    codeRanges.some((r) => from < r.to && to > r.from);

  const text = state.doc.toString();
  let m: RegExpExecArray | null;

  // Block math $$...$$.
  const blockMathRe = /\$\$([\s\S]+?)\$\$/g;
  while ((m = blockMathRe.exec(text))) {
    const from = m.index;
    const to = from + m[0].length;
    if (inCode(from, to) || overlapsSelection(from, to)) continue;
    const lineFrom = state.doc.lineAt(from).from;
    const lineTo = state.doc.lineAt(to).to;
    const widget = new MathWidget(m[1].trim(), true);
    adds.push({
      from,
      to,
      deco: Decoration.replace(
        lineFrom === from && lineTo === to ? { widget, block: true } : { widget }
      ),
    });
  }

  // Tables: contiguous `|`-led lines, rendered unless the caret is inside.
  // Lines inside a code block are never table rows.
  const tableLineRe = /^\s*\|/;
  let row = 1;
  while (row <= state.doc.lines) {
    if (!tableLineRe.test(state.doc.line(row).text)) {
      row += 1;
      continue;
    }
    let end = row;
    while (end < state.doc.lines && tableLineRe.test(state.doc.line(end + 1).text)) end += 1;
    const from = state.doc.line(row).from;
    const to = state.doc.line(end).to;
    if (end > row && !inCode(from, to) && !overlapsSelection(from, to)) {
      adds.push({
        from,
        to,
        deco: Decoration.replace({
          widget: new TableWidget(parseMarkdownTable(state.doc.sliceString(from, to)), state.doc.sliceString(from, to)),
          block: true,
        }),
      });
    }
    row = end + 1;
  }

  adds.sort((a, b) => a.from - b.from || a.to - b.to);
  for (const a of adds) {
    builder.add(a.from, a.to, a.deco);
  }
  return builder.finish();
}

/** `[[` autocomplete against note titles. */
function wikiAutocomplete(notes: Note[], currentId: string) {
  return autocompletion({
    override: [
      (ctx: CompletionContext) => {
        const before = ctx.state.doc.sliceString(0, ctx.pos);
        const idx = before.lastIndexOf('[[');
        if (idx === -1) return null;
        const afterIdx = before.indexOf(']]', idx);
        if (afterIdx !== -1 && afterIdx < ctx.pos) return null;
        const query = before.slice(idx + 2).toLowerCase();
        if (query.length > 40) return null;
        const matches = notes
          .filter((n) => n.id !== currentId && n.title.toLowerCase().includes(query))
          .slice(0, 8);
        if (matches.length === 0) return null;
        return {
          from: idx + 2,
          options: matches.map((n) => ({
            label: n.title,
            type: 'text',
            // Custom apply: closeBrackets has usually already auto-closed the
            // `[[` with a `]]` sitting right after the cursor. Appending our
            // own `]]` unconditionally produced `[[title]]]]` — so reuse the
            // existing closer when present, otherwise add one.
            apply: (view, _completion, from, to) => {
              const hasCloser = view.state.doc.sliceString(to, to + 2) === ']]';
              const insert = hasCloser ? n.title : `${n.title}]]`;
              view.dispatch({
                changes: { from, to, insert },
                selection: { anchor: from + insert.length + (hasCloser ? 2 : 0) },
              });
            },
          })),
        };
      },
    ],
  });
}

/** Resolve a [[link]] target (title or alias) to a note. */
function resolveTarget(raw: string, notes: Note[]): { id: string | null; title: string } {
  const title = raw.split('|')[0].trim();
  const target = notes.find(
    (n) =>
      n.title.trim().toLowerCase() === title.toLowerCase() ||
      (() => {
        try {
          const aliases = JSON.parse(n.aliases || '[]');
          return Array.isArray(aliases) && aliases.some((a: string) => a.trim().toLowerCase() === title.toLowerCase());
        } catch {
          return false;
        }
      })()
  );
  return { id: target?.id ?? null, title };
}

const editorTheme = EditorView.theme({
  '&': { backgroundColor: 'transparent', fontSize: '16px', height: '100%' },
  '.cm-scroller': { fontFamily: 'inherit', lineHeight: '1.65' },
  '.cm-gutters': {
    backgroundColor: 'transparent',
    borderRight: '1px solid rgba(255,255,255,0.06)',
    color: 'rgba(255,255,255,0.18)',
    paddingRight: '2px',
  },
  '.cm-content': { caretColor: '#F5F5F5', padding: '1rem 0.75rem' },
  '&.cm-focused': { outline: 'none' },
});

// ── Component ──────────────────────────────────────────────────────────

interface Props {
  value: string;
  onChange: (v: string) => void;
  /** 可选：提供则启用 [[wiki]] 补全/点击 */
  notes?: Note[];
  currentNoteId?: string;
  onNavigate?: (id: string) => void;
  onCreateLink?: (title: string) => void;
  /** 编辑器滚动时回调（分屏同步滚动用） */
  onEditorScroll?: () => void;
  /** 暴露 EditorView 实例（分屏同步滚动用） */
  editorRef?: (view: EditorView | null) => void;
  /** 额外扩展（外壳层如光标位置监听等） */
  extensions?: Extension[];
  /** Current vault id used to save pasted/dropped image attachments. */
  vaultId?: string;
  /** Absolute path to the vault attachments directory (for resolving relative image paths). */
  attachmentsDir?: string;
  /** false = source mode: raw markdown without live-preview rendering. */
  livePreview?: boolean;
}

export function MarkdownEditor({
  value,
  onChange,
  notes,
  currentNoteId,
  onNavigate,
  onCreateLink,
  onEditorScroll,
  editorRef,
  extensions,
  vaultId,
  attachmentsDir,
  livePreview = true,
}: Props) {
  // Ctrl/Cmd+click handling: wiki links navigate (or create); external
  // markdown links and bare URLs open in the system browser. A plain click
  // enters edit mode so the user can modify the link without being pulled away.
  const linkClick = useMemo<Extension>(() => {
    const openExternal = (url: string) => {
      shellOpen(url).catch(() => window.open(url, '_blank', 'noopener'));
    };
    return EditorView.domEventHandlers({
      mousedown(event, view) {
        if (!(event.ctrlKey || event.metaKey)) return false;
        const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
        if (pos == null) return false;

        // Wiki link [[target]] / [[target|alias]].
        if (notes) {
          const text = view.state.doc.toString();
          const re = /\[\[([^\]\n]+)\]\]/g;
          let m: RegExpExecArray | null;
          while ((m = re.exec(text))) {
            if (pos >= m.index && pos < m.index + m[0].length) {
              event.preventDefault();
              const { id, title } = resolveTarget(m[1], notes);
              if (id) onNavigate?.(id);
              else onCreateLink?.(title);
              return true;
            }
          }
        }

        // External link: walk the syntax tree up from the click position to
        // find a Link ([text](url)) or bare URL node.
        let node: SyntaxNodeLike | null = syntaxTree(view.state).resolveInner(pos, 0);
        while (node) {
          if (node.name === 'Link') {
            const seg = view.state.doc.sliceString(node.from, node.to);
            const um = /\(([^)\s]+)(?:\s+"[^"]*")?\)/.exec(seg);
            if (um && isRemoteImageSrc(um[1])) {
              event.preventDefault();
              openExternal(um[1]);
              return true;
            }
            return false;
          }
          if (node.name === 'URL') {
            const url = view.state.doc.sliceString(node.from, node.to).replace(/^<|>$/g, '');
            if (isRemoteImageSrc(url)) {
              event.preventDefault();
              openExternal(url);
              return true;
            }
            return false;
          }
          node = node.parent;
        }
        return false;
      },
    });
  }, [notes, onNavigate, onCreateLink]);

  const imageOptions = useMemo<ResolveImageOptions>(() => ({ attachmentsDir }), [attachmentsDir]);

  // Paste / drop image files into the editor, save them to the vault attachments
  // directory, and insert a standard Markdown image link at the cursor.
  const imagePasteDropExtension = useMemo<Extension | null>(() => {
    if (!vaultId) return null;
    return EditorView.domEventHandlers({
      paste(event, view) {
        const clipboardData = event.clipboardData;
        if (!clipboardData) return false;
        const types = Array.from(clipboardData.types || []);
        const hasImage = types.some((t) => t === 'Files' || t.startsWith('image/'));
        if (!hasImage) return false;
        // Read the image straight from the paste payload instead of the system
        // clipboard: the webview receives the real file/bitmap here, while a raw
        // clipboard read fails when the source only put a file list or a format
        // that cannot be decoded (e.g. arboard ConversionFailure on Windows).
        const item = Array.from(clipboardData.items || []).find(
          (it) => it.kind === 'file' && it.type.startsWith('image/'),
        );
        const file =
          item?.getAsFile() ??
          Array.from(clipboardData.files || []).find((f) => f.type.startsWith('image/'));
        if (!file) return false;
        event.preventDefault();
        (async () => {
          try {
            const buffer = await file.arrayBuffer();
            const bytes = Array.from(new Uint8Array(buffer));
            const extByType: Record<string, string> = {
              'image/png': 'png',
              'image/jpeg': 'jpg',
              'image/gif': 'gif',
              'image/webp': 'webp',
              'image/bmp': 'bmp',
              'image/svg+xml': 'svg',
            };
            const ext = file.name ? file.name.split('.').pop() : extByType[file.type] ?? 'png';
            const relPath = await saveAttachmentBytes({
              bytes,
              filename: file.name || `pasted-image.${ext}`,
              vaultId,
            });
            const md = `![Pasted image](${relPath})`;
            view.dispatch({ changes: { from: view.state.selection.main.from, insert: md } });
          } catch (err) {
            console.error('paste image:', err);
          }
        })();
        return true;
      },
      drop(event, view) {
        const files = event.dataTransfer?.files;
        if (!files || files.length === 0) return false;
        const imageFile = Array.from(files).find((f) => f.type.startsWith('image/'));
        if (!imageFile) return false;
        event.preventDefault();
        (async () => {
          try {
            const buffer = await imageFile.arrayBuffer();
            const bytes = Array.from(new Uint8Array(buffer));
            const relPath = await saveAttachmentBytes({
              bytes,
              filename: imageFile.name,
              vaultId,
            });
            const alt = imageFile.name.replace(/\.[^.]+$/, '');
            const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
            const insertPos = pos ?? view.state.selection.main.from;
            view.dispatch({ changes: { from: insertPos, insert: `![${alt}](${relPath})` } });
          } catch (err) {
            console.error('drop image:', err);
          }
        })();
        return true;
      },
    });
  }, [vaultId]);

  const assembled = useMemo(() => {
    const list: Extension[] = [
      markdown({ base: markdownLanguage, codeLanguages: languages }),
      // Live-preview decorations are opt-out: source mode (NoteEditor's
      // 源码模式) shows raw markdown without any rendering.
      ...(livePreview ? [livePreviewPlugin, livePreviewBlockField] as Extension[] : []),
      oneDark,
      editorTheme,
      EditorView.lineWrapping,
      EditorView.domEventHandlers({ scroll: () => onEditorScroll?.() }),
      imageOptionsFacet.of(imageOptions),
      linkClick,
    ];
    if (notes) list.push(wikiAutocomplete(notes, currentNoteId ?? ''));
    if (imagePasteDropExtension) list.push(imagePasteDropExtension);
    if (extensions) list.push(...extensions);
    return list;
  }, [notes, currentNoteId, linkClick, onEditorScroll, extensions, imageOptions, imagePasteDropExtension, livePreview]);

  return (
    <CodeMirror
      ref={(cm) => {
        editorRef?.(cm?.view ?? null);
      }}
      value={value}
      onChange={onChange}
      extensions={assembled}
      height="100%"
      style={{ height: '100%', fontSize: '16px' }}
      basicSetup={{ foldGutter: false, highlightActiveLine: false }}
      className="h-full overflow-hidden"
    />
  );
}
