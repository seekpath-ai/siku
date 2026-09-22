import { useMemo } from 'react';
import CodeMirror from '@uiw/react-codemirror';
import { markdown, markdownLanguage } from '@codemirror/lang-markdown';
import { languages } from '@codemirror/language-data';
import { oneDark } from '@codemirror/theme-one-dark';
import { EditorView, Decoration, DecorationSet, WidgetType, ViewPlugin, keymap, type ViewUpdate } from '@codemirror/view';
import { RangeSetBuilder, StateField, Facet, Prec, type EditorState, type Extension } from '@codemirror/state';
import { syntaxTree, ensureSyntaxTree } from '@codemirror/language';
import { autocompletion, completionStatus, type CompletionContext } from '@codemirror/autocomplete';
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

  // No updateDOM override: eq() equality already reuses the DOM, and the
  // default (false) keeps a changed formula from showing stale output.

  ignoreEvent(event: Event) {
    return event.type === 'mousedown';
  }

  eq(other: MathWidget) {
    return other.latex === this.latex && other.displayMode === this.displayMode;
  }
}

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

// ── Rendered table cell editing ──

/** Editing state of the single cell being edited inside a TableWidget's DOM.
 *  Kept DOM-side (a toDOM closure), not on the widget: `eq()`-based DOM reuse
 *  is exactly what lets an open editor survive unrelated decoration rebuilds. */
interface CellEditingState {
  row: number;
  col: number;
  /** Source text of the cell when editing began; re-validated before write-back. */
  original: string;
  savedHtml: string;
  input: HTMLInputElement;
  cellEl: HTMLTableCellElement;
}

/** Cell focus to restore after a dispatch rebuilds the table widget. Consumed
 *  by the next TableWidget.toDOM whose table shape matches; expires quickly so
 *  a stale record can never hijack an unrelated widget mounted later (e.g. by
 *  scrolling — offscreen block widgets are created lazily). */
interface PendingTableCellFocus {
  row: number;
  col: number;
  /** Expected table shape after the edit — mismatch drops the restore. */
  rows: number;
  cols: number;
}
let pendingTableCellFocus: PendingTableCellFocus | null = null;

function scheduleTableCellFocus(focus: { row: number; col: number }, rows: number, cols: number) {
  const pf: PendingTableCellFocus = { ...focus, rows, cols };
  pendingTableCellFocus = pf;
  setTimeout(() => {
    if (pendingTableCellFocus === pf) pendingTableCellFocus = null;
  }, 1000);
}

/** Sanitize typed cell text for a GFM source cell: single line (GFM cells
 *  cannot span lines), pipes escaped — existing `\|` escapes left alone. */
function sanitizeCellSource(text: string): string {
  // GFM cells are single-line; collapse pasted newlines to spaces.
  const singleLine = text.replace(/\s*\n\s*/g, ' ');
  // Escape pipes, leaving existing `\|` escapes untouched.
  return singleLine
    .split('\\|')
    .map((part) => part.replace(/\|/g, '\\|'))
    .join('\\|');
}

type TableChange = { from: number; to?: number; insert?: string };
const byChangeFrom = (a: TableChange, b: TableChange) => a.from - b.from;

/** Renders a GFM table block as an HTML <table> with Obsidian-style cell
 *  editing: clicking a cell focuses a single-cell editor (the raw source text
 *  of that cell), committing writes back into the cell's source range.
 *  Structural controls (add/remove row/column) rewrite the source too. Every
 *  mutation goes through view.dispatch (undo history); a pending-focus record
 *  re-opens the right cell editor after the rebuild a dispatch causes.
 *
 *  Editing interactions never move the CodeMirror selection, so the
 *  overlapsSelection guard in buildBlockDecorations does not tear the widget
 *  down mid-edit; moving the caret into the table source with the keyboard
 *  (or clicking the widget padding) still reveals the raw markdown. */
