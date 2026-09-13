import { useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useNavigate } from '@tanstack/react-router';
import { FileText, FileType2, Search } from 'lucide-react';
import { useTabStore, type Tab } from '@/stores/tabStore';

/**
 * Recently-used tab order, newest first.
 *
 * Module scope on purpose: the switcher unmounts between uses, and the MRU list
 * is the whole point — `Ctrl+Tab` twice should go back to where you came from.
 */
const mru: string[] = [];
function touch(id: string) {
  const i = mru.indexOf(id);
  if (i >= 0) mru.splice(i, 1);
  mru.unshift(id);
}

/** Tabs in most-recently-used order, with never-activated ones appended in
 *  strip order so nothing is unreachable. */
function inMruOrder(tabs: Tab[]): Tab[] {
  const byId = new Map(tabs.map((t) => [t.id, t]));
  const out: Tab[] = [];
  for (const id of mru) {
    const tab = byId.get(id);
    if (tab) {
      out.push(tab);
      byId.delete(id);
    }
  }
  for (const tab of tabs) if (byId.has(tab.id)) out.push(tab);
  return out;
}

/** Small icon mirroring the tab strip's (kept local: the strip's map is tied to
 *  its own lucide imports). */
function switcherIcon(icon?: string) {
  if (icon === 'note') return <FileText size={13} className="shrink-0 opacity-70" />;
  return <FileType2 size={13} className="shrink-0 opacity-70" />;
}

/**
 * `Ctrl+Tab` tab switcher: the list of open tabs in recent-use order with a
 * filter box. Mainstream editors all provide one — a strip that stays readable
 * cannot also hold every tab, so the overflow needs a list. Arrow keys move,
 * Enter switches, Esc closes; `Ctrl+Shift+Tab` opens it on the previous tab.
 */
export function TabSwitcher() {
  const tabs = useTabStore((s) => s.tabs);
  const activeTabId = useTabStore((s) => s.activeTabId);
  const activate = useTabStore((s) => s.activate);
  const navigate = useNavigate();

  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [index, setIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (activeTabId) touch(activeTabId);
  }, [activeTabId]);

  const ordered = useMemo(() => inMruOrder(tabs), [tabs]);
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return ordered;
    return ordered.filter((t) => t.title.toLowerCase().includes(q));
  }, [ordered, query]);

  const switchTo = (tab: Tab) => {
    activate(tab.id);
    navigate({ to: tab.route, params: tab.params, search: tab.search });
    setOpen(false);
    setQuery('');
  };

  // Ctrl+Tab opens the list (Ctrl+Shift+Tab preselects the previous tab).
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.key !== 'Tab') return;
      e.preventDefault();
      setQuery('');
      // Index 1 = the tab used before the current one: pressing Ctrl+Tab twice
      // should return you to where you came from, like every editor does.
      setIndex(Math.min(1, tabs.length - 1));
      setOpen(true);
    };
    window.addEventListener('keydown', onKeyDown, true);
    return () => window.removeEventListener('keydown', onKeyDown, true);
  }, [tabs.length]);

  useEffect(() => {
    if (!open) return;
    inputRef.current?.focus();
  }, [open]);

  if (!open) return null;

  return createPortal(
    <div
      data-no-drag
      data-tauri-drag-region="false"
      className="fixed inset-0 z-[5000] flex items-start justify-center bg-black/50 pt-[12vh]"
      onClick={(e) => {
        if (e.target === e.currentTarget) setOpen(false);
      }}
    >
      <div className="w-[520px] max-w-[92vw] max-h-[60vh] flex flex-col rounded-xl bg-surface border border-surface-hover shadow-2xl overflow-hidden">
        <div className="flex items-center gap-2 px-3 h-10 border-b border-surface-hover shrink-0">
          <Search size={14} className="text-text-secondary shrink-0" />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setIndex(0);
            }}
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                e.preventDefault();
                setOpen(false);
              } else if (e.key === 'ArrowDown' || (e.key === 'Tab' && !e.shiftKey)) {
                e.preventDefault();
                setIndex((i) => (filtered.length === 0 ? 0 : (i + 1) % filtered.length));
              } else if (e.key === 'ArrowUp' || (e.key === 'Tab' && e.shiftKey)) {
                e.preventDefault();
                setIndex((i) => (filtered.length === 0 ? 0 : (i - 1 + filtered.length) % filtered.length));
              } else if (e.key === 'Enter') {
                e.preventDefault();
                const tab = filtered[index];
                if (tab) switchTo(tab);
              }
            }}
            placeholder="搜索打开的标签…"
            className="flex-1 bg-transparent border-0 outline-0 focus:outline-none text-sm text-text-primary placeholder:text-text-secondary/50"
          />
          <span className="text-[10px] text-text-secondary/60 shrink-0">↑↓ 选择 · Enter 切换 · Esc 关闭</span>
        </div>
        <div className="flex-1 overflow-y-auto py-1">
          {filtered.length === 0 && (
            <div className="px-3 py-4 text-xs text-text-secondary">没有匹配的标签</div>
          )}
          {filtered.map((tab, i) => (
            <button
              key={tab.id}
              onMouseEnter={() => setIndex(i)}
              onClick={() => switchTo(tab)}
              className={`w-full flex items-center gap-2 px-3 py-1.5 text-left text-xs transition-colors ${
                i === index ? 'bg-primary/20 text-primary' : 'text-text-primary hover:bg-surface-hover'
              }`}
            >
              {switcherIcon(tab.icon)}
              <span className="truncate flex-1">{tab.title}</span>
              {tab.id === activeTabId && (
                <span className="text-[10px] text-text-secondary/70 shrink-0">当前</span>
              )}
            </button>
          ))}
        </div>
      </div>
    </div>,
    document.body
  );
}
