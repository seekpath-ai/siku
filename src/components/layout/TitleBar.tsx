import { Minus, Square, Copy, X, FileText, Search, Bookmark, Star, Menu, CircleHelp } from 'lucide-react';
import { useState, useEffect, useCallback, useRef } from 'react';
import { createPortal } from 'react-dom';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useRouterState, useNavigate } from '@tanstack/react-router';
import { useShellStore } from '@/stores/shellStore';
import { useTabStore } from '@/stores/tabStore';
import { notesCreate, bookmarksCreate } from '@/lib/tauri';
import { openNoteTab } from '@/lib/openNote';

function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

function SidebarToggleIcon({ collapsed }: { collapsed: boolean }) {
  return (
    <div className="relative w-4 h-4 border border-current rounded-[3px]">
      <div
        className={`absolute left-[3px] top-[3px] bottom-[3px] rounded-[2px] bg-current transition-all duration-200 ${
          collapsed ? 'w-[1px]' : 'w-[3px]'
        }`}
      />
    </div>
  );
}

interface MenuItem {
  label?: string;
  shortcut?: string;
  action?: () => void;
  disabled?: boolean;
  separator?: boolean;
}

let currentZoom = 1;
const applyZoom = async (delta: number) => {
  currentZoom = delta === 0 ? 1 : Math.max(0.5, Math.min(2, currentZoom + delta));
  if (typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window) {
    const { getCurrentWebview } = await import('@tauri-apps/api/webview');
    await getCurrentWebview().setZoom(currentZoom);
  }
};