class TableWidget extends WidgetType {
  constructor(readonly html: string, readonly markdown: string, readonly table: TableInfo) {
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

    // Structural controls stay usable while a cell editor is open: their
    // dispatch rebuilds the widget and the pending-focus record restores it.
    const addRowBtn = document.createElement('button');
    addRowBtn.className = 'cm-live-table-addrow';
    addRowBtn.title = '添加行';
    addRowBtn.textContent = '+';
    wrap.appendChild(addRowBtn);

    const addColBtn = document.createElement('button');
    addColBtn.className = 'cm-live-table-addcol';
    addColBtn.title = '添加列';
    addColBtn.textContent = '+';
    wrap.appendChild(addColBtn);

    // Floating delete handles, positioned over the hovered row/column.
    const delRowBtn = document.createElement('button');
    delRowBtn.className = 'cm-live-table-delrow';
    delRowBtn.title = '删除行';
    delRowBtn.textContent = '×';
    const delColBtn = document.createElement('button');
    delColBtn.className = 'cm-live-table-delcol';
    delColBtn.title = '删除列';
    delColBtn.textContent = '×';
    wrap.appendChild(delRowBtn);
    wrap.appendChild(delColBtn);

    let editing: CellEditingState | null = null;

    // this.table's offsets were captured when the decorations were built and
    // can go stale when the DOM is reused across an unrelated edit (eq() is
    // content-based). Everything that reads or writes the source therefore
    // re-resolves the table from the widget's live DOM position instead.
    const resolveTable = (): TableInfo | null => {
      if (!wrap.isConnected) return null;
      const pos = view.posAtDOM(wrap);
      return findTables(view.state).find((t) => pos >= t.from && pos <= t.to) ?? null;
    };

    // DOM cell ↔ source coordinates: source rows are header + delimiter +
    // body rows, and the delimiter row is not rendered, so tbody row indices
    // shift by 2.
    const srcRowColOfCell = (cell: HTMLTableCellElement): { row: number; col: number } | null => {
      const tr = cell.parentElement as HTMLTableRowElement | null;
      if (!tr) return null;
      const row = tr.parentElement?.tagName === 'THEAD' ? 0 : tr.sectionRowIndex + 2;
      return { row, col: cell.cellIndex };
    };
    const cellDomAt = (row: number, col: number): HTMLTableCellElement | null => {
      const tr =
        row === 0
          ? wrap.querySelector('thead tr')
          : wrap.querySelectorAll('tbody tr')[row - 2];
      const cell = tr?.children[col];
      return cell instanceof HTMLTableCellElement ? cell : null;
    };

    /** The pending edit as a source change, or null when clean/invalid.
     *  Re-validates the cell's current source against what the editor
     *  started from, so a table that changed externally never gets a write
     *  to the wrong range — the edit is dropped instead. */
    const buildEditChange = (table: TableInfo): TableChange | null => {
      if (!editing) return null;
      const rowInfo = table.rows[editing.row];
      const cell = rowInfo?.cells[editing.col];
      if (!rowInfo || rowInfo.kind === 'delimiter' || !cell) return null;
      if (view.state.doc.sliceString(cell.from, cell.to) !== editing.original) return null;
      const insert = sanitizeCellSource(editing.input.value);
      return insert === editing.original ? null : { from: cell.from, to: cell.to, insert };
    };

    const clearEditing = () => {
      if (!editing) return;
      const { cellEl, savedHtml } = editing;
      editing = null;
      cellEl.classList.remove('cm-live-table-cell-editing');
      cellEl.innerHTML = savedHtml;
    };

    const enterEdit = (row: number, col: number) => {
      if (editing && editing.row === row && editing.col === col) return;
      const table = resolveTable() ?? this.table;
      const rowInfo = table.rows[row];
      const cell = rowInfo?.cells[col];
      const cellEl = cellDomAt(row, col);
      if (!rowInfo || rowInfo.kind === 'delimiter' || !cell || !cellEl) return;
      const input = document.createElement('input');
      input.className = 'cm-live-table-input';
      const original = view.state.doc.sliceString(cell.from, cell.to);
      input.value = original;
      input.addEventListener('keydown', (e) => {
        e.stopPropagation(); // belt-and-braces: ignoreEvent already shields CM
        if (e.isComposing) return; // IME candidate keys must not commit/navigate
        if (e.key === 'Tab') {
          e.preventDefault();
          moveCellFocus(e.shiftKey ? -1 : 1);
        } else if (e.key === 'Enter' || e.key === 'Escape') {
          e.preventDefault();
          commitAndExit(true);
        }
      });
      input.addEventListener('blur', () => {
        // Defer: a blur caused by our own dispatch rebuilding the widget must
        // not dispatch again inside CodeMirror's update cycle.
        setTimeout(() => {
          if (editing && editing.input === input) commitAndExit();
        }, 0);
      });
      editing = { row, col, original, savedHtml: cellEl.innerHTML, input, cellEl };
      cellEl.classList.add('cm-live-table-cell-editing');
      cellEl.textContent = '';
      cellEl.appendChild(input);
      input.focus({ preventScroll: true });
      input.setSelectionRange(original.length, original.length);
    };

    const commitAndExit = (focusEditor = false) => {
      const table = resolveTable();
      const change = table ? buildEditChange(table) : null;
      clearEditing();
      if (change) view.dispatch({ changes: change });
      if (focusEditor) view.focus();
    };

    /** Commit the open editor (if dirty) and move the cell editor to
     *  (row, col) — either in place, or through a rebuild + pending focus. */
    const commitAndSwitch = (row: number, col: number) => {
      const table = resolveTable() ?? this.table;
      const change = buildEditChange(table);
      clearEditing();
      if (change) {
        scheduleTableCellFocus({ row, col }, table.rows.length, table.rows[0]?.cells.length ?? 0);
        view.dispatch({ changes: change });
      } else {
        enterEdit(row, col);
      }
    };

    const moveCellFocus = (dir: 1 | -1) => {
      if (!editing) return;
      const table = resolveTable() ?? this.table;
      const rowInfo = table.rows[editing.row];
      if (!rowInfo) return;
      const stepRow = (ri: number) => {
        let n = ri + dir;
        while (n >= 0 && n < table.rows.length && table.rows[n].kind === 'delimiter') n += dir;
        return n;
      };
      let nr = editing.row;
      let nc = editing.col + dir;
      if (nc >= rowInfo.cells.length) {
        nr = stepRow(editing.row);
        nc = 0;
      } else if (nc < 0) {
        nr = stepRow(editing.row);
        nc = nr >= 0 ? table.rows[nr].cells.length - 1 : 0;
      }
      if (nr >= table.rows.length) {
        // Tab past the last cell: append an empty row and land in its first
        // cell — same shape as the source-mode tableKeymap behaviour.
        const cols = table.rows[0]?.cells.length ?? 1;
        const changes = addTableRowChanges(table);
        const change = buildEditChange(table);
        if (change) changes.push(change);
        clearEditing();
        scheduleTableCellFocus({ row: table.rows.length, col: 0 }, table.rows.length + 1, cols);
        view.dispatch({ changes: changes.sort(byChangeFrom) });
        return;
      }
      if (nr < 0 || !table.rows[nr].cells[nc]) return; // first cell / ragged row: stay
      commitAndSwitch(nr, nc);
    };

    /** Structural control handler: combine the pending cell edit and the
     *  structural change into ONE dispatch (single undo step), then restore
     *  the cell editor at `focus` after the rebuild. */
    const runStructural = (
      changes: TableChange[],
      focus: { row: number; col: number } | null,
      rowsAfter: number,
      colsAfter: number,
      dropEdit: boolean
    ) => {
      if (changes.length === 0) return;
      if (!dropEdit) {
        const table = resolveTable();
        const change = table ? buildEditChange(table) : null;
        if (change) changes.push(change);
      }
      clearEditing();
      if (focus) scheduleTableCellFocus(focus, rowsAfter, colsAfter);
      view.dispatch({ changes: changes.sort(byChangeFrom) });
    };

    // Hovered cell translated back to source coordinates.
    let hoverSrcRow = -1;
    let hoverCol = -1;
    wrap.addEventListener('mouseover', (e) => {
      const target = e.target as HTMLElement;
      const cell = target.closest('td, th') as HTMLTableCellElement | null;
      if (!cell || !wrap.contains(cell)) return; // over a control: keep as-is
      const rc = srcRowColOfCell(cell);
      if (!rc) return;
      hoverSrcRow = rc.row;
      hoverCol = rc.col;
      const wrapRect = wrap.getBoundingClientRect();
      const rowInfo = this.table.rows[hoverSrcRow];
      const tr = cell.parentElement as HTMLTableRowElement;
      if (rowInfo && rowInfo.kind === 'body') {
        const r = tr.getBoundingClientRect();
        delRowBtn.style.top = `${r.top - wrapRect.top + r.height / 2}px`;
        delRowBtn.classList.add('visible');
      } else {
        delRowBtn.classList.remove('visible');
      }
      const colCount = this.table.rows[0]?.cells.length ?? 0;
      const tableEl = wrap.querySelector('table');
      if (tableEl && hoverCol >= 0 && hoverCol < colCount && colCount > 1) {
        const c = cell.getBoundingClientRect();
        const t = tableEl.getBoundingClientRect();
        delColBtn.style.left = `${c.left - wrapRect.left + c.width / 2}px`;
        delColBtn.style.top = `${t.top - wrapRect.top}px`;
        delColBtn.classList.add('visible');
      } else {
        delColBtn.classList.remove('visible');
      }
    });
    wrap.addEventListener('mouseleave', () => {
      delRowBtn.classList.remove('visible');
      delColBtn.classList.remove('visible');
    });

    wrap.addEventListener('mousedown', (e) => {
      const target = e.target as HTMLElement;
      // Clicks inside the active cell editor keep native behaviour (caret
      // placement, drag-selecting the text).
      if (target.closest('.cm-live-table-input')) return;
      e.preventDefault();
      e.stopPropagation();
      // Buttons must be handled HERE, on mousedown: preventDefault keeps the
      // focus where it is, so no blur races the handlers below.
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
      if (target.closest('.cm-live-table-addrow')) {
        const table = resolveTable();
        if (table) {
          const cols = table.rows[0]?.cells.length ?? 1;
          // Land in the new row's first cell, ready to type.
          runStructural(addTableRowChanges(table), { row: table.rows.length, col: 0 }, table.rows.length + 1, cols, false);
        }
        return;
      }
      if (target.closest('.cm-live-table-addcol')) {
        const table = resolveTable();
        if (table) {
          const cols = table.rows[0]?.cells.length ?? 1;
          runStructural(
            addTableColumnChanges(view.state, table),
            editing ? { row: editing.row, col: editing.col } : null,
            table.rows.length,
            cols + 1,
            false
          );
        }
        return;
      }
      if (target.closest('.cm-live-table-delrow')) {
        const table = resolveTable();
        if (table && hoverSrcRow >= 0) {
          const dropEdit = editing?.row === hoverSrcRow;
          runStructural(
            deleteTableRowChanges(view.state, table, hoverSrcRow),
            editing && !dropEdit
              ? { row: editing.row > hoverSrcRow ? editing.row - 1 : editing.row, col: editing.col }
              : null,
            table.rows.length - 1,
            table.rows[0]?.cells.length ?? 0,
            dropEdit
          );
        }
        return;
      }
      if (target.closest('.cm-live-table-delcol')) {
        const table = resolveTable();
        if (table && hoverCol >= 0) {
          const dropEdit = editing?.col === hoverCol;
          runStructural(
            deleteTableColumnChanges(view.state, table, hoverCol),
            editing && !dropEdit
              ? { row: editing.row, col: editing.col > hoverCol ? editing.col - 1 : editing.col }
              : null,
            table.rows.length,
            (table.rows[0]?.cells.length ?? 1) - 1,
            dropEdit
          );
        }
        return;
      }
      const cellEl = target.closest('td, th') as HTMLTableCellElement | null;
      if (cellEl && wrap.contains(cellEl)) {
        const rc = srcRowColOfCell(cellEl);
        if (rc && !(editing && editing.row === rc.row && editing.col === rc.col)) {
          commitAndSwitch(rc.row, rc.col);
        }
        return;
      }
      // Escape hatch kept from the source-click behaviour: clicking the
      // widget's padding moves the caret to the table source, which tears the
      // widget down into raw markdown. Commit the open editor first so the
      // rebuild triggered here cannot swallow it.
      const table = resolveTable();
      const change = table ? buildEditChange(table) : null;
      clearEditing();
      view.dispatch({
        changes: change ?? [],
        selection: { anchor: view.posAtDOM(wrap) },
        scrollIntoView: true,
      });
    });

    // A dispatch we issued rebuilt this widget: re-open the cell editor that
    // was targeted, when the rebuilt table still has the expected shape.
    if (pendingTableCellFocus) {
      const pf = pendingTableCellFocus;
      pendingTableCellFocus = null;
      const rows = this.table.rows;
      const ok =
        rows.length === pf.rows &&
        (rows[0]?.cells.length ?? 0) === pf.cols &&
        pf.row >= 0 &&
        pf.row < rows.length &&
        rows[pf.row].kind !== 'delimiter' &&
        pf.col >= 0 &&
        pf.col < rows[pf.row].cells.length;
      if (ok) {
        // toDOM runs before the widget is attached; focus needs it mounted.
        requestAnimationFrame(() => {
          if (wrap.isConnected) enterEdit(pf.row, pf.col);
        });
      }
    }
    return wrap;
  }

