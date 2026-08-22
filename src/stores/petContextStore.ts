import { create } from 'zustand';

/** Snapshot of the current page context for the global pet. */
export interface PetContext {
  /** Page type, e.g. 'notes' | 'library' | 'research'. */
  page: string;
  /** Id of the focused object (note / paper / topic). */
  objectId: string;
  /** Human-readable title of the focused object. */
  title: string;
  /** Current page number in the reader (page === 'reader'). */
  pageNum?: number;
  /** Currently selected text in the reader. */
  selectedText?: string;
}

interface PetContextState {
  context: PetContext | null;
  setContext: (c: PetContext | null) => void;
}

export const usePetContextStore = create<PetContextState>((set) => ({
  context: null,
  setContext: (context) => set({ context }),
}));
