import { create } from 'zustand';
import type { KnowledgeDomain, KnowledgeItem } from '@/lib/types';

interface KnowledgeState {
  domains: KnowledgeDomain[];
  activeDomainId: string | null;
  items: KnowledgeItem[];
  isLoading: boolean;
  searchQuery: string;

  setDomains: (d: KnowledgeDomain[]) => void;
  setActiveDomain: (id: string | null) => void;
  setItems: (items: KnowledgeItem[]) => void;
  setLoading: (v: boolean) => void;
  setSearchQuery: (q: string) => void;
}

export const useKnowledgeStore = create<KnowledgeState>((set) => ({
  domains: [],
  activeDomainId: null,
  items: [],
  isLoading: false,
  searchQuery: '',

  setDomains: (domains) => set({ domains }),
  setActiveDomain: (id) => set({ activeDomainId: id, items: [] }),
  setItems: (items) => set({ items }),
  setLoading: (v) => set({ isLoading: v }),
  setSearchQuery: (q) => set({ searchQuery: q }),
}));