  // Rough height for not-yet-measured offscreen tables: rows + padding.
  get estimatedHeight() {
    return (this.html.split('<tr').length - 1) * 34 + 12;
  }

  // NOTE: no updateDOM override. CM calls it as a LAST-RESORT DOM reuse check
  // when eq() was false (WidgetBuffer.findWidget pass 1) — returning true
  // there would keep the old table's DOM showing stale content after edits.

  ignoreEvent(event: Event) {
    if (event.type === 'mousedown') return true;
    // Everything targeted at the open cell editor (keys, paste, selection)
    // belongs to the input — CodeMirror must not run its keymaps on it.
    const target = event.target;
    return target instanceof HTMLElement && !!target.closest('.cm-live-table-cell-editing');
  }

  // Edit state deliberately NOT compared here: it lives in the DOM, and
  // content-equal rebuilds reusing the DOM (eq true) are exactly what keeps
  // an open cell editor alive across unrelated decoration rebuilds.
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
  // No updateDOM override: eq() equality already reuses the DOM (so a stable
  // image is not re-fetched); the default (false) redraws a changed one.
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

/**
 * Inline markdown inside a rendered table cell.
 *
 * The live preview styles inline constructs through CodeMirror decorations,
 * which cannot reach widget DOM — so cells were plain `escapeHtml` and a cell
 * like `` `previous_response_id` `` kept its backticks, while the reading view
 * (react-markdown) rendered it as code. HTML is escaped first, then the few
 * inline forms that actually show up in table cells are turned into elements.
 * Italic is deliberately not handled: `_` appears inside identifiers
 * (`previous_response_id`), where a naive `_x_` rule would italicise the middle
 * of a word.
 */
function inlineCellHtml(text: string): string {
  // The classes are the live preview's own (see index.css): widget DOM sits
  // outside CodeMirror's decorations, so reusing them is what keeps a cell's
  // inline code looking like inline code elsewhere in the editor.
  let html = escapeHtml(text);
  html = html.replace(/`([^`]+)`/g, '<code class="cm-live-code">$1</code>');
  html = html.replace(/\*\*([^*]+)\*\*/g, '<strong class="cm-live-strong">$1</strong>');
  html = html.replace(
    /\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g,
    '<a class="cm-live-link" href="$2" target="_blank" rel="noreferrer">$1</a>'
  );
  return html;
}

/**
 * Split one table row into cells, keeping source offsets.
 *
 * A naive `split('|')` cuts a cell like `` `a|b` `` in half, and pipes inside
 * inline code are common (shell pipelines, `a || b`, type unions). The reading
 * view (remark-gfm) keeps them, so the live preview has to as well: pipes
 * inside a backtick span are ignored, and GFM's `\|` escape yields a literal
 * `|`. Backtick runs are matched by length, so `` ``a|b`` `` works too.
 *
 * The returned spans carry document offsets (when `lineFrom` is given) so the
 * structural table commands (add/remove row/column, Tab navigation) can target
 * cells precisely; `rawFrom`/`rawTo` cover the untrimmed text between the
 * surrounding delimiter pipes.
 */
interface TableCellSpan {
  /** Cell text with `\|` unescaped and surrounding whitespace trimmed. */
  text: string;
  /** Trimmed content range; collapsed to a point for empty cells. */
  from: number;
  to: number;
  /** Untrimmed range between the surrounding delimiter pipes. */
  rawFrom: number;
  rawTo: number;
}

function splitTableRowSpans(line: string, lineFrom = 0): TableCellSpan[] {
  let start = 0;
  while (start < line.length && (line[start] === ' ' || line[start] === '\t')) start++;
  if (line[start] === '|') start++;
  let end = line.length;
  while (end > start && (line[end - 1] === ' ' || line[end - 1] === '\t')) end--;
  if (end > start && line[end - 1] === '|' && line[end - 2] !== '\\') end--;

  const out: TableCellSpan[] = [];
  let rawFrom = start;
  let cur = '';
  let contentFrom = -1;
  let contentTo = -1;
  let fence: string | null = null;

  const flush = (rawTo: number) => {
    let from: number;
    let to: number;
    if (contentFrom === -1) {
      // Empty cell: collapse to just after the opening pipe's space, so Tab
      // lands at a natural typing position.
      const p = rawFrom < rawTo && line[rawFrom] === ' ' ? rawFrom + 1 : rawFrom;
      from = to = lineFrom + p;
    } else {
      from = lineFrom + contentFrom;
      to = lineFrom + contentTo;
    }
    out.push({ text: cur.trim(), from, to, rawFrom: lineFrom + rawFrom, rawTo: lineFrom + rawTo });
    cur = '';
    contentFrom = -1;
    contentTo = -1;
  };

  for (let i = start; i < end; i++) {
    const ch = line[i];
    if (ch === '\\' && line[i + 1] === '|') {
      if (contentFrom === -1) contentFrom = i;
      contentTo = i + 2;
      cur += '|';
      i += 1;
      continue;
    }
    if (ch === '`') {
      let n = 1;
      while (line[i + n] === '`') n += 1;
      const run = '`'.repeat(n);
      if (fence === null) fence = run;
      else if (run === fence) fence = null;
      if (contentFrom === -1) contentFrom = i;
      contentTo = i + n;
      cur += run;
      i += n - 1;
      continue;
    }
    if (ch === '|' && fence === null) {
      flush(i);
      rawFrom = i + 1;
      continue;
    }
    cur += ch;
    if (ch !== ' ' && ch !== '\t') {
      if (contentFrom === -1) contentFrom = i;
      contentTo = i + 1;
    }
  }
  flush(end);
  return out;
}

