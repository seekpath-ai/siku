import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { useNavigate } from '@tanstack/react-router';
import {
  Search,
  X,
  FileText,
  BookOpen,
  FolderOpen,
  MessageSquare,
  Settings,
  Plus,
  Upload,
  Library,
  Bot,
  FlaskConical,
  GitGraph,
  Bookmark,
  Clock,
  Folder,
} from 'lucide-react';
import { useTabStore } from '@/stores/tabStore';
import { openNoteTab } from '@/lib/openNote';
import {
  notesSearch,
  listPapers,
  knowledgeListItems,
  listChatSessions,
  notesCreate,
  bookmarksList,
} from '@/lib/tauri';
import type { NoteSearchResult } from '@/lib/tauri';
import type { Paper, KnowledgeItem, AgentSession, Bookmark as BookmarkType } from '@/lib/types';
import { useChatStore } from '@/stores/chatStore';

interface SearchItem {
  id: string;
  type: 'command' | 'note' | 'paper' | 'knowledge' | 'chat' | 'bookmark';
  title: string;
  subtitle?: string;
  icon: React.ReactNode;
  action: () => void;
}

interface GlobalSearchProps {
  onImportPdf?: () => void;
}

function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

export function GlobalSearch({ onImportPdf }: GlobalSearchProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [items, setItems] = useState<SearchItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [bookmarks, setBookmarks] = useState<BookmarkType[]>([]);
  const inputRef = useRef<HTMLInputElement>(null);
  const creatingNoteRef = useRef(false);
  const navigate = useNavigate();
  const { openRoute, open: openTab } = useTabStore();

  const goTo = useCallback((route: string, title: string, icon?: string) => {
    const tab = openRoute(route, { title, icon });
    if (tab.params) {
      navigate({ to: tab.route, params: tab.params });
    } else {
      navigate({ to: tab.route });
    }
  }, [openRoute, navigate]);

  const openBookmark = useCallback((bookmark: BookmarkType) => {
    const tab = openRoute(bookmark.route, { title: bookmark.title, icon: 'bookmark' });
    try {
      const params = JSON.parse(bookmark.params_json || '{}');
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
  }, [openRoute, navigate]);

  // Static command palette items.
  const commandItems = useMemo<SearchItem[]>(() => {
    const cmds: SearchItem[] = [
      {
        id: 'cmd-new-note',
        type: 'command',
        title: '新建笔记',
        subtitle: 'Ctrl + N',
        icon: <Plus size={16} />,
        action: async () => {
          if (!isTauri()) return;
          if (creatingNoteRef.current) return;
          creatingNoteRef.current = true;
          try {
            const note = await notesCreate('Untitled.md', '', undefined, undefined, false);
            const id = `note_${note.id}`;
            openTab({ id, title: note.title || 'Untitled.md', route: '/notes', icon: 'note' });
            navigate({ to: '/notes', search: { note: note.id } });
            window.dispatchEvent(new CustomEvent('siku:note-created', { detail: note.id }));
          } catch (err) {
            console.error('新建笔记失败:', err);
          } finally {
            setTimeout(() => { creatingNoteRef.current = false; }, 300);
          }
        },
      },
      {
        id: 'cmd-import-pdf',
        type: 'command',
        title: '导入 PDF',
        subtitle: 'Ctrl + I',
        icon: <Upload size={16} />,
        action: () => onImportPdf?.(),
      },
      {
        id: 'cmd-library',
        type: 'command',
        title: '前往图书馆',
        subtitle: 'Ctrl + 1',
        icon: <Library size={16} />,
        action: () => goTo('/library', '图书馆', 'home'),
      },
      {
        id: 'cmd-chat',
        type: 'command',
        title: '前往对话',
        subtitle: 'Ctrl + 2',
        icon: <Bot size={16} />,
        action: () => goTo('/chat', '对话', 'chat'),
      },
      {
        id: 'cmd-notes',
        type: 'command',
        title: '前往笔记',
        subtitle: 'Ctrl + 3',
        icon: <FileText size={16} />,
        action: () => goTo('/notes', '笔记', 'note'),
      },
      {
        id: 'cmd-knowledge',
        type: 'command',
        title: '前往知识库',
        subtitle: 'Ctrl + 4',
        icon: <FolderOpen size={16} />,
        action: () => goTo('/knowledge', '知识库', 'knowledge'),
      },
      {
        id: 'cmd-files',
        type: 'command',
        title: '前往文件列表',
        subtitle: 'Ctrl + 5',
        icon: <Folder size={16} />,
        action: () => goTo('/files', '文件列表', 'files'),
      },
      {
        id: 'cmd-research',
        type: 'command',
        title: '前往科研追踪',
        subtitle: 'Ctrl + R',
        icon: <FlaskConical size={16} />,
        action: () => goTo('/research', '科研追踪', 'research'),
      },
      {
        id: 'cmd-graph',
        type: 'command',
        title: '前往知识图谱',
        icon: <GitGraph size={16} />,
        action: () => goTo('/graph', '知识图谱', 'graph'),
      },
      {
        id: 'cmd-bookmarks',
        type: 'command',
        title: '前往书签',
        icon: <Bookmark size={16} />,
        action: () => goTo('/bookmarks', '书签', 'bookmark'),
      },
      {
        id: 'cmd-timeline',
        type: 'command',
        title: '前往时间轴',
        icon: <Clock size={16} />,
        action: () => goTo('/timeline', '时间轴', 'clock'),
      },
      {
        id: 'cmd-settings',
        type: 'command',
        title: '打开设置',
        subtitle: 'Ctrl + ,',
        icon: <Settings size={16} />,
        action: () => goTo('/settings', '设置', 'settings'),
      },
    ];
    return cmds;
  }, [navigate, goTo, openTab, onImportPdf]);

  // Load bookmarks once when search opens.
  useEffect(() => {
    if (!open) return;
    bookmarksList().then(setBookmarks).catch(() => setBookmarks([]));
  }, [open]);

  // Listen for the global search event and Ctrl+K shortcut.
  useEffect(() => {
    const handleEvent = () => {
      setOpen(true);
      setQuery('');
      setItems(commandItems);
      setSelectedIndex(0);
      setTimeout(() => inputRef.current?.focus(), 50);
    };

    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        handleEvent();
      }
    };

    window.addEventListener('siku:global-search', handleEvent);
    window.addEventListener('keydown', handleKeyDown);
    return () => {
      window.removeEventListener('siku:global-search', handleEvent);
      window.removeEventListener('keydown', handleKeyDown);
    };
  }, [commandItems]);

  // Fetch search results when query changes.
  useEffect(() => {
    if (!open) return;

    const trimmed = query.trim();
    if (!trimmed) {
      setItems(commandItems);
      setSelectedIndex(0);
      return;
    }

    let cancelled = false;
    const timer = setTimeout(async () => {
      setLoading(true);
      try {
        const limit = 8;
        const [notes, papers, knowledge, chats] = await Promise.allSettled([
          notesSearch(trimmed, limit).catch(() => [] as NoteSearchResult[]),
          listPapers({ search: trimmed, limit }).catch(() => [] as Paper[]),
          knowledgeListItems(undefined, trimmed, undefined, limit).catch(() => [] as KnowledgeItem[]),
          listChatSessions().catch(() => [] as AgentSession[]),
        ]);

        if (cancelled) return;

        const results: SearchItem[] = [];

        if (notes.status === 'fulfilled') {
          results.push(
            ...notes.value.map((n) => ({
              id: `note-${n.id}`,
              type: 'note' as const,
              title: n.title || 'Untitled',
              subtitle: n.snippet,
              icon: <FileText size={16} className="text-emerald-400" />,
              action: () => {
                openNoteTab(navigate, { id: n.id, title: n.title || 'Untitled' });
              },
            }))
          );
        }

        if (papers.status === 'fulfilled') {
          results.push(
            ...papers.value.map((p) => ({
              id: `paper-${p.id}`,
              type: 'paper' as const,
              title: p.title,
              subtitle: Array.isArray(p.authors) ? p.authors.join(', ') : p.authors,
              icon: <BookOpen size={16} className="text-blue-400" />,
              action: () => {
                openRoute('/reader', { title: p.title, icon: 'paper' });
                navigate({ to: '/reader/$paperId', params: { paperId: p.id } });
              },
            }))
          );
        }

        if (knowledge.status === 'fulfilled') {
          results.push(
            ...knowledge.value.map((k) => ({
              id: `knowledge-${k.id}`,
              type: 'knowledge' as const,
              title: k.title,
              subtitle: k.content?.slice(0, 80),
              icon: <FolderOpen size={16} className="text-amber-400" />,
              action: () => {
                goTo(`/knowledge/${k.domain_id}`, k.title, 'knowledge');
              },
            }))
          );
        }

        if (chats.status === 'fulfilled') {
          const filtered = chats.value.filter((c) =>
            c.title?.toLowerCase().includes(trimmed.toLowerCase())
          );
          results.push(
            ...filtered.slice(0, limit).map((c) => ({
              id: `chat-${c.id}`,
              type: 'chat' as const,
              title: c.title || '未命名会话',
              icon: <MessageSquare size={16} className="text-purple-400" />,
              action: () => {
                openRoute('/chat', { title: c.title || '未命名会话', icon: 'chat' });
                useChatStore.getState().setActiveSession(c.id);
                navigate({ to: '/chat' });
              },
            }))
          );
        }

        if (bookmarks.length > 0) {
          const filtered = bookmarks.filter((b) =>
            b.title.toLowerCase().includes(trimmed.toLowerCase()) ||
            b.route.toLowerCase().includes(trimmed.toLowerCase())
          );
          results.push(
            ...filtered.slice(0, limit).map((b) => ({
              id: `bookmark-${b.id}`,
              type: 'bookmark' as const,
              title: b.title,
              subtitle: b.route,
              icon: <Bookmark size={16} className="text-rose-400" />,
              action: () => openBookmark(b),
            }))
          );
        }

        // Always include matching commands at the top.
        const matchedCommands = commandItems.filter((c) =>
          c.title.toLowerCase().includes(trimmed.toLowerCase())
        );
        setItems([...matchedCommands, ...results]);
        setSelectedIndex(0);
      } finally {
        if (!cancelled) setLoading(false);
      }
    }, 200);

    return () => {
      clearTimeout(timer);
      cancelled = true;
    };
  }, [query, open, commandItems, navigate, openRoute, bookmarks, openBookmark]);

  const close = useCallback(() => {
    setOpen(false);
    setQuery('');
    setItems(commandItems);
    setSelectedIndex(0);
  }, [commandItems]);

  const executeSelected = useCallback(() => {
    const item = items[selectedIndex];
    if (item) {
      item.action();
      close();
    }
  }, [items, selectedIndex, close]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      switch (e.key) {
        case 'ArrowDown':
          e.preventDefault();
          setSelectedIndex((i) => (i + 1) % items.length);
          break;
        case 'ArrowUp':
          e.preventDefault();
          setSelectedIndex((i) => (i - 1 + items.length) % items.length);
          break;
        case 'Enter':
          e.preventDefault();
          executeSelected();
          break;
        case 'Escape':
          e.preventDefault();
          close();
          break;
      }
    },
    [items.length, executeSelected, close]
  );

  if (!open) return null;

  const groups: { type: SearchItem['type']; label: string }[] = [
    { type: 'command', label: '命令' },
    { type: 'bookmark', label: '书签' },
    { type: 'note', label: '笔记' },
    { type: 'paper', label: '论文' },
    { type: 'knowledge', label: '知识库' },
    { type: 'chat', label: '对话' },
  ];

  return (
    <div className="fixed inset-0 z-[300] flex items-start justify-center pt-[15vh]">
      <div className="absolute inset-0 z-0 bg-black/40 backdrop-blur-sm" onClick={close} />
      <div
        className="relative z-10 w-full max-w-2xl mx-4 bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden flex flex-col max-h-[70vh]"
        onMouseDown={(e) => e.stopPropagation()}
      >
        {/* Search input */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-surface-hover">
          <Search size={18} className="text-text-secondary/60" />
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="搜索笔记、论文、知识库、对话或执行命令..."
            className="flex-1 bg-transparent text-sm text-text-primary placeholder:text-text-secondary/40 outline-none"
          />
          {loading && (
            <div className="w-4 h-4 rounded-full border-2 border-primary/30 border-t-primary animate-spin" />
          )}
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              close();
            }}
            className="p-2 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
          >
            <X size={16} />
          </button>
        </div>

        {/* Results */}
        <div className="overflow-y-auto py-2">
          {items.length === 0 ? (
            <div className="px-4 py-8 text-center text-sm text-text-secondary/60">未找到结果</div>
          ) : (
            groups.map((group) => {
              const groupItems = items.filter((i) => i.type === group.type);
              if (groupItems.length === 0) return null;
              return (
                <div key={group.type}>
                  <div className="px-4 py-1.5 text-[10px] font-medium text-text-secondary/50 uppercase tracking-wider">
                    {group.label}
                  </div>
                  {groupItems.map((item) => {
                    const globalIndex = items.findIndex((i) => i.id === item.id);
                    const isSelected = globalIndex === selectedIndex;
                    return (
                      <button
                        key={item.id}
                        onClick={() => {
                          setSelectedIndex(globalIndex);
                          item.action();
                          close();
                        }}
                        onMouseEnter={() => setSelectedIndex(globalIndex)}
                        className={`w-full flex items-center gap-3 px-4 py-2 text-left transition-colors ${
                          isSelected ? 'bg-primary/10 text-text-primary' : 'text-text-secondary hover:bg-surface-hover'
                        }`}
                      >
                        <span className={isSelected ? 'text-primary' : ''}>{item.icon}</span>
                        <div className="flex-1 min-w-0">
                          <div className={`text-sm truncate ${isSelected ? 'text-primary' : 'text-text-primary'}`}>
                            {item.title}
                          </div>
                          {item.subtitle && (
                            <div className="text-xs text-text-secondary/60 truncate">{item.subtitle}</div>
                          )}
                        </div>
                        {item.type === 'command' && (item as SearchItem).subtitle && (
                          <span className="text-[10px] text-text-secondary/40">{(item as SearchItem).subtitle}</span>
                        )}
                      </button>
                    );
                  })}
                </div>
              );
            })
          )}
        </div>

        {/* Footer hints */}
        <div className="flex items-center gap-4 px-4 py-2 border-t border-surface-hover bg-surface/50 text-[10px] text-text-secondary/50">
          <span>↑↓ 选择</span>
          <span>↵ 打开</span>
          <span>Esc 关闭</span>
        </div>
      </div>
    </div>
  );
}
