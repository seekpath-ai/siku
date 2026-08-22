import { useEffect, useState } from 'react';
import { createRoute, useNavigate, Link } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { Bookmark, X, ArrowLeft } from 'lucide-react';
import { bookmarksList, bookmarksDelete } from '@/lib/tauri';
import { useTabStore } from '@/stores/tabStore';
import type { Bookmark as BookmarkType } from '@/lib/types';

function BookmarksPage() {
  const [items, setItems] = useState<BookmarkType[]>([]);
  const [loading, setLoading] = useState(true);
  const navigate = useNavigate();
  const { openRoute } = useTabStore();

  const load = async () => {
    setLoading(true);
    try {
      const data = await bookmarksList();
      setItems(data);
    } catch (err) {
      console.error('加载书签失败:', err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  const handleOpen = (item: BookmarkType) => {
    const tab = openRoute(item.route, { title: item.title, icon: 'bookmark' });
    try {
      const params = JSON.parse(item.params_json || '{}');
      if (tab.params) {
        navigate({ to: tab.route, params: { ...tab.params, ...params } });
      } else if (Object.keys(params).length > 0) {
        navigate({ to: tab.route, search: params });
      } else {
        navigate({ to: tab.route });
      }
    } catch {
      navigate({ to: tab.route });
    }
  };

  const handleDelete = async (e: React.MouseEvent, id: string) => {
    e.stopPropagation();
    try {
      await bookmarksDelete(id);
      setItems((prev) => prev.filter((i) => i.id !== id));
    } catch (err) {
      console.error('删除书签失败:', err);
    }
  };

  return (
    <div className="max-w-3xl mx-auto px-6 py-8">
      <Link to="/library" className="flex items-center gap-2 text-sm text-text-secondary hover:text-text-primary mb-6">
        <ArrowLeft size={16} />返回图书馆
      </Link>

      <div className="flex items-center gap-3 mb-6">
        <Bookmark size={24} className="text-primary" />
        <h1 className="text-xl font-semibold text-text-primary">书签</h1>
      </div>

      {loading ? (
        <div className="text-sm text-text-secondary/60">加载中...</div>
      ) : items.length === 0 ? (
        <div className="text-sm text-text-secondary/60">暂无书签，可在任意页面按 Ctrl+D 收藏当前位置。</div>
      ) : (
        <div className="space-y-1">
          {items.map((item) => (
            <button
              key={item.id}
              onClick={() => handleOpen(item)}
              className="w-full flex items-center gap-3 px-4 py-3 rounded-lg bg-surface border border-surface-hover text-left hover:border-primary/30 transition-colors group"
            >
              <Bookmark size={16} className="text-primary shrink-0" />
              <div className="flex-1 min-w-0">
                <div className="text-sm text-text-primary truncate">{item.title}</div>
                <div className="text-xs text-text-secondary/60 truncate">{item.route}</div>
              </div>
              <button
                type="button"
                onClick={(e) => handleDelete(e, item.id)}
                className="p-1.5 rounded text-text-secondary/40 hover:text-red-400 hover:bg-surface-hover opacity-0 group-hover:opacity-100 transition-opacity"
                title="删除书签"
              >
                <X size={14} />
              </button>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/bookmarks',
  component: BookmarksPage,
});
