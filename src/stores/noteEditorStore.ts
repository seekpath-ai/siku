import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { useNotesSettingsStore } from './notesSettingsStore';

export type NoteViewMode = 'edit' | 'source' | 'reading' | 'split-h' | 'split-v' | 'backlinks';

export interface NoteEditorState {
  mode: NoteViewMode;
  /** Editor scroll position (scrollTop of the CodeMirror scroll DOM). */
  scroll: number;
  /** Cursor offset in the document. */
  cursor: number;
  /** Outline panel open (per-note, like the view mode). */
  outline: boolean;
}

interface NoteEditorStoreState {
  /** Per-note editor state keyed by note id. */
  states: Record<string, NoteEditorState>;
  setState: (noteId: string, patch: Partial<NoteEditorState>) => void;
  getState: (noteId: string) => NoteEditorState;
  /** Forget a note's state (called when the note is deleted). */
  remove: (noteId: string) => void;
}

const DEFAULT_STATE: NoteEditorState = { mode: 'edit', scroll: 0, cursor: 0, outline: false };

/** Cap on persisted per-note entries; oldest-touched are dropped (the map is
 *  re-inserted on every setState, so key order is LRU order). */
const MAX_PERSISTED_NOTES = 200;

/** Fallback for notes with no remembered state: the global default view mode
 *  from the notes settings (device-local). */
function defaultState(): NoteEditorState {
  return { ...DEFAULT_STATE, mode: useNotesSettingsStore.getState().defaultMode };
}

export const useNoteEditorStore = create<NoteEditorStoreState>()(
  persist(
    (set, get) => ({
      states: {},

      setState: (noteId, patch) => {
        set((s) => {
          const states = { ...s.states };
          // Re-insert at the end so key order tracks recency (LRU trim below).
          const prev = states[noteId] ?? defaultState();
          delete states[noteId];
          states[noteId] = { ...prev, ...patch };
          const keys = Object.keys(states);
          if (keys.length > MAX_PERSISTED_NOTES) {
            for (const k of keys.slice(0, keys.length - MAX_PERSISTED_NOTES)) {
              delete states[k];
            }
          }
          return { states };
        });
      },

      getState: (noteId) => {
        return get().states[noteId] ?? defaultState();
      },

      remove: (noteId) => {
        set((s) => {
          if (!(noteId in s.states)) return s;
          const states = { ...s.states };
          delete states[noteId];
          return { states };
        });
      },
    }),
    {
      name: 'siku.note-editor',
      partialize: (s) => ({ states: s.states }),
    }
  )
);
