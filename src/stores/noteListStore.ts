import { create } from 'zustand';

/** Persists NoteList UI state (expanded folders, scroll position) per vault so
 *  the tree position survives tab switches / page remounts (the app shell has
 *  no keep-alive — routes unmount on navigation). */
interface NoteListState {
  expandedByVault: Record<string, string[]>;
  scrollByVault: Record<string, number>;
  setExpanded: (vaultId: string, ids: string[]) => void;
  setScroll: (vaultId: string, top: number) => void;
}

export const useNoteListStore = create<NoteListState>((set) => ({
  expandedByVault: {},
  scrollByVault: {},
  setExpanded: (vaultId, ids) =>
    set((s) => ({ expandedByVault: { ...s.expandedByVault, [vaultId]: ids } })),
  setScroll: (vaultId, top) =>
    set((s) => ({ scrollByVault: { ...s.scrollByVault, [vaultId]: top } })),
}));