function splitTableRow(line: string): string[] {
  return splitTableRowSpans(line).map((c) => c.text);
}

/** Render a GFM table block (lines of `|`-separated cells) as HTML.
 *  Each cell gets a hover-only copy button (rendered tables swallow
 *  mousedown for click-to-edit, so without it cell text cannot be copied
 *  at all). */
function parseMarkdownTable(text: string): string {
  const lines = text.split('\n').map((l) => l.trim());
  const cells = (line: string) => splitTableRow(line);
  const header = cells(lines[0]);
  const isAlignRow = lines.length > 1 && /^[\s:|-]+$/.test(lines[1]) && lines[1].includes('-');
  const body = lines.slice(isAlignRow ? 2 : 1);
  const cellBtn = `<button class="cm-live-table-cellcopy" title="复制单元格">${COPY_ICON}</button>`;

  let html = '<table><thead><tr>';
  html += header.map((c) => `<th>${inlineCellHtml(c)}${cellBtn}</th>`).join('');
  html += '</tr></thead>';
  if (body.length > 0) {
    html += '<tbody>';
    for (const row of body) {
      html += '<tr>' + cells(row).map((c) => `<td>${inlineCellHtml(c)}${cellBtn}</td>`).join('') + '</tr>';
    }
    html += '</tbody>';
  }
  html += '</table>';
  return html;
}

