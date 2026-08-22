import { create } from 'zustand';

export interface ReaderState {
  page: number;
  zoom: number;
}

interface ReaderStoreState {
  /** Per-paper reader state keyed by paper id. */
  states: Record<string, ReaderState>;
  setState: (paperId: string, patch: Partial<ReaderState>) => void;
  getState: (paperId: string) => ReaderState;
}

const DEFAULT_STATE: ReaderState = { page: 1, zoom: 1 };

export const useReaderStore = create<ReaderStoreState>((set, get) => ({
  states: {},

  setState: (paperId, patch) => {
    set((s) => ({
      states: {
        ...s.states,
        [paperId]: { ...(s.states[paperId] ?? DEFAULT_STATE), ...patch },
      },
    }));
  },

  getState: (paperId) => {
    return get().states[paperId] ?? DEFAULT_STATE;
  },
}));
