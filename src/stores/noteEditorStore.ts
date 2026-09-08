import { create } from 'zustand';

export type NoteViewMode = 'edit' | 'source' | 'reading' | 'split-h' | 'split-v' | 'backlinks';

export interface NoteEditorState {
  mode: NoteViewMode;
  /** Editor scroll position (scrollTop of the CodeMirror scroll DOM). */
  scroll: number;
  /** Cursor offset in the document. */
  cursor: number;
}

interface NoteEditorStoreState {
  /** Per-note editor state keyed by note id. */
  states: Record<string, NoteEditorState>;
  setState: (noteId: string, patch: Partial<NoteEditorState>) => void;
  getState: (noteId: string) => NoteEditorState;
  /** Forget a note's state (called when the note is deleted). */
  remove: (noteId: string) => void;
}

const DEFAULT_STATE: NoteEditorState = { mode: 'edit', scroll: 0, cursor: 0 };

export const useNoteEditorStore = create<NoteEditorStoreState>((set, get) => ({
  states: {},

  setState: (noteId, patch) => {
    set((s) => ({
      states: {
        ...s.states,
        [noteId]: { ...(s.states[noteId] ?? DEFAULT_STATE), ...patch },
      },
    }));
  },

  getState: (noteId) => {
    return get().states[noteId] ?? DEFAULT_STATE;
  },

  remove: (noteId) => {
    set((s) => {
      if (!(noteId in s.states)) return s;
      const states = { ...s.states };
      delete states[noteId];
      return { states };
    });
  },
}));
