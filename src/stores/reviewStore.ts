import { create } from 'zustand';

/** AI 综述 dialog state. Entries (paper multi-select, collection tree)
 *  resolve their paper set, then open the dialog mounted in LibraryLayout. */
interface ReviewState {
  open: boolean;
  paperIds: string[];
  openWith: (paperIds: string[]) => void;
  close: () => void;
}

export const useReviewStore = create<ReviewState>((set) => ({
  open: false,
  paperIds: [],
  openWith: (paperIds) => set({ open: true, paperIds }),
  close: () => set({ open: false, paperIds: [] }),
}));
