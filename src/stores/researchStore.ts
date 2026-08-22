import { create } from 'zustand';
import type { ResearchTopic, ResearchSource } from '@/lib/types';

interface ResearchState {
  topics: ResearchTopic[];
  activeTopicId: string | null;
  sources: ResearchSource[];
  isDiscovering: boolean;
  /** Whether more pages of sources remain to be loaded. */
  hasMore: boolean;
  loadingMoreSources: boolean;

  setTopics: (t: ResearchTopic[]) => void;
  setActiveTopic: (id: string | null) => void;
  setSources: (s: ResearchSource[]) => void;
  /** Append a newly discovered source (streaming display). */
  appendSource: (s: ResearchSource) => void;
  setDiscovering: (v: boolean) => void;
  setHasMore: (v: boolean) => void;
  setLoadingMoreSources: (v: boolean) => void;
  removeTopic: (id: string) => void;
}

export const useResearchStore = create<ResearchState>((set) => ({
  topics: [],
  activeTopicId: null,
  sources: [],
  isDiscovering: false,
  hasMore: false,
  loadingMoreSources: false,

  setTopics: (topics) => set({ topics }),
  setActiveTopic: (id) => set({ activeTopicId: id, sources: [], hasMore: false, loadingMoreSources: false }),
  setSources: (sources) => set({ sources }),
  // Prepend so streaming items appear at the top, matching the final
  // `discovered_at DESC` sort — no reorder jump when discovery finishes.
  appendSource: (source) =>
    set((s) => ({
      sources: s.sources.some((x) => x.id === source.id) ? s.sources : [source, ...s.sources],
    })),
  setDiscovering: (v) => set({ isDiscovering: v }),
  setHasMore: (hasMore) => set({ hasMore }),
  setLoadingMoreSources: (loadingMoreSources) => set({ loadingMoreSources }),
  removeTopic: (id) => set((s) => ({
    topics: s.topics.filter((t) => t.id !== id),
    activeTopicId: s.activeTopicId === id ? null : s.activeTopicId,
  })),
}));
