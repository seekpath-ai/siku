import { X, FileText, Bot, StickyNote, Folder, Settings, Home, Clock, FlaskConical, FolderOpen, GitGraph, Bookmark } from 'lucide-react';
import { useTabStore } from '@/stores/tabStore';
import { useShellStore } from '@/stores/shellStore';
import { useNavigate, useRouterState } from '@tanstack/react-router';
import { useCallback } from 'react';

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

const ICON_MAP: Record<string, React.ReactNode> = {
  home: <Home size={13} />,
  pdf: <FileText size={13} />,
  chat: <Bot size={13} />,
  note: <StickyNote size={13} />,
  notes: <StickyNote size={13} />,
  files: <Folder size={13} />,
  settings: <Settings size={13} />,
  knowledge: <FolderOpen size={13} />,
  research: <FlaskConical size={13} />,
  graph: <GitGraph size={13} />,
  bookmark: <Bookmark size={13} />,
  clock: <Clock size={13} />,
};

export function TabBar() {
  const { tabs, activeTabId, activate, close } = useTabStore();
  const { sidePanelCollapsed, toggleSidePanel } = useShellStore();
  const navigate = useNavigate();
  const { location } = useRouterState();
  const isReader = location.pathname.startsWith('/reader/');

  const handleActivate = useCallback((tabId: string) => {
    const tab = useTabStore.getState().findById(tabId);
    if (!tab) return;
    activate(tabId);
    navigate({ to: tab.route, params: tab.params as Record<string, string> });
  }, [activate, navigate]);

  const handleClose = useCallback((e: React.MouseEvent, tabId: string) => {
    e.stopPropagation();
    close(tabId);
    const { activeTabId: nextId, tabs: remaining } = useTabStore.getState();
    if (nextId) {
      const next = remaining.find((t) => t.id === nextId);
      if (next) navigate({ to: next.route, params: next.params as Record<string, string> });
      else navigate({ to: '/library' });
    } else {
      navigate({ to: '/library' });
    }
  }, [close, navigate]);

  return (
    <div className="flex items-center bg-surface/80 border-b border-surface-hover overflow-x-auto shrink-0">
      {!isTauri() && !isReader && (
        <button
          onClick={toggleSidePanel}
          className="h-8 w-8 mx-1 flex items-center justify-center rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors shrink-0"
          title={sidePanelCollapsed ? '展开侧边栏' : '折叠侧边栏'}
        >
          <SidebarToggleIcon collapsed={sidePanelCollapsed} />
        </button>
      )}
      {tabs.map((tab) => {
        const isActive = tab.id === activeTabId;
        const isClosable = tab.closable !== false;
        return (
          <div
            key={tab.id}
            onClick={() => handleActivate(tab.id)}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-xs cursor-pointer border-r border-surface-hover transition-colors min-w-0 max-w-[200px] group select-none ${
              isActive
                ? 'bg-background text-text-primary border-t-2 border-t-primary -mt-px'
                : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
            }`}
          >
            <span className="shrink-0 text-text-secondary/60">{ICON_MAP[tab.icon || ''] || ICON_MAP.pdf}</span>
            <span className="truncate flex-1">{tab.title}</span>
            {isClosable && (
              <button
                onClick={(e) => handleClose(e, tab.id)}
                className="shrink-0 p-0.5 rounded hover:bg-surface-hover text-text-secondary/40 hover:text-red-400 opacity-0 group-hover:opacity-100 transition-opacity"
              >
                <X size={12} />
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}
