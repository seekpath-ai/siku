import { useCallback, useState, useEffect, useRef } from 'react';
import { useRouterState, useNavigate } from '@tanstack/react-router';
import { Sidebar } from './Sidebar';
import { TitleBar } from './TitleBar';
import { SkeletonShell } from './SkeletonShell';
import { Dialog } from '@/components/ui/Dialog';
import { Pet } from '@/components/pet/Pet';
import { GlobalSearch } from '@/components/search/GlobalSearch';
import { ImportFromLinkDialog } from '@/components/library/ImportFromLinkDialog';
import { useImportPaper } from '@/hooks/useLibrary';
import { useShellStore } from '@/stores/shellStore';
import { useTabStore } from '@/stores/tabStore';
import { notesCreate, bookmarksCreate, settingsAppGet, settingsAppSave } from '@/lib/tauri';
import { openNoteTab } from '@/lib/openNote';
import { listen } from '@tauri-apps/api/event';

interface AppShellProps { children: React.ReactNode; }

function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

function useIsReaderRoute(): boolean {
  const { location } = useRouterState();
  return location.pathname.startsWith('/reader/');
}

export function AppShell({ children }: AppShellProps) {
  const { mutate: importPaper } = useImportPaper();
  const [toast, setToast] = useState<{ message: string; type: 'info' | 'warning' } | null>(null);
  const toastTimerRef = useRef<number | null>(null);
  const [importLinkOpen, setImportLinkOpen] = useState(false);
  const [isShuttingDown, setIsShuttingDown] = useState(false);
  const isReader = useIsReaderRoute();
  const { location } = useRouterState();
  const toggleSidePanel = useShellStore((state) => state.toggleSidePanel);
  const isMaximized = useShellStore((state) => state.isMaximized);
  const navigate = useNavigate();
  const { openRoute } = useTabStore();
  const zoomRef = useRef(1);
  const creatingNoteRef = useRef(false);

  // Show a toast that auto-dismisses after a few seconds.
  const showToast = useCallback((message: string, type: 'info' | 'warning' = 'info') => {
    setToast({ message, type });
    if (toastTimerRef.current) window.clearTimeout(toastTimerRef.current);
    toastTimerRef.current = window.setTimeout(() => setToast(null), 2500);
  }, []);

  // Skeleton screen — shows briefly on first mount to bridge the gap
  // between the splash fade-out and the real content rendering.
  // This prevents the "flash of unstyled layout" that occurs when the
  // router hasn't fully committed the first route yet.
  const [showSkeleton, setShowSkeleton] = useState(true);
  const skeletonDismissed = useRef(false);
  useEffect(() => {
    if (skeletonDismissed.current) return;
    // Show skeleton for at least 200ms, then fade to real content.
    // By this point the router has committed the first route and
    // TanStack Query has started loading initial data.
    const timer = setTimeout(() => {
      skeletonDismissed.current = true;
      setShowSkeleton(false);
    }, 200);
    return () => clearTimeout(timer);
  }, []);

  // Show a shutdown notice when the backend is finalizing background work.
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: (() => void) | undefined;
    listen('app:shutdown-started', () => setIsShuttingDown(true))
      .then((fn) => { unlisten = fn; })
      .catch(() => {});
    return () => { unlisten?.(); };
  }, []);

  // Listen for menu events
  useEffect(() => {
    const handleImport = () => selectAndImport();
    const handleImportFromLink = () => setImportLinkOpen(true);
    const handleBookmarks = () => {
      openRoute('/bookmarks', { title: '书签', icon: 'bookmark' });
    };
    const handleToggleTranslation = () => {
      if (isReader) {
        showToast('请在摘录面板中使用翻译功能');
      } else {
        showToast('翻译面板仅在阅读器页面可用');
      }
    };
    window.addEventListener('siku:import-pdf', handleImport);
    window.addEventListener('siku:import-from-link', handleImportFromLink);
    window.addEventListener('siku:bookmarks', handleBookmarks);
    window.addEventListener('siku:toggle-translation', handleToggleTranslation);
    return () => {
      window.removeEventListener('siku:import-pdf', handleImport);
      window.removeEventListener('siku:import-from-link', handleImportFromLink);
      window.removeEventListener('siku:bookmarks', handleBookmarks);
      window.removeEventListener('siku:toggle-translation', handleToggleTranslation);
    };
  }, [isReader]);

  // Sync the pinned home tab with the user-configured homepage.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    const load = async () => {
      try {
        const settings = await settingsAppGet();
        useTabStore.getState().setHomeTab(settings.homepage || '/library');
      } catch {
        // ignore
      }
    };
    load();
    const setup = async () => {
      unlisten = await listen('app:settings_changed', () => {
        load();
      });
    };
    setup();
    return () => unlisten?.();
  }, []);

  // Drag-and-drop PDF import
  useEffect(() => {
    if (!isTauri()) return;
    const handler = async (e: DragEvent) => {
      e.preventDefault();
      const file = e.dataTransfer?.files?.[0];
      if (file && (file.name.endsWith('.pdf') || file.type === 'application/pdf')) {
        const path = (file as File & { path?: string }).path;
        if (path) { setToast(null); importPaper(path); }
      }
    };
    const prevent = (e: DragEvent) => e.preventDefault();
    window.addEventListener('dragover', prevent);
    window.addEventListener('drop', handler);
    return () => { window.removeEventListener('dragover', prevent); window.removeEventListener('drop', handler); };
  }, [importPaper]);

  const selectAndImport = useCallback(async () => {
    if (!isTauri()) { showToast('仅桌面应用支持', 'warning'); return; }
    setToast(null);
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({ multiple: false, filters: [{ name: 'PDF', extensions: ['pdf'] }] });
      if (selected && typeof selected === 'string') importPaper(selected);
    } catch (err) { showToast(`打开文件对话框失败: ${err}`, 'warning'); }
  }, [importPaper, showToast]);

  const applyWebviewZoom = useCallback(async (next: number) => {
    zoomRef.current = Math.max(0.5, Math.min(2, next));
    if (isTauri()) {
      const { getCurrentWebview } = await import('@tauri-apps/api/webview');
      getCurrentWebview().setZoom(zoomRef.current).catch(() => {});
    }
  }, []);

  const createNote = useCallback(async () => {
    if (creatingNoteRef.current) return;
    creatingNoteRef.current = true;
    try {
      const note = await notesCreate('Untitled.md', '', undefined, undefined, false);
      openNoteTab(navigate, note);
      window.dispatchEvent(new CustomEvent('siku:note-created', { detail: note.id }));
    } catch (err) {
      showToast(`新建笔记失败: ${err}`, 'warning');
    } finally {
      // Keep the guard for a short moment to prevent double key events.
      setTimeout(() => { creatingNoteRef.current = false; }, 300);
    }
  }, [navigate, showToast]);

  // Open a route in a tab AND navigate to it. Keyboard shortcuts must do
  // both (like the menu bar), otherwise only the tab highlight changes
  // while the displayed page stays on the old route.
  const openRouteTab = useCallback((route: string, title: string, icon?: string) => {
    const tab = openRoute(route, { title, icon });
    if (tab.params) {
      navigate({ to: tab.route, params: tab.params });
    } else {
      navigate({ to: tab.route });
    }
  }, [navigate, openRoute]);

  // Toggle the desktop pet. Persists via settings so the pet window and the
  // settings checkbox stay in sync (the pet ball's own hide uses the same path).
  const togglePet = useCallback(async () => {
    try {
      const current = await settingsAppGet();
      const next = !(current.show_pet ?? true);
      await settingsAppSave({ ...current, show_pet: next });
      showToast(next ? '宠物已显示' : '宠物已隐藏');
    } catch (err) {
      console.error('Failed to toggle pet:', err);
    }
  }, [showToast]);

  // Keyboard shortcuts for the menu bar items.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;

      const target = e.target as HTMLElement;
      const isTyping =
        target.tagName === 'INPUT' ||
        target.tagName === 'TEXTAREA' ||
        target.isContentEditable;

      const key = e.key.toLowerCase();

      // Navigation and global actions should work even while typing.
      switch (key) {
        case 'i':
          e.preventDefault();
          selectAndImport();
          return;
        case 'n':
          e.preventDefault();
          createNote();
          return;
        case ',':
          e.preventDefault();
          openRouteTab('/settings', '设置', 'settings');
          return;
        case '1':
          e.preventDefault();
          openRouteTab('/library', '图书馆', 'home');
          return;
        case '2':
          e.preventDefault();
          openRouteTab('/chat', '对话', 'chat');
          return;
        case '3':
          e.preventDefault();
          openRouteTab('/notes', '笔记', 'note');
          return;
        case '4':
          e.preventDefault();
          openRouteTab('/knowledge', '知识库', 'knowledge');
          return;
        case '5':
          e.preventDefault();
          openRouteTab('/files', '文件', 'files');
          return;
        case 'r':
          e.preventDefault();
          openRouteTab('/research', '研究追踪', 'research');
          return;
        case 'p':
          // Ctrl+Shift+P toggles the desktop pet.
          if (e.shiftKey) {
            e.preventDefault();
            togglePet();
          }
          return;
        case 'd':
          e.preventDefault();
          {
            const tab = useTabStore.getState().tabs.find((t) => t.id === useTabStore.getState().activeTabId);
            const title = tab?.title || document.title || location.pathname;
            bookmarksCreate({ title, route: location.pathname, params_json: JSON.stringify(location.search) })
              .then(() => showToast('已收藏当前页'))
              .catch((err) => showToast(`收藏失败: ${err}`, 'warning'));
          }
          return;
        case 't':
          e.preventDefault();
          showToast(isReader ? '请在摘录面板中使用翻译功能' : '翻译面板仅在阅读器页面可用');
          return;
        case '=':
        case '+':
          e.preventDefault();
          applyWebviewZoom(zoomRef.current + 0.1);
          return;
        case '-':
        case '_':
          e.preventDefault();
          applyWebviewZoom(zoomRef.current - 0.1);
          return;
        case '0':
          e.preventDefault();
          applyWebviewZoom(1);
          return;
      }

      // Side panel toggle and editing commands should not hijack inputs.
      if (isTyping) return;

      if (key === 'b' && !isReader) {
        e.preventDefault();
        toggleSidePanel();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [toggleSidePanel, isReader, selectAndImport, createNote, navigate, openRoute, openRouteTab, togglePet, showToast, applyWebviewZoom]);

  return (
    <div
      className={`flex flex-col h-dvh bg-background overflow-hidden border border-surface-hover ${isMaximized ? '' : 'rounded-xl'}`}
      style={{ position: 'relative' }}
    >
      <TitleBar />

      {/* Toast feedback */}
      {toast && (
        <div
          className={`mx-4 mt-1 px-3 py-1.5 border rounded-lg text-xs shrink-0 ${
            toast.type === 'warning'
              ? 'bg-yellow-500/10 border-yellow-500/30 text-yellow-400'
              : 'bg-blue-500/10 border-blue-500/30 text-blue-400'
          }`}
        >
          {toast.message}
        </div>
      )}

      {/* Reader mode: full screen, no sidebar */}
      {isReader ? (
        <div className="flex-1 flex flex-col min-h-0">{children}</div>
      ) : (
        <div className="flex flex-1 min-h-0">
          <Sidebar />
          <div className="flex flex-1 flex-col min-w-0">
            <main className="flex-1 overflow-y-auto">{children}</main>
          </div>
        </div>
      )}

      {/* Skeleton overlay — bridges splash → real content transition */}
      {showSkeleton && (
        <div
          style={{
            position: 'absolute',
            inset: 0,
            top: 38, // below TitleBar
            zIndex: 100,
            transition: 'opacity 0.25s ease',
          }}
        >
          <SkeletonShell />
        </div>
      )}

      {/* Shutdown notice — shown while the backend finalizes DB/sync work. */}
      {isShuttingDown && (
        <div
          className="fixed inset-0 z-[9999] flex flex-col items-center justify-center bg-background/80 backdrop-blur-sm"
          data-tauri-drag-region
        >
          <div className="flex flex-col items-center gap-3 px-6 py-5 bg-surface border border-surface-hover rounded-2xl shadow-2xl">
            <div className="w-8 h-8 border-2 border-primary/30 border-t-primary rounded-full animate-spin" />
            <div className="text-sm font-medium text-text-primary">正在同步更改…</div>
            <div className="text-xs text-text-secondary/70">请稍候，应用正在安全关闭数据库</div>
          </div>
        </div>
      )}

      <Dialog />
      <ImportFromLinkDialog open={importLinkOpen} onClose={() => setImportLinkOpen(false)} />
      <Pet />
      <GlobalSearch onImportPdf={selectAndImport} />
    </div>
  );
}
