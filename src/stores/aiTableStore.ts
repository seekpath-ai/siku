import { create } from 'zustand';

/** AI 表格列 dialog state. Entries (paper multi-select, collection tree)
 *  resolve their paper set, then open the dialog mounted in LibraryLayout. */
interface AiTableState {
  open: boolean;
  paperIds: string[];
  openWith: (paperIds: string[]) => void;
  close: () => void;
}

export const useAiTableStore = create<AiTableState>((set) => ({
  open: false,
  paperIds: [],
  openWith: (paperIds) => set({ open: true, paperIds }),
  close: () => set({ open: false, paperIds: [] }),
}));
