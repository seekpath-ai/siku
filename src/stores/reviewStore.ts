import { create } from 'zustand';

/** AI 综述 dialog state. Entries (paper multi-select, collection tree)
 *  resolve their paper set, then open the dialog mounted in AppShell. */
interface ReviewState {
  open: boolean;
  paperIds: string[];
  runId: string | null;
  /** 「新建综述」丢弃的 runId。后端 run 仍在（可能仍在跑），保留它让用户
   *  能从表单页返回，而不是永久丢失该任务的入口。 */
  lastRunId: string | null;
  openWith: (paperIds: string[]) => void;
  /** Re-open after a close: paperIds and runId survive close, so the dialog
   *  restores the running/finished task snapshot instead of a blank form. */
  reopen: () => void;
  setRunId: (runId: string) => void;
  resetRun: () => void;
  /** 把 lastRunId 换回 runId，返回被「新建综述」挤掉的任务。 */
  restoreRun: () => void;
  close: () => void;
}

export const useReviewStore = create<ReviewState>((set) => ({
  open: false,
  paperIds: [],
  runId: null,
  lastRunId: null,
  openWith: (paperIds) => set({ open: true, paperIds }),
  reopen: () => set({ open: true }),
  setRunId: (runId) => set({ runId }),
  resetRun: () => set((s) => ({ runId: null, lastRunId: s.runId ?? s.lastRunId })),
  restoreRun: () => set((s) => (s.lastRunId ? { runId: s.lastRunId, lastRunId: null } : s)),
  close: () => set({ open: false }),
}));
