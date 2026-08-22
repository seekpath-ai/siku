import { create } from 'zustand';
import type { SnippetRect, TextQuote } from '@/components/reader/PdfViewer';

/** One contiguous segment of a multi-range (discontinuous) selection. */
export interface SnippetSegment {
  pageIndex: number; // PDF page number (1-based)
  rects: SnippetRect[];
  text: string;
  quote: TextQuote;
}

export interface Snippet {
  id: string;
  paperId: string;
  pageIndex: number; // PDF page number (1-based)
  yRatio: number; // vertical position ratio within page (0=top, 1=bottom), zoom-independent
  xRatio: number; // horizontal position ratio within page (0=left, 1=right), used for column ordering
  heightRatio: number; // relative height within page, for highlight overlay
  widthRatio: number; // relative width within page, for highlight overlay
  /** Per-line/per-column rects for the highlight overlay. Absent for legacy rows. */
  rects?: SnippetRect[];
  /** Segments of a multi-range selection (may span pages). Absent for single-range snippets. */
  segments?: SnippetSegment[];
  text: string;
  note: string;
  tags: string[];
  translation: string | null;
  createdAt: string; // ISO 8601
}

function newId() {
  return crypto.randomUUID?.() ?? Math.random().toString(36).slice(2);
}
function now() {
  return new Date().toISOString();
}

/** Stable sort: page ASC, column (left→right) ASC, yRatio (top→bottom) ASC, createdAt */
function sortSnippets(list: Snippet[]): Snippet[] {
  const COL = 0.5;
  return [...list].sort((a, b) =>
    a.pageIndex - b.pageIndex ||
    (Math.floor(a.xRatio / COL) - Math.floor(b.xRatio / COL)) ||
    a.yRatio - b.yRatio ||
    a.createdAt.localeCompare(b.createdAt)
  );
}

interface SnippetState {
  snippets: Snippet[];
  /** Insert a snippet in page+y1 sorted order. Returns the new id. */
  addSnippet: (snip: Omit<Snippet, 'id' | 'note' | 'tags' | 'translation' | 'createdAt'>) => string;
  removeSnippet: (id: string) => void;
  updateNote: (id: string, note: string) => void;
  updateTranslation: (id: string, translation: string | null) => void;
  addTag: (id: string, tag: string) => void;
  removeTag: (id: string, tag: string) => void;
  getByPaper: (paperId: string) => Snippet[];
  clearPaper: (paperId: string) => void;
  /** Replace all snippets (or derive from current state) — used on backend load. */
  setAll: (snippets: Snippet[] | ((prev: Snippet[]) => Snippet[])) => void;
}

export const useSnippetStore = create<SnippetState>((set, get) => ({
  snippets: [],

  addSnippet: (input) => {
    const id = newId();
    const snippet: Snippet = { ...input, id, note: '', tags: [], translation: null, createdAt: now() };
    set((s) => ({ snippets: sortSnippets([...s.snippets, snippet]) }));
    return id;
  },

  removeSnippet: (id) => {
    set((s) => ({ snippets: s.snippets.filter((sn) => sn.id !== id) }));
  },

  updateNote: (id, note) => {
    set((s) => ({
      snippets: s.snippets.map((sn) => (sn.id === id ? { ...sn, note } : sn)),
    }));
  },

  updateTranslation: (id, translation) => {
    set((s) => ({
      snippets: s.snippets.map((sn) => (sn.id === id ? { ...sn, translation } : sn)),
    }));
  },

  addTag: (id, tag) => {
    const trimmed = tag.trim();
    if (!trimmed) return;
    set((s) => ({
      snippets: s.snippets.map((sn) =>
        sn.id === id && !sn.tags.includes(trimmed)
          ? { ...sn, tags: [...sn.tags, trimmed] }
          : sn
      ),
    }));
  },

  removeTag: (id, tag) => {
    set((s) => ({
      snippets: s.snippets.map((sn) =>
        sn.id === id ? { ...sn, tags: sn.tags.filter((t) => t !== tag) } : sn
      ),
    }));
  },

  getByPaper: (paperId) => {
    return get().snippets.filter((s) => s.paperId === paperId);
  },

  clearPaper: (paperId) => {
    set((s) => ({
      snippets: s.snippets.filter((sn) => sn.paperId !== paperId),
    }));
  },

  setAll: (snippets) =>
    set((s) => ({
      snippets:
        typeof snippets === 'function'
          ? sortSnippets(snippets(s.snippets))
          : snippets,
    })),
}));
