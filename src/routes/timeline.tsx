import { useEffect, useMemo, useState } from 'react';
import { createRoute, useNavigate } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import {
  Clock, FileText, StickyNote, Highlighter, FlaskConical, FolderOpen, Bot, Loader2, Languages,
} from 'lucide-react';
import { timelineList } from '@/lib/tauri';
import { useTabStore } from '@/stores/tabStore';
import type { TimelineItem } from '@/lib/types';

const MODULES = [
  { key: '', label: '全部' },
  { key: 'library', label: '文献' },
  { key: 'zhisi', label: '智思' },
  { key: 'notes', label: '笔记' },
  { key: 'research', label: '科研' },
  { key: 'knowledge', label: '知识库' },
] as const;

const PAGE_SIZE = 50;

const TYPE_META: Record<string, { icon: React.ReactNode; color: string; label: string }> = {
  paper_imported: { icon: <FileText size={14} />, color: 'text-sky-400', label: '导入文献' },
  snippet_created: { icon: <Highlighter size={14} />, color: 'text-amber-400', label: '新增摘录' },
  snippet_translated: { icon: <LanguagesIcon />, color: 'text-emerald-400', label: '翻译摘录' },
  note_created: { icon: <StickyNote size={14} />, color: 'text-violet-400', label: '新建笔记' },
  note_updated: { icon: <StickyNote size={14} />, color: 'text-violet-400', label: '编辑笔记' },
  note_agent_edited: { icon: <Bot size={14} />, color: 'text-primary', label: 'AI 整理' },
  research_topic_created: { icon: <FlaskConical size={14} />, color: 'text-rose-400', label: '新建课题' },
  research_source_discovered: { icon: <FlaskConical size={14} />, color: 'text-rose-400', label: '发现文献' },
  knowledge_item_created: { icon: <FolderOpen size={14} />, color: 'text-teal-400', label: '知识条目' },
};

function LanguagesIcon() {
  return <Languages size={14} />;
}

