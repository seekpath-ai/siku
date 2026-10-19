import { create } from 'zustand';

/** AI 表格列 dialog state. Entries (paper multi-select, collection tree)
 *  resolve their paper set, then open the dialog mounted in AppShell. */
interface AiTableState {
  open: boolean;
  paperIds: string[];
  openWith: (paperIds: string[]) => void;
  /** Re-open after a close: paperIds are kept on close, so the grid comes back
   *  as-is and any in-flight backend run re-attaches (ai_table_extract_status). */
  reopen: () => void;
  close: () => void;
}

export const useAiTableStore = create<AiTableState>((set) => ({
  open: false,
  paperIds: [],
  openWith: (paperIds) => set({ open: true, paperIds }),
  reopen: () => set((s) => (s.paperIds.length > 0 ? { open: true } : {})),
  // Closing must not stop or forget the task: keep the paper set so the
  // dialog can be reopened from the global AI-task chips.
  close: () => set({ open: false }),
}));