// ── Structured table model (shared by rendering, controls and keymap) ──

interface TableRowInfo {
  kind: 'header' | 'delimiter' | 'body';
  /** Document range of the whole source line. */
  from: number;
  to: number;
  cells: TableCellSpan[];
}

interface TableInfo {
  /** Document range covering every source line of the table. */
  from: number;
  to: number;
  /** Header row, delimiter row, then body rows — in source order. */
  rows: TableRowInfo[];
}

function tableRowInfo(state: EditorState, kind: TableRowInfo['kind'], from: number): TableRowInfo {
  const line = state.doc.lineAt(from);
  return { kind, from: line.from, to: line.to, cells: splitTableRowSpans(line.text, line.from) };
}

/** Regex fallback for findTables when the syntax tree cannot be produced in
 *  time (huge document on a slow tick). Mirrors the pre-tree detection: runs
 *  of `|`-led lines, fenced code still excluded via the partial tree. */
function findTablesFallback(state: EditorState): TableInfo[] {
  const codeRanges: { from: number; to: number }[] = [];
  syntaxTree(state).iterate({
    enter(node) {
      if (node.name === 'FencedCode') codeRanges.push({ from: node.from, to: node.to });
    },
  });
  const inCode = (from: number, to: number) =>
    codeRanges.some((r) => from < r.to && to > r.from);

  const tableLineRe = /^\s*\|/;
  const out: TableInfo[] = [];
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
    if (end > row && !inCode(from, to)) {
      const rows: TableRowInfo[] = [];
      for (let ln = row; ln <= end; ln++) {
        const line = state.doc.line(ln);
        let kind: TableRowInfo['kind'] = ln === row ? 'header' : 'body';
        if (ln === row + 1 && /^[\s:|-]+$/.test(line.text.trim()) && line.text.includes('-')) {
          kind = 'delimiter';
        }
        rows.push(tableRowInfo(state, kind, line.from));
      }
      out.push({ from, to, rows });
    }
    row = end + 1;
  }
  return out;
}

