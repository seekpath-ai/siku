import { create } from 'zustand';

/** AI 综述 dialog state. Entries (paper multi-select, collection tree)
 *  resolve their paper set, then open the dialog mounted in AppShell. */
interface ReviewState {
  open: boolean;
  paperIds: string[];
  runId: string | null;
  openWith: (paperIds: string[]) => void;
  /** Re-open after a close: paperIds and runId survive close, so the dialog
   *  restores the running/finished task snapshot instead of a blank form. */
  reopen: () => void;
  setRunId: (runId: string) => void;
  resetRun: () => void;
  close: () => void;
}

export const useReviewStore = create<ReviewState>((set) => ({
  open: false,
  paperIds: [],
  runId: null,
  openWith: (paperIds) => set({ open: true, paperIds }),
  reopen: () => set({ open: true }),
  setRunId: (runId) => set({ runId }),
  resetRun: () => set({ runId: null }),
  close: () => set({ open: false }),
}));
