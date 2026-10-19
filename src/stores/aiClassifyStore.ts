import { create } from 'zustand';

/** Scope the classification run was opened on — the collection whose children
 *  should receive the new categories, if any. */
export interface ClassifyScope {
  /** collection | selection | all */
  kind: string;
  collectionId: string | null;
  label: string;
}

interface AiClassifyState {
  open: boolean;
  paperIds: string[];
  scope: ClassifyScope;
  /** Backend run the dialog is attached to. Lives here (not in the component)
   *  so closing the dialog or switching routes — which unmounts LibraryLayout
   *  and everything under it — never orphans the run. Cleared only when the
   *  user explicitly starts a new batch, or when the backend no longer knows
   *  the run. */
  runId: string | null;
  /** Separate dialog: manage the unattended rules. */
  rulesOpen: boolean;
  /** Prefill for the new-rule form when opened from a finished run. */
  rulesPrefill: { dimension: string; target: string; scopeId: string | null } | null;
  /** Open with an explicit paper set (collection node / list selection). */
  openWith: (paperIds: string[], scope?: ClassifyScope) => void;
  close: () => void;
  setRunId: (runId: string | null) => void;
  openRules: (prefill?: { dimension: string; target: string; scopeId: string | null }) => void;
  closeRules: () => void;
}

const DEFAULT_SCOPE: ClassifyScope = {
  kind: 'selection',
  collectionId: null,
  label: '所选文献',
};

/** AI 文献分类 dialog state. Entries (collection tree, paper multi-select)
 *  resolve their paper set, then open the dialog mounted in AppShell. */
export const useAiClassifyStore = create<AiClassifyState>((set) => ({
  open: false,
  paperIds: [],
  scope: DEFAULT_SCOPE,
  runId: null,
  rulesOpen: false,
  rulesPrefill: null,
  openWith: (paperIds, scope) =>
    set({ open: true, paperIds, scope: scope ?? { ...DEFAULT_SCOPE, kind: 'selection' } }),
  // Deliberately keeps runId: closing the window must not detach the run.
  close: () => set({ open: false, paperIds: [], scope: DEFAULT_SCOPE }),
  setRunId: (runId) => set({ runId }),
  openRules: (prefill) => set({ rulesOpen: true, rulesPrefill: prefill ?? null }),
  closeRules: () => set({ rulesOpen: false }),
}));
