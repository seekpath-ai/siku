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
  /** Separate dialog: manage the unattended rules. */
  rulesOpen: boolean;
  /** Prefill for the new-rule form when opened from a finished run. */
  rulesPrefill: { dimension: string; target: string; scopeId: string | null } | null;
  /** Open with an explicit paper set (collection node / list selection). */
  openWith: (paperIds: string[], scope?: ClassifyScope) => void;
  close: () => void;
  openRules: (prefill?: { dimension: string; target: string; scopeId: string | null }) => void;
  closeRules: () => void;
}

const DEFAULT_SCOPE: ClassifyScope = {
  kind: 'selection',
  collectionId: null,
  label: '所选文献',
};

/** AI 文献分类 dialog state. Entries (collection tree, paper multi-select)
 *  resolve their paper set, then open the dialog mounted in LibraryLayout. */
export const useAiClassifyStore = create<AiClassifyState>((set) => ({
  open: false,
  paperIds: [],
  scope: DEFAULT_SCOPE,
  rulesOpen: false,
  rulesPrefill: null,
  openWith: (paperIds, scope) =>
    set({ open: true, paperIds, scope: scope ?? { ...DEFAULT_SCOPE, kind: 'selection' } }),
  close: () => set({ open: false, paperIds: [], scope: DEFAULT_SCOPE }),
  openRules: (prefill) => set({ rulesOpen: true, rulesPrefill: prefill ?? null }),
  closeRules: () => set({ rulesOpen: false }),
}));
