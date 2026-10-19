import { create } from 'zustand';

/** AI 表格列 dialog state. Entries (paper multi-select, collection tree)
 *  resolve their paper set, then open the dialog mounted in AppShell. */
interface AiTableState {
  open: boolean;
  paperIds: string[];
  /** 列定义/提取设置侧栏收起为图标栏（会话级记忆）。 */
  sidebarCollapsed: boolean;
  /** 用户手动展开/收起过 → 开始抽取时不再自动收起。 */
  sidebarTouched: boolean;
  openWith: (paperIds: string[]) => void;
  /** Re-open after a close: paperIds are kept on close, so the grid comes back
   *  as-is and any in-flight backend run re-attaches (ai_table_extract_status). */
  reopen: () => void;
  close: () => void;
  /** Manual toggle: remembers the user's choice for this session. */
  toggleSidebar: () => void;
  /** 开始抽取时自动收起（表格主体才是重点）；用户手动碰过侧栏则不动。 */
  autoCollapseSidebar: () => void;
}

export const useAiTableStore = create<AiTableState>((set) => ({
  open: false,
  paperIds: [],
  sidebarCollapsed: false,
  sidebarTouched: false,
  openWith: (paperIds) => set({ open: true, paperIds }),
  reopen: () => set((s) => (s.paperIds.length > 0 ? { open: true } : {})),
  // Closing must not stop or forget the task: keep the paper set so the
  // dialog can be reopened from the global AI-task chips.
  close: () => set({ open: false }),
  toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed, sidebarTouched: true })),
  autoCollapseSidebar: () => set((s) => (s.sidebarTouched ? {} : { sidebarCollapsed: true })),
}));
