import { useMemo } from 'react';
import { create } from 'zustand';

/** Session-scoped note view-lock state. Deliberately NOT persisted: every
 *  unlock is forgotten when the app restarts, so all locked notes re-lock.
 *  There is no cached password — every unlock requires typing it again. */
interface NoteLockState {
  /** Unlocked lock-root ids, grouped by vault. Unlocking a folder root
   *  releases its whole subtree (see computeHiddenSet). */
  unlockedByVault: Record<string, string[]>;
  isUnlocked: (vaultId: string, id: string) => boolean;
  unlock: (vaultId: string, id: string) => void;
  unlockMany: (vaultId: string, ids: string[]) => void;
  /** Remove ids from the unlocked set (blur-relock / folder collapse). */
  relock: (vaultId: string, ids: string[]) => void;
  /** Forget every unlock of a vault (called on vault switch / delete). */
  clearVault: (vaultId: string) => void;
}

export const useNoteLockStore = create<NoteLockState>()((set, get) => ({
  unlockedByVault: {},

  isUnlocked: (vaultId, id) => (get().unlockedByVault[vaultId] ?? []).includes(id),

  unlock: (vaultId, id) => {
    get().unlockMany(vaultId, [id]);
  },

  unlockMany: (vaultId, ids) => {
    set((s) => {
      const list = s.unlockedByVault[vaultId] ?? [];
      const merged = [...list];
      let changed = false;
      for (const id of ids) {
        if (!merged.includes(id)) {
          merged.push(id);
          changed = true;
        }
      }
      if (!changed) return s;
      return { unlockedByVault: { ...s.unlockedByVault, [vaultId]: merged } };
    });
  },

  relock: (vaultId, ids) => {
    set((s) => {
      const list = s.unlockedByVault[vaultId];
      if (!list) return s;
      const drop = new Set(ids);
      const next = list.filter((id) => !drop.has(id));
      if (next.length === list.length) return s;
      const unlockedByVault = { ...s.unlockedByVault };
      if (next.length === 0) delete unlockedByVault[vaultId];
      else unlockedByVault[vaultId] = next;
      return { unlockedByVault };
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
}));

/** Union of every vault's unlocked lock-root ids (note ids are globally
 *  unique), for the `unlocked_ids` parameter of search/backlinks/list
 *  commands — the backend releases each root's whole subtree. */
export function getAllUnlockedIds(): string[] {
  return Object.values(useNoteLockStore.getState().unlockedByVault).flat();
}

/** Reactive version of getAllUnlockedIds as a Set (re-renders on unlock). */
export function useAllUnlockedSet(): Set<string> {
  const map = useNoteLockStore((s) => s.unlockedByVault);
  return useMemo(() => new Set(Object.values(map).flat()), [map]);
}
