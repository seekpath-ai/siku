import { useEffect } from 'react';
import { useResearchStore } from '@/stores/researchStore';
import { useDialogStore } from '@/stores/dialogStore';
import {
  researchListTopics, researchCreateTopic, researchDiscoverSources,
  researchListSources, researchDeleteTopic, researchUpdateTopic,
} from '@/lib/tauri';

const reportError = (action: string, err: unknown) => {
  console.error(`${action}:`, err);
  useDialogStore.getState().alert(
    `${action}失败：${err instanceof Error ? err.message : String(err)}`,
    '科研追踪'
  );
};

export function useResearch() {
  const store = useResearchStore();

  useEffect(() => { loadTopics(); }, []);

  const loadTopics = async () => {
    try { store.setTopics(await researchListTopics()); }
    catch (err) { console.error('load topics:', err); }
  };

  const PAGE_SIZE = 50;

  const loadSources = async (topicId: string) => {
    try {
      const rows = await researchListSources(topicId, undefined, PAGE_SIZE, 0);
      store.setSources(rows);
      store.setHasMore(rows.length === PAGE_SIZE);
    } catch (err) { console.error('load sources:', err); }
  };

  const loadMoreSources = async (topicId: string) => {
    if (store.loadingMoreSources) return;
    store.setLoadingMoreSources(true);
    try {
      const rows = await researchListSources(topicId, undefined, PAGE_SIZE, store.sources.length);
      store.setSources([...store.sources, ...rows]);
      store.setHasMore(rows.length === PAGE_SIZE);
    } catch (err) {
      reportError('加载更多文献', err);
    } finally {
      store.setLoadingMoreSources(false);
    }
  };

  const createTopic = async (name: string, keywords: string[], description?: string) => {
    try {
      await researchCreateTopic(name, description, keywords);
      await loadTopics();
    } catch (err) { reportError('创建课题', err); }
  };

  const discoverSources = async (topicId: string) => {
    store.setDiscovering(true);
    try {
      const sources = await researchDiscoverSources(topicId);
      store.setSources(sources);
      await loadTopics(); // refresh topic status
    } catch (err) {
      reportError('发现文献', err);
    } finally {
      store.setDiscovering(false);
    }
  };

  const updateTopic = async (topicId: string, status?: string) => {
    try { await researchUpdateTopic(topicId, status); await loadTopics(); }
    catch (err) { reportError('更新课题', err); }
  };

  const deleteTopic = async (topicId: string) => {
    try { await researchDeleteTopic(topicId); store.removeTopic(topicId); }
    catch (err) { reportError('删除课题', err); }
  };

  return { ...store, loadSources, loadMoreSources, createTopic, discoverSources, updateTopic, deleteTopic };
}