/** Find every GFM table in the document with per-row, per-cell source ranges.
 *
 *  Detection walks the lezer syntax tree (`Table` → `TableHeader` /
 *  `TableDelimiter` / `TableRow` nodes; GFM is enabled in markdownLanguage),
 *  so pipe-looking lines inside fenced code blocks are excluded for free and
 *  detection matches what the reading view (remark-gfm) renders — a run of
 *  `|` lines without a `---` delimiter row is plain text, not a table. Cell
 *  spans come from splitTableRowSpans (not the tree's TableCell nodes), so
 *  empty cells get ranges too and `\|`/backtick pipes never split a cell.
 *
 *  Tables nested in a blockquote/list carry `> ` etc. before the row content;
 *  the renderer and the edit commands assume plain pipe lines, so those are
 *  skipped (same as the old `^\s*\|` detection).
 */
function findTables(state: EditorState): TableInfo[] {
  const tree = ensureSyntaxTree(state, state.doc.length, 100);
  if (!tree) return findTablesFallback(state);
  const tables: TableInfo[] = [];
  tree.iterate({
    enter(node) {
      if (node.name !== 'Table') return;
      const rows: TableRowInfo[] = [];
      let plain = true;
      for (let ch = node.node.firstChild; ch; ch = ch.nextSibling) {
        const kind =
          ch.name === 'TableHeader' ? 'header'
          : ch.name === 'TableDelimiter' ? 'delimiter'
          : ch.name === 'TableRow' ? 'body'
          : null;
        if (!kind) continue;
        const line = state.doc.lineAt(ch.from);
        if (state.doc.sliceString(line.from, ch.from).trim() !== '') {
          plain = false;
          break;
        }
        rows.push(tableRowInfo(state, kind, ch.from));
      }
      if (plain && rows.length > 0) {
        tables.push({ from: rows[0].from, to: rows[rows.length - 1].to, rows });
      }
      return false; // table cells don't nest tables
    },
  });
  return tables;
}

/** Locate the table row containing `pos`, if any. */
function tableAtPos(
  state: EditorState,
  pos: number
): { table: TableInfo; rowIndex: number; row: TableRowInfo } | null {
  for (const table of findTables(state)) {
    if (pos < table.from || pos > table.to) continue;
    for (let i = 0; i < table.rows.length; i++) {
      const row = table.rows[i];
      if (pos >= row.from && pos <= row.to) return { table, rowIndex: i, row };
    }
  }
  return null;
}

