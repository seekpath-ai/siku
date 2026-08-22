import { create } from 'zustand';

/** A request to highlight a piece of quoted evidence in the reader. */
export interface EvidenceRequest {
  paperId: string;
  /** 1-based page hint from the citation; the locator also probes neighbors. */
  page?: number;
  /** Verbatim quote from the paper text (LLM-provided evidence snippet). */
  exact: string;
  /** Bumped on every request so identical quotes retrigger the highlight. */
  nonce: number;
}

interface EvidenceState {
  request: EvidenceRequest | null;
  requestHighlight: (r: Omit<EvidenceRequest, 'nonce'>) => void;
}

export const useEvidenceStore = create<EvidenceState>((set) => ({
  request: null,
  requestHighlight: (r) => set({ request: { ...r, nonce: Date.now() } }),
}));
