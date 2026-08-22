import { useEffect, useState } from 'react';
import { createRoute, Link } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { listen } from '@tauri-apps/api/event';
import { useResearch } from '@/hooks/useResearch';
import { useResearchStore } from '@/stores/researchStore';
import { usePetContextStore } from '@/stores/petContextStore';
import { useTabStore } from '@/stores/tabStore';
import { useDialog } from '@/hooks/useDialog';
import { AutoDiscoveryPanel } from '@/components/research/AutoDiscoveryPanel';
import { SourceTable } from '@/components/research/SourceTable';
import { researchImportSource, researchUpdateSourceStatus } from '@/lib/tauri';
import type { ResearchSource } from '@/lib/types';
import { ArrowLeft, Loader2 } from 'lucide-react';

function ResearchTopicPage() {
  const { topicId } = Route.useParams();
  const navigate = Route.useNavigate();
  const { confirm, alert } = useDialog();
  const { open: openTab } = useTabStore();
  const {
    topics, sources, isDiscovering, hasMore, loadingMoreSources,
    setActiveTopic, loadSources, loadMoreSources, discoverSources,
  } = useResearch();
  const [importingId, setImportingId] = useState<string | null>(null);
  const [discoverHint, setDiscoverHint] = useState<string | null>(null);

  useEffect(() => {
    setActiveTopic(topicId);
    loadSources(topicId);
  }, [topicId]);

  // Stream discovery events: append sources one by one and show phase hints.
  useEffect(() => {
    if (!isDiscovering) {
      setDiscoverHint(null);
      return;
    }
    let cancelled = false;
    let unlisten: (() => void)[] = [];
    const setup = async () => {
      const u1 = await listen<{ topic_id: string; phase: string; found: number }>(
        'research:discover_progress',
        (e) => {
          if (cancelled || e.payload.topic_id !== topicId) return;
          const p = e.payload.phase;
          setDiscoverHint(
            p === 'arxiv'
              ? '正在搜索 arXiv…'
              : p === 'crossref'
                ? '正在搜索 Crossref…'
                : `发现完成，新增 ${e.payload.found} 条`
          );
        }
      );
      const u2 = await listen<{ topic_id: string; source: ResearchSource }>(
        'research:discovered',
        (e) => {
          if (cancelled || e.payload.topic_id !== topicId) return;
          useResearchStore.getState().appendSource(e.payload.source);
        }
      );
      if (cancelled) {
        u1();
        u2();
        return;
      }
      unlisten = [u1, u2];
    };
    setup();
    return () => {
      cancelled = true;
      unlisten.forEach((u) => u());
    };
  }, [isDiscovering, topicId]);

  const topic = topics.find((t) => t.id === topicId);

  // Import a discovered source into the library, then offer to open it.
  const handleImport = async (source: ResearchSource) => {
    if (importingId) return;
    setImportingId(source.id);
    try {
      const res = await researchImportSource(source.id);
      await loadSources(topicId);
      const go = await confirm(`已导入「${res.title}」到图书馆，是否打开阅读器？`, '导入成功');
      if (go) {
        openTab({
          id: `reader-${res.paper_id}`,
          title: res.title,
          icon: 'pdf',
          route: '/reader/$paperId',
          params: { paperId: res.paper_id },
        });
        navigate({ to: '/reader/$paperId', params: { paperId: res.paper_id } });
      }
    } catch (err) {
      await alert(`导入失败：${err}`, '导入失败');
    } finally {
      setImportingId(null);
    }
  };

  const handleMarkRead = async (source: ResearchSource) => {
    try {
      await researchUpdateSourceStatus(source.id, 'read');
      await loadSources(topicId);
    } catch (err) {
      await alert(`操作失败：${err}`, '标记已读');
    }
  };

  // Expose the focused topic to the global pet.
  useEffect(() => {
    if (topic) {
      usePetContextStore.getState().setContext({
        page: 'research',
        objectId: topic.id,
        title: topic.name || '未命名课题',
      });
    } else {
      usePetContextStore.getState().setContext(null);
    }
    return () => usePetContextStore.getState().setContext(null);
  }, [topic]);

  if (!topic) {
    return (
      <div className="p-8 text-text-secondary">
        <Link to="/research" className="flex items-center gap-2 text-sm hover:text-text-primary mb-4">
          <ArrowLeft size={16} />返回
        </Link>
        课题未找到
      </div>
    );
  }

  return (
    <div className="max-w-3xl mx-auto px-6 py-8 space-y-6">
      <Link to="/research" className="flex items-center gap-2 text-sm text-text-secondary hover:text-text-primary">
        <ArrowLeft size={16} />返回科研追踪
      </Link>

      <div>
        <h1 className="text-xl font-semibold text-text-primary">{topic.name}</h1>
        {topic.description && <p className="text-sm text-text-secondary mt-1">{topic.description}</p>}
      </div>

      <AutoDiscoveryPanel
        isDiscovering={isDiscovering}
        onDiscover={() => discoverSources(topicId)}
        hint={discoverHint}
      />

      <SourceTable sources={sources} importingId={importingId} onImport={handleImport} onMarkRead={handleMarkRead} />

      {hasMore && (
        <div className="flex justify-center pt-1">
          <button
            onClick={() => loadMoreSources(topicId)}
            disabled={loadingMoreSources}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded text-xs text-text-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
          >
            {loadingMoreSources && <Loader2 size={12} className="animate-spin" />}
            {loadingMoreSources ? '加载中…' : '加载更多'}
          </button>
        </div>
      )}
    </div>
  );
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/research/$topicId',
  component: ResearchTopicPage,
});
