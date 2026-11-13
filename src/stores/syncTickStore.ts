import { create } from 'zustand';

/** Monotonic counter bumped whenever remote changes are applied. Artifact
 *  chips/images subscribe to it so a blob that arrives after the first
 *  resolution attempt is retried without remounting the whole message list. */
export const useSyncTick = create<{ tick: number; bump: () => void }>((set) => ({
  tick: 0,
  bump: () => set((s) => ({ tick: s.tick + 1 })),
}));