function relativeTime(iso: string): string {
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return '';
  const diff = Date.now() - t;
  const min = Math.floor(diff / 60_000);
  if (min < 1) return '刚刚';
  if (min < 60) return `${min} 分钟前`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr} 小时前`;
  const day = Math.floor(hr / 24);
  if (day < 7) return `${day} 天前`;
  return new Date(iso).toLocaleDateString('zh-CN', { month: 'numeric', day: 'numeric' });
}

function dayKey(iso: string): string {
  const d = new Date(iso);
  return `${d.getFullYear()}-${d.getMonth() + 1}-${d.getDate()}`;
}

function groupLabel(iso: string): string {
  const now = new Date();
  const today = dayKey(now.toISOString());
  const yesterday = dayKey(new Date(now.getTime() - 86_400_000).toISOString());
  const k = dayKey(iso);
  if (k === today) return '今天';
  if (k === yesterday) return '昨天';
  const d = new Date(iso);
  return `${d.getFullYear()} 年 ${d.getMonth() + 1} 月 ${d.getDate()} 日`;
}

function TimelinePage() {
  const navigate = useNavigate();
  const { openRoute, open } = useTabStore();
  const [items, setItems] = useState<TimelineItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [hasMore, setHasMore] = useState(true);
  const [module, setModule] = useState('');

  const load = async (offset: number, replace: boolean) => {
    if (replace) setLoading(true);
    else setLoadingMore(true);
    try {
      const data = await timelineList(PAGE_SIZE, offset, module || undefined);
      setItems((prev) => (replace ? data : [...prev, ...data]));
      setHasMore(data.length === PAGE_SIZE);
    } catch (err) {
      console.error('加载时间轴失败:', err);
    } finally {
      setLoading(false);
      setLoadingMore(false);
    }
  };

  useEffect(() => {
    load(0, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [module]);

  const groups = useMemo(() => {
    const map = new Map<string, TimelineItem[]>();
    for (const item of items) {
      const k = groupLabel(item.timestamp);
      const list = map.get(k) || [];
      list.push(item);
      map.set(k, list);
    }
    return [...map.entries()];
  }, [items]);

  const handleOpen = (item: TimelineItem) => {
    // Paper / snippet → open (or activate) the reader tab for that paper.
    if (item.module === 'library' || item.module === 'zhisi') {
      const paperId = item.params?.paperId;
      if (!paperId) return;
      open({
        id: `reader-${paperId}`,
        title: item.title,
        icon: 'pdf',
        route: '/reader/$paperId',
        params: { paperId },
      });
      navigate({ to: '/reader/$paperId', params: { paperId } });
      return;
    }
    // Note → activate the notes tab and select the note.
    if (item.module === 'notes') {
      const tab = openRoute('/notes', { title: '笔记', icon: 'note' });
      navigate({ to: tab.route, search: item.search || {} });
      return;
    }
    // Research → navigate to the topic detail (same as research page does).
    if (item.module === 'research') {
      const topicId = item.params?.topicId;
      if (topicId) navigate({ to: '/research/$topicId', params: { topicId } });
      return;
    }
    // Knowledge → activate the knowledge tab and open the domain.
    if (item.module === 'knowledge') {
      const domainId = item.params?.domainId;
      if (!domainId) return;
      openRoute('/knowledge', { title: '知识库', icon: 'knowledge' });
      navigate({ to: '/knowledge/$domainId', params: { domainId } });
      return;
    }
    // Fallback: generic navigation.
    if (item.params && Object.keys(item.params).length > 0) {
      navigate({ to: item.route, params: item.params });
    } else if (item.search && Object.keys(item.search).length > 0) {
      navigate({ to: item.route, search: item.search });
    } else {
      navigate({ to: item.route });
    }
  };

  return (
    <div className="max-w-3xl mx-auto px-6 py-8">
      <div className="flex items-center gap-3 mb-2">
        <Clock size={24} className="text-primary" />
        <h1 className="text-xl font-semibold text-text-primary">时间轴</h1>
      </div>
      <p className="text-xs text-text-secondary/60 mb-4">跨模块的活动记录，按时间倒序排列</p>

      {/* Module filter */}
      <div className="flex flex-wrap gap-1.5 mb-6">
        {MODULES.map((m) => (
          <button
            key={m.key || 'all'}
            onClick={() => setModule(m.key)}
            className={`px-2.5 py-1 rounded text-xs transition-colors ${
              module === m.key
                ? 'bg-primary/15 text-primary'
                : 'bg-surface-hover text-text-secondary hover:text-text-primary'
            }`}
          >
            {m.label}
          </button>
        ))}
      </div>

      {loading ? (
        <div className="flex items-center gap-2 text-sm text-text-secondary/60 py-10 justify-center">
          <Loader2 size={14} className="animate-spin" />加载中...
        </div>
      ) : items.length === 0 ? (
        <div className="text-sm text-text-secondary/50 text-center py-12">
          暂无活动记录。导入文献、添加摘录、创建笔记后，它们会出现在这里。
        </div>
      ) : (
        <div className="space-y-6">
          {groups.map(([label, list]) => (
            <div key={label}>
              <div className="flex items-center gap-2 mb-2">
                <span className="text-xs font-medium text-text-secondary/70">{label}</span>
                <div className="flex-1 h-px bg-surface-hover" />
              </div>
              <div className="relative">
                {/* Vertical line */}
                <div className="absolute left-[9px] top-1 bottom-1 w-px bg-surface-hover" />
                <div className="space-y-0.5">
                  {list.map((item) => {
                    const meta = TYPE_META[item.activity_type] || {
                      icon: <Clock size={14} />,
                      color: 'text-text-secondary/60',
                      label: '活动',
                    };
                    return (
                      <button
                        key={item.id}
                        onClick={() => handleOpen(item)}
                        className="w-full flex items-start gap-3 px-2 py-2 rounded-lg text-left hover:bg-surface-hover/70 transition-colors group"
                      >
                        <span
                          className={`mt-0.5 w-[18px] h-[18px] shrink-0 rounded-full bg-surface border border-surface-hover flex items-center justify-center ${meta.color}`}
                        >
                          {meta.icon}
                        </span>
                        <span className="flex-1 min-w-0">
                          <span className="flex items-center gap-1.5 text-[13px] text-text-primary truncate">
                            <span className="truncate">{item.title || '未命名'}</span>
                            <span className="shrink-0 text-[10px] px-1.5 py-px rounded bg-surface-hover text-text-secondary/70">
                              {meta.label}
                            </span>
                          </span>
                          {item.subtitle && (
                            <span className="block text-xs text-text-secondary/60 truncate mt-0.5">
                              {item.subtitle}
                            </span>
                          )}
                        </span>
                        <span className="shrink-0 text-[11px] text-text-secondary/40 mt-0.5">
                          {relativeTime(item.timestamp)}
                        </span>
                      </button>
                    );
                  })}
                </div>
              </div>
            </div>
          ))}

          {hasMore && (
            <div className="flex justify-center pt-2">
              <button
                onClick={() => load(items.length, false)}
                disabled={loadingMore}
                className="px-3 py-1.5 rounded text-xs text-text-secondary hover:bg-surface-hover transition-colors disabled:opacity-50 flex items-center gap-1.5"
              >
                {loadingMore ? <><Loader2 size={12} className="animate-spin" />加载中...</> : '加载更多'}
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/timeline',
  component: TimelinePage,
});
