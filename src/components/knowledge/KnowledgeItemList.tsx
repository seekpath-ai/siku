import { useState } from 'react';
import { Plus, Search, Trash2, ExternalLink, Loader2, ChevronLeft, ChevronRight, Tag } from 'lucide-react';
import type { KnowledgeItem } from '@/lib/types';
import { parseKnowledgeTags } from '@/lib/types';
import { ConfirmButton } from '@/components/ui/ConfirmButton';
import { MarkdownEditor } from '@/components/editor/MarkdownEditor';

interface Props {
  items: KnowledgeItem[];
  isLoading: boolean;
  domainName: string;
  hasMore?: boolean;
  page?: number;
  onCreateItem: (title: string, content: string) => Promise<boolean>;
  onUpdateItem?: (id: string, title: string, content: string) => Promise<void>;
  onDeleteItem: (id: string) => void;
  onSearch: (q: string) => void;
  onPageChange?: (page: number) => void;
  onViewItem?: (id: string) => void;
}

export function KnowledgeItemList({
  items, isLoading, domainName, hasMore, page,
  onCreateItem, onUpdateItem, onDeleteItem, onSearch, onPageChange, onViewItem,
}: Props) {
  const [showNew, setShowNew] = useState(false);
  const [newTitle, setNewTitle] = useState('');
  const [newContent, setNewContent] = useState('');
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editTitle, setEditTitle] = useState('');
  const [editContent, setEditContent] = useState('');

  const handleCreate = async () => {
    if (!newTitle.trim()) return;
    const ok = await onCreateItem(newTitle, newContent);
    if (ok) { setNewTitle(''); setNewContent(''); setShowNew(false); }
  };

  const handleStartEdit = (item: KnowledgeItem) => {
    setEditingId(item.id);
    setEditTitle(item.title);
    setEditContent(item.content || '');
  };

  const handleSaveEdit = async () => {
    if (!editingId || !onUpdateItem) return;
    await onUpdateItem(editingId, editTitle, editContent);
    setEditingId(null);
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-lg font-semibold text-text-primary">{domainName}</h2>
        <button onClick={() => setShowNew(!showNew)}
          className="flex items-center gap-1 px-3 py-1.5 rounded-lg bg-primary/10 text-primary text-sm hover:bg-primary/20">
          <Plus size={14} />新建
        </button>
      </div>

      <div className="relative">
        <Search size={16} className="absolute left-3 top-1/2 -translate-y-1/2 text-text-secondary" />
        <input type="text" placeholder="搜索条目..."
          onChange={(e) => onSearch(e.target.value)}
          className="w-full bg-surface border border-surface-hover rounded-lg pl-10 pr-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary" />
      </div>

      {showNew && (
        <div className="space-y-2 p-4 bg-surface border border-surface-hover rounded-xl">
          <input type="text" value={newTitle} onChange={(e) => setNewTitle(e.target.value)} placeholder="标题"
            className="w-full bg-background border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary" />
          <div className="h-[240px] rounded-lg border border-surface-hover overflow-hidden bg-background">
            <MarkdownEditor value={newContent} onChange={setNewContent} />
          </div>
          <div className="flex gap-2">
            <button onClick={handleCreate} className="px-3 py-1.5 bg-primary text-white rounded-lg text-sm">创建</button>
            <button onClick={() => setShowNew(false)} className="px-3 py-1.5 bg-surface-hover text-text-secondary rounded-lg text-sm">取消</button>
          </div>
        </div>
      )}

      {isLoading ? (
        <div className="flex justify-center py-8"><Loader2 size={24} className="animate-spin text-text-secondary" /></div>
      ) : items.length === 0 ? (
        <p className="text-center text-text-secondary py-8 text-sm">暂无条目</p>
      ) : (
        <div className="space-y-2">
          {items.map((item) => {
            const tags = parseKnowledgeTags(item);
            const isEditing = editingId === item.id;
            return (
              <div key={item.id}
                className="p-3 bg-surface border border-surface-hover rounded-xl group hover:border-primary/30 transition-colors">
                {isEditing ? (
                  <div className="space-y-2">
                    <input type="text" value={editTitle} onChange={(e) => setEditTitle(e.target.value)}
                      className="w-full bg-background border border-surface-hover rounded px-2 py-1 text-sm text-text-primary focus:outline-none focus:border-primary" />
                    <div className="h-[240px] rounded border border-surface-hover overflow-hidden bg-background">
                      <MarkdownEditor value={editContent} onChange={setEditContent} />
                    </div>
                    <div className="flex gap-1">
                      <button onClick={handleSaveEdit} className="px-2 py-1 bg-primary text-white rounded text-xs">保存</button>
                      <button onClick={() => setEditingId(null)} className="px-2 py-1 bg-surface-hover rounded text-xs">取消</button>
                    </div>
                  </div>
                ) : (
                  <>
                    <div className="flex items-start gap-3 cursor-pointer" onClick={() => onViewItem?.(item.id)}>
                      <div className="flex-1 min-w-0">
                        <h3 className="text-sm font-medium text-text-primary truncate">{item.title}</h3>
                        {item.content && <p className="text-xs text-text-secondary mt-1 line-clamp-2">{item.content}</p>}
                        <div className="flex items-center gap-2 mt-2">
                          <span className="text-xs text-text-secondary/60">{item.content_type}</span>
                          {item.source_type && (
                            <span className="text-xs text-text-secondary/60 flex items-center gap-1">
                              <ExternalLink size={10} />{item.source_type}
                            </span>
                          )}
                          {tags.length > 0 && (
                            <span className="flex items-center gap-1 text-xs text-text-secondary/60">
                              <Tag size={10} />{tags.join(', ')}
                            </span>
                          )}
                          <span className="text-xs text-text-secondary/60">{new Date(item.updated_at).toLocaleDateString()}</span>
                        </div>
                      </div>
                      <div className="flex gap-1 opacity-0 group-hover:opacity-100 transition-all">
                        {onUpdateItem && (
                          <button onClick={(e) => { e.stopPropagation(); handleStartEdit(item); }}
                            className="p-1 rounded hover:bg-surface-hover text-xs text-text-secondary">编辑</button>
                        )}
                        <ConfirmButton
                          icon
                          onConfirm={() => onDeleteItem(item.id)}
                          confirmText="确认删除"
                          aria-label="删除知识条目"
                        >
                          <Trash2 size={14} />
                        </ConfirmButton>
                      </div>
                    </div>
                  </>
                )}
              </div>
            );
          })}
        </div>
      )}

      {/* Pagination */}
      {onPageChange && page !== undefined && (
        <div className="flex items-center justify-center gap-3 pt-2">
          <button onClick={() => onPageChange(page - 1)} disabled={page <= 0}
            className="p-1 rounded hover:bg-surface-hover disabled:opacity-30"><ChevronLeft size={16} /></button>
          <span className="text-xs text-text-secondary">第 {page + 1} 页</span>
          <button onClick={() => onPageChange(page + 1)} disabled={!hasMore}
            className="p-1 rounded hover:bg-surface-hover disabled:opacity-30"><ChevronRight size={16} /></button>
        </div>
      )}
    </div>
  );
}