// ── Structural table edits (all dispatched as source rewrites → undo history).
// These return change lists instead of dispatching, so the table widget can
// merge a pending cell edit and a structural change into one transaction. ──

function emptyTableRowSource(cols: number): string {
  return '|' + '  |'.repeat(Math.max(1, cols));
}

/** Append an empty row at the end of the table. */
function addTableRowChanges(table: TableInfo): TableChange[] {
  const cols = table.rows[0]?.cells.length ?? 1;
  return [{ from: table.to, insert: '\n' + emptyTableRowSource(cols) }];
}

/** Append an empty column at the right edge of every row (the delimiter row
 *  gets a `---` cell). */
function addTableColumnChanges(state: EditorState, table: TableInfo): TableChange[] {
  return table.rows.map((row) => {
    const text = state.doc.sliceString(row.from, row.to);
    const trimmedEnd = row.from + text.replace(/\s+$/, '').length;
    const cell = row.kind === 'delimiter' ? ' --- ' : '  ';
    return { from: trimmedEnd, insert: text.trimEnd().endsWith('|') ? `${cell}|` : ` |${cell}|` };
  });
}

/** Delete a body row (header/delimiter rows are refused: removing either
 *  would break the GFM table). */
function deleteTableRowChanges(state: EditorState, table: TableInfo, rowIndex: number): TableChange[] {
  const row = table.rows[rowIndex];
  if (!row || row.kind !== 'body') return [];
  let { from, to } = row;
  if (to < state.doc.length) to += 1; // swallow the trailing newline
  else if (from > table.from) from -= 1; // last line of the doc: swallow the preceding one
  return [{ from, to }];
}

/** Delete the column at `colIndex` from every row. Data rows lose the cell
 *  plus one adjacent pipe (keeping at least one pipe so the line stays a
 *  table row); the delimiter row is rebuilt from its remaining cells so it
 *  stays a valid GFM delimiter line. Ragged rows missing that cell are left
 *  untouched. */
function deleteTableColumnChanges(state: EditorState, table: TableInfo, colIndex: number): TableChange[] {
  const colCount = table.rows[0]?.cells.length ?? 0;
  if (colCount <= 1 || colIndex < 0 || colIndex >= colCount) return [];
  const doc = state.doc;
  const changes: TableChange[] = [];
  for (const row of table.rows) {
    const cell = row.cells[colIndex];
    if (!cell) continue;
    if (row.kind === 'delimiter') {
      const cells = row.cells.filter((_, j) => j !== colIndex).map((c) => c.text || '---');
      let insert: string;
      if (cells.length <= 1) {
        insert = `| ${cells[0] ?? '---'} |`;
      } else {
        const line = doc.sliceString(row.from, row.to);
        insert = `${/^\s*\|/.test(line) ? '| ' : ''}${cells.join(' | ')}${/\|\s*$/.test(line) ? ' |' : ''}`;
      }
      changes.push({ from: row.from, to: row.to, insert });
      continue;
    }
    let { rawFrom: from, rawTo: to } = cell;
    if (doc.sliceString(to, to + 1) === '|') {
      to += 1; // cell + the pipe after it
    } else if (
      from > row.from &&
      doc.sliceString(from - 1, from) === '|' &&
      doc.sliceString(row.from, from - 1).includes('|')
    ) {
      from -= 1; // cell + the pipe before it, but never the row's last pipe
    }
    changes.push({ from, to });
  }
  return changes;
}

// ── In-table key bindings (Tab / Shift-Tab / Enter) ──

/** Move the caret one cell forward (dir=1) or backward (dir=-1). Tab past the
 *  last cell wraps to the next row's first cell, appending a fresh empty row
 *  at the bottom of the table when there is none. Returns false outside
 *  tables so the default bindings (indent etc.) keep working. */
