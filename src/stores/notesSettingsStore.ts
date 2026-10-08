import { create } from 'zustand';
import { persist } from 'zustand/middleware';

/** Global default view mode for opening notes (edit = live preview). Newly
 *  created notes always open in edit mode regardless (see notesCreate). */
export type NotesDefaultMode = 'edit' | 'source' | 'reading';

interface NotesSettingsState {
  defaultMode: NotesDefaultMode;
  /** Default line-wrapping for rendered code blocks (reading view AND chat
   *  bubbles — CodeBlock is shared). Per-block toggle overrides per session. */
  codeBlockWrap: boolean;
  /** Obsidian-style: a single Enter is a line break. Off = CommonMark (soft
   *  breaks collapse to a space). */
  strictLineBreaks: boolean;
  /** Font size (px) of the note editor and reading view body text. */
  editorFontSize: number;
  set: (patch: Partial<Omit<NotesSettingsState, 'set'>>) => void;
}

const STORAGE_KEY = 'siku.notes-settings';
export const FONT_SIZE_MIN = 12;
export const FONT_SIZE_MAX = 24;

// Display preferences are device-local by design (like Obsidian): persisted
// to localStorage, never synced.
export const useNotesSettingsStore = create<NotesSettingsState>()(
  persist(
    (set) => ({
      defaultMode: 'edit',
      codeBlockWrap: false,
      strictLineBreaks: true,
      editorFontSize: 16,
      set: (patch) =>
        set((s) => ({
          ...s,
          ...patch,
          editorFontSize: Math.min(
            FONT_SIZE_MAX,
            Math.max(FONT_SIZE_MIN, patch.editorFontSize ?? s.editorFontSize)
          ),
        })),
    }),
    { name: STORAGE_KEY }
  )
);