export function TitleBar() {
  const [inTauri, setInTauri] = useState(false);
  const [openMenu, setOpenMenu] = useState<string | null>(null);
  const [tabContextMenu, setTabContextMenu] = useState<{ x: number; y: number; tabId: string } | null>(null);
  const [helpOpen, setHelpOpen] = useState(false);
  const [appVersion, setAppVersion] = useState('0.1.0');
  const tabContextMenuRef = useRef<HTMLDivElement>(null);
  const menuBarRef = useRef<HTMLDivElement>(null);
  const helpBtnRef = useRef<HTMLButtonElement>(null);
  const helpPanelRef = useRef<HTMLDivElement>(null);

  const { sidePanelCollapsed, toggleSidePanel, isMaximized, setIsMaximized } = useShellStore();
  const { location } = useRouterState();
  const isReader = location.pathname.startsWith('/reader/');

  const { tabs, activeTabId, activate, close, closeAll, closeOthers, closeToRight, closeToLeft, openRoute } = useTabStore();
  const navigate = useNavigate();

  useEffect(() => {
    setInTauri(isTauri());
    if (!isTauri()) return;
    let unlisten: (() => void) | undefined;
    const win = getCurrentWindow();
    win.isMaximized().then(setIsMaximized).catch(() => {});
    win.onResized(async () => { const m = await win.isMaximized(); setIsMaximized(m); }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [setIsMaximized]);

  useEffect(() => {
    if (!openMenu) return;
    const handler = (e: MouseEvent) => {
      if (menuBarRef.current && !menuBarRef.current.contains(e.target as Node)) {
        setOpenMenu(null);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [openMenu]);

  useEffect(() => {
    if (!openMenu) return;
    const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') setOpenMenu(null); };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, [openMenu]);

  // Fetch the app version (Tauri) with a package fallback.
  useEffect(() => {
    if (!isTauri()) return;
    import('@tauri-apps/api/app')
      .then(({ getVersion }) => getVersion())
      .then((v) => setAppVersion(v))
      .catch(() => { /* keep fallback */ });
  }, []);

  // Close the help panel on click-away / Escape.
  useEffect(() => {
    if (!helpOpen) return;
    const onMouseDown = (e: MouseEvent) => {
      const btn = helpBtnRef.current;
      const panel = helpPanelRef.current;
      if (btn && btn.contains(e.target as Node)) return;
      if (panel && panel.contains(e.target as Node)) return;
      setHelpOpen(false);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setHelpOpen(false);
    };
    document.addEventListener('mousedown', onMouseDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('mousedown', onMouseDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [helpOpen]);

  const handleMinimize = useCallback(() => getCurrentWindow().minimize().catch(() => {}), []);
  const handleToggleMaximize = useCallback(() => getCurrentWindow().toggleMaximize().catch(() => {}), []);
  const handleClose = useCallback(() => getCurrentWindow().close().catch(() => {}), []);

  const openRouteTab = useCallback((route: string, title: string, icon?: string) => {
    const tab = openRoute(route, { title, icon });
    if (tab.params) {
      navigate({ to: tab.route, params: tab.params });
    } else {
      navigate({ to: tab.route });
    }
  }, [navigate, openRoute]);

  const creatingNoteRef = useRef(false);
  const handleNewNote = useCallback(async () => {
    if (creatingNoteRef.current) return;
    creatingNoteRef.current = true;
    try {
      const note = await notesCreate('Untitled.md', '', undefined, undefined, false);
      openNoteTab(navigate, note);
      window.dispatchEvent(new CustomEvent('siku:note-created', { detail: note.id }));
    } catch (err) {
      console.error('新建笔记失败:', err);
    } finally {
      setTimeout(() => { creatingNoteRef.current = false; }, 300);
    }
  }, [navigate]);

  const handleActivateTab = useCallback((tabId: string) => {
    const tab = useTabStore.getState().findById(tabId);
    if (!tab) return;
    activate(tabId);
    navigate({ to: tab.route, params: tab.params, search: tab.search });
  }, [activate, navigate]);

  const handleCloseTab = useCallback((e: React.MouseEvent, tabId: string) => {
    e.stopPropagation();
    close(tabId);
    const { activeTabId: nextId, tabs: remaining } = useTabStore.getState();
    if (nextId) {
      const next = remaining.find((t) => t.id === nextId);
      if (next) navigate({ to: next.route, params: next.params, search: next.search });
      else navigate({ to: '/library' });
    } else {
      navigate({ to: '/library' });
    }
  }, [close, navigate]);

  const handleTabContextMenu = useCallback((e: React.MouseEvent, tabId: string) => {
    e.preventDefault();
    setTabContextMenu({ x: e.clientX, y: e.clientY, tabId });
  }, []);

  const runContextAction = useCallback((action: () => void, targetTabId: string) => {
    action();
    setTabContextMenu(null);
    const { activeTabId: nextId, tabs: remaining } = useTabStore.getState();
    if (nextId && nextId !== targetTabId) {
      const next = remaining.find((t) => t.id === nextId);
      if (next) navigate({ to: next.route, params: next.params, search: next.search });
    }
  }, [navigate]);

  // Close the tab context menu when clicking outside.
  useEffect(() => {
    if (!tabContextMenu) return;
    const onMouseDown = (e: MouseEvent) => {
      if (tabContextMenuRef.current && !tabContextMenuRef.current.contains(e.target as Node)) {
        setTabContextMenu(null);
      }
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setTabContextMenu(null);
    };
    document.addEventListener('mousedown', onMouseDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('mousedown', onMouseDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [tabContextMenu]);

  // Keep the active tab indicator in sync with the actual route. Some
  // navigations (browser back/forward, in-page links, keyboard shortcuts)
  // change the URL without updating activeTabId, causing the wrong tab to be
  // highlighted.
  useEffect(() => {
    const pathname = location.pathname;
    const noteSearch = (location.search as { note?: unknown }).note;
    const matched = tabs.find((t) => {
      // Note tabs are keyed by their ?note=<id> search param: match exactly so
      // the "笔记" list-home tab (no search) and individual note tabs highlight
      // independently.
      if (t.route === '/notes') {
        if (pathname !== '/notes') return false;
        const tabNote = typeof t.search?.note === 'string' ? t.search.note : undefined;
        const locNote = typeof noteSearch === 'string' ? noteSearch : undefined;
        return tabNote === locNote;
      }
      if (t.route === '/reader/$paperId') {
        return pathname.startsWith('/reader/') && t.params?.paperId === pathname.split('/')[2];
      }
      return pathname.startsWith(t.route);
    });
    if (matched && matched.id !== activeTabId) {
      activate(matched.id);
    }
  }, [location.pathname, location.search, tabs, activeTabId, activate]);

  const dispatch = (name: string, detail?: string) => {
    window.dispatchEvent(new CustomEvent(name, detail ? { detail } : undefined));
  };

  const appMenu: MenuItem[] = [
    { label: '导入 PDF...', shortcut: 'Ctrl+I', action: () => dispatch('siku:import-pdf') },
    { label: '从链接导入...', action: () => dispatch('siku:import-from-link') },
    { label: '新建笔记', shortcut: 'Ctrl+N', action: handleNewNote },
    { separator: true },
    { label: '设置', shortcut: 'Ctrl+,', action: () => openRouteTab('/settings', '设置', 'settings') },
  ];

  const editMenu: MenuItem[] = [
    { label: '撤销', shortcut: 'Ctrl+Z', action: () => document.execCommand('undo') },
    { label: '重做', shortcut: 'Ctrl+Y', action: () => document.execCommand('redo') },
    { separator: true },
    { label: '剪切', shortcut: 'Ctrl+X', action: () => document.execCommand('cut') },
    { label: '复制', shortcut: 'Ctrl+C', action: () => document.execCommand('copy') },
    { label: '粘贴', shortcut: 'Ctrl+V', action: () => document.execCommand('paste') },
  ];

  const viewMenu: MenuItem[] = [
    { label: '放大', shortcut: 'Ctrl+=', action: () => applyZoom(0.1) },
    { label: '缩小', shortcut: 'Ctrl+-', action: () => applyZoom(-0.1) },
    { label: '重置缩放', shortcut: 'Ctrl+0', action: () => applyZoom(0) },
    { separator: true },
    { label: '切换翻译面板', shortcut: 'Ctrl+T', action: () => dispatch('siku:toggle-translation') },
  ];

  const goMenu: MenuItem[] = [
    { label: '图书馆', shortcut: 'Ctrl+1', action: () => openRouteTab('/library', '图书馆', 'home') },
    { label: '对话', shortcut: 'Ctrl+2', action: () => openRouteTab('/chat', '对话', 'chat') },
    { label: '笔记', shortcut: 'Ctrl+3', action: () => openRouteTab('/notes', '笔记', 'note') },
    { label: '知识库', shortcut: 'Ctrl+4', action: () => openRouteTab('/knowledge', '知识库', 'knowledge') },
    { label: '文件', shortcut: 'Ctrl+5', action: () => openRouteTab('/files', '文件', 'files') },
  ];

  const toolsMenu: MenuItem[] = [
    { label: '研究追踪', shortcut: 'Ctrl+R', action: () => openRouteTab('/research', '研究追踪', 'research') },
    { separator: true },
    { label: '关系图谱', action: () => openRouteTab('/graph', '关系图谱', 'graph') },
  ];

  const quitItem: MenuItem = { label: '退出', shortcut: 'Alt+F4', action: handleClose };

  const menuGroups: { label: string; items: MenuItem[] }[] = [
    { label: '文件', items: appMenu },
    { label: '编辑', items: editMenu },
    { label: '查看', items: viewMenu },
    { label: '前往', items: goMenu },
    { label: '工具', items: toolsMenu },
    { label: '', items: [{ separator: true }, quitItem] },
  ];

  return (
    <div
      data-tauri-drag-region="deep"
      className="titlebar-drag flex items-center h-[38px] bg-surface border-b border-surface-hover select-none shrink-0"
      onDoubleClick={inTauri ? handleToggleMaximize : undefined}
    >
      {/* Left: sidebar toggle + menu + view toggles */}
      <div className="flex items-center h-full gap-0.5 pl-1.5" ref={menuBarRef}>
        {!isReader && (
          <button
            onClick={toggleSidePanel}
            className="h-[28px] w-[28px] flex items-center justify-center rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
            title={sidePanelCollapsed ? '展开侧边栏' : '折叠侧边栏'}
          >
            <SidebarToggleIcon collapsed={sidePanelCollapsed} />
          </button>
        )}

        <div className="relative">
          <button
            onClick={() => setOpenMenu(openMenu === 'app' ? null : 'app')}
            className={`h-[28px] w-[28px] flex items-center justify-center rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors ${openMenu === 'app' ? 'bg-surface-hover text-text-primary' : ''}`}
            title="菜单"
          >
            <Menu size={15} />
          </button>
          {openMenu === 'app' && (
            <div data-no-drag data-tauri-drag-region="false" className="absolute top-full left-0 mt-0.5 bg-surface border border-surface-hover rounded-lg shadow-xl z-50 py-1 min-w-[200px] max-h-[70vh] overflow-y-auto">
              {menuGroups.map((group, groupIdx) => (
                <div key={group.label || `group-${groupIdx}`}>
                  {group.label && (
                    <div className="px-3 py-1 text-[10px] text-text-secondary/60 uppercase tracking-wider">{group.label}</div>
                  )}
                  {group.items.map((item, i) =>
                    item.separator ? (
                      <div key={i} className="my-1 border-t border-surface-hover" />
                    ) : (
                      <button
                        key={i}
                        disabled={item.disabled}
                        onClick={() => { item.action?.(); setOpenMenu(null); }}
                        className="w-full flex items-center justify-between px-3 py-1.5 text-xs text-text-primary hover:bg-primary/20 hover:text-primary disabled:opacity-30 disabled:cursor-default transition-colors"
                      >
                        <span>{item.label}</span>
                        {item.shortcut && <span className="text-text-secondary/50 ml-4 text-[10px]">{item.shortcut}</span>}
                      </button>
                    )
                  )}
                </div>
              ))}
            </div>
          )}
        </div>

        <button
          onClick={() => openRouteTab('/notes', '笔记', 'note')}
          className={`h-[28px] w-[28px] flex items-center justify-center rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors ${location.pathname.startsWith('/notes') ? 'text-primary' : ''}`}
          title="笔记"
        >
          <FileText size={15} />
        </button>
        <button
          onClick={() => dispatch('siku:global-search')}
          className="h-[28px] w-[28px] flex items-center justify-center rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
          title="搜索"
        >
          <Search size={15} />
        </button>
        <button
          onClick={async () => {
            const tab = tabs.find((t) => t.id === activeTabId);
            const title = tab?.title || document.title || location.pathname;
            try {
              await bookmarksCreate({ title, route: location.pathname, params_json: JSON.stringify(location.search) });
            } catch (err) {
              console.error('添加书签失败:', err);
            }
          }}
          className="h-[28px] w-[28px] flex items-center justify-center rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
          title="收藏当前页 (Ctrl+D)"
        >
          <Star size={15} />
        </button>
        <button
          onClick={() => openRouteTab('/bookmarks', '书签', 'bookmark')}
          className={`h-[28px] w-[28px] flex items-center justify-center rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors ${location.pathname === '/bookmarks' ? 'text-primary' : ''}`}
          title="书签"
        >
          <Bookmark size={15} />
        </button>
      </div>

      {/* Center: tabs */}
      <div className="flex-1 flex items-center h-full overflow-hidden">
        <button
          ref={helpBtnRef}
          onClick={() => setHelpOpen((v) => !v)}
          className={`h-6 w-6 mx-1 flex items-center justify-center rounded transition-colors shrink-0 ${
            helpOpen ? 'bg-surface-hover text-primary' : 'text-text-secondary hover:text-text-primary hover:bg-surface-hover'
          }`}
          title="应用信息与快捷键"
        >
          <CircleHelp size={15} />
        </button>
        {helpOpen &&
          helpBtnRef.current &&
          createPortal(
            (() => {
              const rect = helpBtnRef.current!.getBoundingClientRect();
              const left = Math.min(rect.left, window.innerWidth - 300 - 8);
              return (
                <div
                  ref={helpPanelRef}
                  data-no-drag
                  className="fixed z-[5000] w-[300px] max-h-[70vh] overflow-y-auto bg-surface border border-surface-hover rounded-lg shadow-2xl"
                  style={{ left, top: rect.bottom + 6 }}
                >
                  {/* Version */}
                  <div className="px-4 py-3 border-b border-surface-hover">
                    <div className="text-sm font-semibold text-text-primary">思库</div>
                    <div className="text-xs text-text-secondary/60 mt-0.5">版本 {appVersion}</div>
                  </div>

                  {/* Common actions */}
                  <div className="px-1.5 py-2 border-b border-surface-hover">
                    <div className="px-2.5 py-1 text-[10px] text-text-secondary/60 uppercase tracking-wider">常用操作</div>
                    {[
                      { label: '全局搜索', shortcut: 'Ctrl+K', action: () => dispatch('siku:global-search') },
                      { label: '新建笔记', shortcut: 'Ctrl+N', action: handleNewNote },
                      { label: '导入 PDF', shortcut: 'Ctrl+I', action: () => dispatch('siku:import-pdf') },
                      { label: '打开设置', shortcut: 'Ctrl+,', action: () => openRouteTab('/settings', '设置', 'settings') },
                    ].map((cmd) => (
                      <button
                        key={cmd.label}
                        onClick={() => { cmd.action(); setHelpOpen(false); }}
                        className="w-full flex items-center justify-between px-2.5 py-1.5 rounded text-xs text-text-primary hover:bg-primary/20 hover:text-primary transition-colors"
                      >
                        <span>{cmd.label}</span>
                        <span className="text-text-secondary/50 text-[10px]">{cmd.shortcut}</span>
                      </button>
                    ))}
                  </div>

                  {/* Shortcut reference */}
                  <div className="px-1.5 py-2">
                    <div className="px-2.5 py-1 text-[10px] text-text-secondary/60 uppercase tracking-wider">快捷键</div>
                    {[
                      ['图书馆', 'Ctrl+1'], ['对话', 'Ctrl+2'], ['笔记', 'Ctrl+3'],
                      ['知识库', 'Ctrl+4'], ['文件', 'Ctrl+5'], ['科研追踪', 'Ctrl+R'],
                      ['收藏当前页', 'Ctrl+D'], ['切换宠物', 'Ctrl+Shift+P'],
                      ['放大 / 缩小', 'Ctrl+= / Ctrl+-'], ['重置缩放', 'Ctrl+0'],
                      ['翻译面板', 'Ctrl+T'], ['退出', 'Alt+F4'],
                    ].map(([label, shortcut]) => (
                      <div key={label} className="flex items-center justify-between px-2.5 py-1 rounded text-xs text-text-secondary">
                        <span>{label}</span>
                        <span className="text-text-secondary/40 text-[10px]">{shortcut}</span>
                      </div>
                    ))}
                  </div>
                </div>
              );
            })(),
            document.body
          )}
        <div className="flex items-center h-full overflow-hidden">
          {tabs.map((tab) => {
            const isActive = tab.id === activeTabId;
            return (
              <div
                key={tab.id}
                data-no-drag
                data-tauri-drag-region="false"
                onClick={() => handleActivateTab(tab.id)}
                onContextMenu={(e) => handleTabContextMenu(e, tab.id)}
                className={`flex items-center gap-1.5 px-3 h-[28px] text-xs cursor-pointer border-r border-surface-hover transition-colors min-w-0 max-w-[180px] group select-none ${
                  isActive
                    ? 'bg-background text-text-primary border-t-2 border-t-primary'
                    : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
                }`}
              >
                <span className="truncate flex-1">{tab.title}</span>
                {tab.closable !== false && (
                  <button
                    onClick={(e) => handleCloseTab(e, tab.id)}
                    className="shrink-0 p-0.5 rounded hover:bg-surface-hover text-text-secondary/40 hover:text-red-400 opacity-0 group-hover:opacity-100 transition-opacity"
                  >
                    <X size={11} />
                  </button>
                )}
              </div>
            );
          })}
        </div>
      </div>

      {/* Tab context menu */}
      {tabContextMenu && (
        <div
          ref={tabContextMenuRef}
          style={{ left: tabContextMenu.x, top: tabContextMenu.y }}
          className="fixed z-[5000] min-w-[140px] py-1 bg-surface border border-surface-hover rounded-lg shadow-xl"
        >
          {[
            { label: '关闭', action: () => runContextAction(() => close(tabContextMenu.tabId), tabContextMenu.tabId) },
            { label: '关闭其他', action: () => runContextAction(() => closeOthers(tabContextMenu.tabId), tabContextMenu.tabId) },
            { label: '关闭右侧', action: () => runContextAction(() => closeToRight(tabContextMenu.tabId), tabContextMenu.tabId) },
            { label: '关闭左侧', action: () => runContextAction(() => closeToLeft(tabContextMenu.tabId), tabContextMenu.tabId) },
            { label: '全部关闭', action: () => runContextAction(() => closeAll(), tabContextMenu.tabId) },
          ].map((item, idx) => (
            <button
              key={item.label}
              onClick={(e) => { e.stopPropagation(); item.action(); }}
              className={`w-full text-left px-3 py-1.5 text-xs text-text-primary hover:bg-primary/20 hover:text-primary transition-colors ${
                idx === 0 ? '' : 'border-t border-surface-hover'
              }`}
            >
              {item.label}
            </button>
          ))}
        </div>
      )}

      {/* Right: window controls (Tauri only) */}
      {inTauri && (
        <div className="flex items-center h-full shrink-0">
          <button onClick={handleMinimize} className="h-full w-11 flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors">
            <Minus size={16} strokeWidth={1.5} />
          </button>
          <button onClick={handleToggleMaximize} className="h-full w-11 flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors">
            {isMaximized ? <Copy size={14} strokeWidth={1.5} /> : <Square size={14} strokeWidth={1.5} />}
          </button>
          <button onClick={handleClose} className="h-full w-11 flex items-center justify-center text-text-secondary hover:text-white hover:bg-red-500/80 transition-colors">
            <X size={18} strokeWidth={1.5} />
          </button>
        </div>
      )}
    </div>
  );
}
