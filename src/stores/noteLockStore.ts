import { useMemo } from 'react';
import { create } from 'zustand';

/** Session-scoped note view-lock state. Deliberately NOT persisted: every
 *  unlock is forgotten when the app restarts, so all locked notes re-lock. */
interface NoteLockState {
  /** Unlocked note/folder ids, grouped by vault. */
  unlockedByVault: Record<string, string[]>;
  /** Cached vault lock password after the first successful verification,
   *  used for one-click / silent unlocks later in the session. Memory only. */
  sessionPassword: string | null;
  isUnlocked: (vaultId: string, id: string) => boolean;
  unlock: (vaultId: string, id: string) => void;
  /** Forget every unlock of a vault (called on vault switch / delete). */
  clearVault: (vaultId: string) => void;
  setSessionPassword: (password: string | null) => void;
}

export const useNoteLockStore = create<NoteLockState>()((set, get) => ({
  unlockedByVault: {},
  sessionPassword: null,

  isUnlocked: (vaultId, id) => (get().unlockedByVault[vaultId] ?? []).includes(id),

  unlock: (vaultId, id) => {
    set((s) => {
      const list = s.unlockedByVault[vaultId] ?? [];
      if (list.includes(id)) return s;
      return { unlockedByVault: { ...s.unlockedByVault, [vaultId]: [...list, id] } };
    });
  },

  clearVault: (vaultId) => {
    set((s) => {
      if (!(vaultId in s.unlockedByVault)) return s;
      const unlockedByVault = { ...s.unlockedByVault };
      delete unlockedByVault[vaultId];
      return { unlockedByVault };
    });
  },

  setSessionPassword: (password) => set({ sessionPassword: password }),
}));

/** Union of every vault's unlocked ids (note ids are globally unique), for
 *  the `unlocked_ids` parameter of search/backlinks/list commands. */
export function getAllUnlockedIds(): string[] {
  return Object.values(useNoteLockStore.getState().unlockedByVault).flat();
}

/** Reactive version of getAllUnlockedIds as a Set (re-renders on unlock). */
export function useAllUnlockedSet(): Set<string> {
  const map = useNoteLockStore((s) => s.unlockedByVault);
  return useMemo(() => new Set(Object.values(map).flat()), [map]);
}