function moveTableCell(view: EditorView, dir: 1 | -1): boolean {
  const { state } = view;
  const sel = state.selection.main;
  if (!sel.empty) return false;
  const hit = tableAtPos(state, sel.head);
  if (!hit) return false;
  const { table, rowIndex, row } = hit;

  let idx: number;
  if (row.kind === 'delimiter') {
    idx = dir === 1 ? row.cells.length : -1; // step straight to the adjacent row
  } else {
    idx = row.cells.findIndex((c) => sel.head >= c.rawFrom && sel.head <= c.rawTo);
    if (idx === -1) {
      // Caret sits on a delimiter pipe or outside the cells: take the nearest
      // cell in the travel direction.
      const next = row.cells.findIndex((c) => c.rawFrom > sel.head);
      const bound = next === -1 ? row.cells.length : next;
      idx = dir === 1 ? bound : bound - 1;
    } else {
      idx += dir;
    }
  }

  const stepRow = (ri: number) => {
    let n = ri + dir;
    while (n >= 0 && n < table.rows.length && table.rows[n].kind === 'delimiter') n += dir;
    return n;
  };

  let r = rowIndex;
  let c = idx;
  if (c >= row.cells.length) {
    const nr = stepRow(rowIndex);
    if (nr >= table.rows.length) {
      const cols = table.rows[0]?.cells.length ?? 1;
      const newRow = emptyTableRowSource(cols);
      view.dispatch({
        changes: { from: table.to, insert: '\n' + newRow },
        selection: { anchor: table.to + 1 + 2 }, // after the new row's "| "
        scrollIntoView: true,
      });
      return true;
    }
    r = nr;
    c = 0;
  } else if (c < 0) {
    const pr = stepRow(rowIndex);
    if (pr < 0) return true; // already on the first cell: swallow Shift-Tab
    r = pr;
    c = table.rows[pr].cells.length - 1;
  }
  const target = table.rows[r]?.cells[c];
  if (!target) return true; // ragged row: stay put, but keep Tab from indenting
  view.dispatch({ selection: { anchor: target.from }, scrollIntoView: true });
  return true;
}

/** Enter at the end of a table row inserts a fresh empty row below it (below
 *  the delimiter line when pressed on the header) and moves the caret into
 *  its first cell. Mid-line Enter falls through to the default newline. */
function insertTableRowBelow(view: EditorView): boolean {
  const { state } = view;
  const sel = state.selection.main;
  if (!sel.empty) return false;
  if (completionStatus(state) === 'active') return false; // let Enter accept completions
  const hit = tableAtPos(state, sel.head);
  if (!hit) return false;
  if (sel.head !== hit.row.to) return false;
  const anchorRow =
    hit.row.kind === 'header' && hit.table.rows[1]?.kind === 'delimiter'
      ? hit.table.rows[1]
      : hit.row;
  const cols = hit.table.rows[0]?.cells.length ?? 1;
  const newRow = emptyTableRowSource(cols);
  view.dispatch({
    changes: { from: anchorRow.to, insert: '\n' + newRow },
    selection: { anchor: anchorRow.to + 1 + 2 }, // after the new row's "| "
    scrollIntoView: true,
  });
  return true;
}

const tableKeymap = Prec.high(
  keymap.of([
    { key: 'Tab', run: (view) => moveTableCell(view, 1) },
    { key: 'Shift-Tab', run: (view) => moveTableCell(view, -1) },
    { key: 'Enter', run: insertTableRowBelow },
  ])
);

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
  // inline decorations get dropped. Table detection shares the syntax-tree
  // based findTables with the block field (fenced code excluded for free).
  const tableRanges: { from: number; to: number }[] = findTables(state);
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
  const codeRanges: { from: number; to: number; block: boolean }[] = [];
  syntaxTree(state).iterate({
    enter(node) {
      if (node.name === 'FencedCode' || node.name === 'InlineCode') {
        codeRanges.push({ from: node.from, to: node.to, block: node.name === 'FencedCode' });
      }
    },
  });
  // Block widgets (tables, $$…$$) are only blocked by *block* code. Gating them
  // on InlineCode too broke valid tables: one stray backtick above the table
  // makes Lezer extend an InlineCode node over it, so the table fell back to
  // source — while the reading view (remark) renders it fine, because an
  // unmatched backtick stays literal text there. Inline code is a span and
  // cannot contain a block, so it must not block a block widget.
  const inFencedCode = (from: number, to: number) =>
    codeRanges.some((r) => r.block && from < r.to && to > r.from);

  const text = state.doc.toString();
  let m: RegExpExecArray | null;

  // Block math $$...$$.
  const blockMathRe = /\$\$([\s\S]+?)\$\$/g;
  while ((m = blockMathRe.exec(text))) {
    const from = m.index;
    const to = from + m[0].length;
    if (inFencedCode(from, to) || overlapsSelection(from, to)) continue;
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

  // Tables: syntax-tree GFM tables (see findTables), rendered unless the
  // caret is inside. Pipe lines inside fenced code never reach here — the
  // tree does not parse them as tables.
  for (const table of findTables(state)) {
    if (overlapsSelection(table.from, table.to)) continue;
    const source = state.doc.sliceString(table.from, table.to);
    adds.push({
      from: table.from,
      to: table.to,
      deco: Decoration.replace({
        widget: new TableWidget(parseMarkdownTable(source), source, table),
        block: true,
      }),
    });
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
      // Structural table keys (Tab/Shift-Tab/Enter between cells). Active only
      // on table source lines — the handlers return false elsewhere — so this
      // is useful in source mode too.
      tableKeymap,
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
