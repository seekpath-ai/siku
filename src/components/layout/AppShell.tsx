import { useCallback, useState, useEffect, useRef } from 'react';
import { useRouterState, useNavigate } from '@tanstack/react-router';
import { Sidebar } from './Sidebar';
import { TitleBar } from './TitleBar';
import { TabSwitcher } from './TabSwitcher';
import { SkeletonShell } from './SkeletonShell';
import { Dialog } from '@/components/ui/Dialog';
import { Pet } from '@/components/pet/Pet';
import { GlobalSearch } from '@/components/search/GlobalSearch';
import { ImportFromLinkDialog } from '@/components/library/ImportFromLinkDialog';
import { BatchImportDialog } from '@/components/library/BatchImportDialog';
import { ZoteroImportDialog } from '@/components/library/ZoteroImportDialog';
import { AiTableDialog } from '@/components/library/AiTableDialog';
import { ReviewGenDialog } from '@/components/library/ReviewGenDialog';
import { AiClassifyDialog } from '@/components/library/AiClassifyDialog';
import { AiClassifyRulesDialog } from '@/components/library/AiClassifyRulesDialog';
import { runFileBatch, useBatchImportStore } from '@/stores/batchImportStore';
import { useImportPaper } from '@/hooks/useLibrary';
import { useShellStore } from '@/stores/shellStore';
import { useTabStore } from '@/stores/tabStore';
import { useResearchStore } from '@/stores/researchStore';
import { notesCreate, bookmarksCreate, settingsAppGet, settingsAppSave, screenshotHotkeySync, screenshotStart } from '@/lib/tauri';
import { openNoteTab } from '@/lib/openNote';
import { listen } from '@/lib/events';
import { notify } from '@/lib/notify';
import { ToastHost } from '@/components/ui/ToastHost';
import { installShortcutListener, registerShortcut, listShortcuts } from '@/lib/shortcuts';
import { Modal } from '@/components/ui/Modal';

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
  const [importLinkOpen, setImportLinkOpen] = useState(false);
  const [importZoteroOpen, setImportZoteroOpen] = useState(false);
  const [isShuttingDown, setIsShuttingDown] = useState(false);
  const isReader = useIsReaderRoute();
  const { location } = useRouterState();
  const toggleSidePanel = useShellStore((state) => state.toggleSidePanel);
  const isMaximized = useShellStore((state) => state.isMaximized);
  const navigate = useNavigate();
  const { openRoute } = useTabStore();
  const zoomRef = useRef(1);
  const creatingNoteRef = useRef(false);


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
    const handleImportFolder = () => selectFolderAndImport();
    const handleImportZotero = () => setImportZoteroOpen(true);
    const handleImportFromLink = () => setImportLinkOpen(true);
    const handleBookmarks = () => {
      openRoute('/bookmarks', { title: '书签', icon: 'bookmark' });
    };
    const handleToggleTranslation = () => {
      if (isReader) {
        notify('请在摘录面板中使用翻译功能');
      } else {
        notify('翻译面板仅在阅读器页面可用');
      }
    };
    window.addEventListener('siku:import-pdf', handleImport);
    window.addEventListener('siku:import-folder', handleImportFolder);
    window.addEventListener('siku:import-zotero', handleImportZotero);
    window.addEventListener('siku:import-from-link', handleImportFromLink);
    window.addEventListener('siku:bookmarks', handleBookmarks);
    window.addEventListener('siku:toggle-translation', handleToggleTranslation);
    return () => {
      window.removeEventListener('siku:import-pdf', handleImport);
      window.removeEventListener('siku:import-folder', handleImportFolder);
      window.removeEventListener('siku:import-zotero', handleImportZotero);
      window.removeEventListener('siku:import-from-link', handleImportFromLink);
      window.removeEventListener('siku:bookmarks', handleBookmarks);
      window.removeEventListener('siku:toggle-translation', handleToggleTranslation);
    };
  }, [isReader]);

  // Global screenshot hotkey: apply the setting at startup, and route the
  // backend's trigger event. Chat page: forward to the message input, which
  // launches the snipping tool and auto-attaches the result. Everywhere else
  // (notes, reader, …): just launch the tool — the shot lands in the system
  // clipboard and stays there for manual pasting (Ctrl+V / right-click in the
  // note editor) or other apps; nothing auto-attaches.
  const pathnameRef = useRef(location.pathname);
  pathnameRef.current = location.pathname;
  useEffect(() => {
    screenshotHotkeySync().catch((err) => console.warn('global screenshot hotkey:', err));
    let unlisten: (() => void) | undefined;
    listen('siku:global-screenshot', () => {
      if (pathnameRef.current === '/chat') {
        window.dispatchEvent(new CustomEvent('siku:screenshot-hotkey'));
      } else {
        screenshotStart().catch((err) => console.warn('screenshot start:', err));
      }
    }).then((u) => {
      unlisten = u;
    });
    return () => unlisten?.();
  }, []);

  // Research auto-discovery: toast + bump the topic's unread badge when a
  // background scan finds new sources.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<{ topic_id: string; topic_name: string; added: number }>(
      'research:auto_discovered',
      (e) => {
        notify(`科研追踪：课题「${e.payload.topic_name}」新增 ${e.payload.added} 篇文献`);
        useResearchStore.getState().bumpUnread(e.payload.topic_id, e.payload.added);
      }
    ).then((u) => {
      unlisten = u;
    });
    return () => unlisten?.();
  }, []);

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

  // Drag-and-drop PDF import (single file → direct import; several → batch)
  useEffect(() => {
    if (!isTauri()) return;
    const handler = async (e: DragEvent) => {
      e.preventDefault();
      const files = Array.from(e.dataTransfer?.files ?? []).filter(
        (f) => f.name.toLowerCase().endsWith('.pdf') || f.type === 'application/pdf'
      );
      const paths = files
        .map((f) => (f as File & { path?: string }).path)
        .filter((p): p is string => !!p);
      if (paths.length === 0) return;
      if (paths.length === 1) importPaper(paths[0]);
      else runFileBatch(paths).catch(() => {});
    };
    const prevent = (e: DragEvent) => e.preventDefault();
    window.addEventListener('dragover', prevent);
    window.addEventListener('drop', handler);
    return () => { window.removeEventListener('dragover', prevent); window.removeEventListener('drop', handler); };
  }, [importPaper]);

  const selectAndImport = useCallback(async () => {
    if (!isTauri()) { notify('仅桌面应用支持', 'warning'); return; }
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({ multiple: true, filters: [{ name: 'PDF', extensions: ['pdf'] }] });
      const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
      if (paths.length === 1) importPaper(paths[0]);
      else if (paths.length > 1) runFileBatch(paths).catch(() => {});
    } catch (err) { notify(`打开文件对话框失败: ${err}`, 'warning'); }
  }, [importPaper]);

  // Folder batch import: pick a directory, scan it for PDFs, import them all.
  const selectFolderAndImport = useCallback(async () => {
    if (!isTauri()) { notify('仅桌面应用支持', 'warning'); return; }
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({ directory: true, title: '选择要导入的文件夹（含子目录）' });
      if (typeof selected !== 'string') return;
      const found = await useBatchImportStore.getState().runFolder(selected);
      if (found === 0) notify('该文件夹（含子目录）中没有 PDF 文件', 'warning');
    } catch (err) { notify(`打开文件夹失败: ${err}`, 'warning'); }
  }, []);

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
      const note = await notesCreate('新笔记', '', undefined, undefined, false);
      openNoteTab(navigate, note);
      window.dispatchEvent(new CustomEvent('siku:note-created', { detail: note.id }));
    } catch (err) {
      notify(`新建笔记失败: ${err}`, 'warning');
    } finally {
      // Keep the guard for a short moment to prevent double key events.
      setTimeout(() => { creatingNoteRef.current = false; }, 300);
    }
  }, [navigate]);

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
      notify(next ? '宠物已显示' : '宠物已隐藏');
    } catch (err) {
      console.error('Failed to toggle pet:', err);
    }
  }, []);

  // 全局快捷键：集中注册到 shortcuts.ts 注册表（帮助面板据此生成）。
  const [shortcutsHelpOpen, setShortcutsHelpOpen] = useState(false);
  useEffect(() => {
    installShortcutListener();
    const bookmark = () => {
      const tab = useTabStore.getState().tabs.find((t) => t.id === useTabStore.getState().activeTabId);
      const title = tab?.title || document.title || location.pathname;
      bookmarksCreate({ title, route: location.pathname, params_json: JSON.stringify(location.search) })
        .then(() => notify('已收藏当前页'))
        .catch((err) => notify(`收藏失败: ${err}`, 'warning'));
    };
    const defs: Parameters<typeof registerShortcut>[0][] = [
      { id: 'app.import', keys: 'Ctrl+I', description: '导入 PDF 文件', allowInInput: true, handler: () => selectAndImport() },
      { id: 'app.new-note', keys: 'Ctrl+N', description: '新建笔记', allowInInput: true, handler: () => createNote() },
      { id: 'app.new-agent', keys: 'Ctrl+Shift+N', description: '新建智能体对话', allowInInput: true, handler: () => {
        openRouteTab('/chat', '对话', 'chat');
        window.dispatchEvent(new CustomEvent('siku:new-agent'));
      } },
      { id: 'nav.settings', keys: 'Ctrl+,', description: '打开设置', allowInInput: true, handler: () => openRouteTab('/settings', '设置', 'settings') },
      { id: 'nav.library', keys: 'Ctrl+1', description: '图书馆', allowInInput: true, handler: () => openRouteTab('/library', '图书馆', 'home') },
      { id: 'nav.chat', keys: 'Ctrl+2', description: '对话', allowInInput: true, handler: () => openRouteTab('/chat', '对话', 'chat') },
      { id: 'nav.notes', keys: 'Ctrl+3', description: '笔记', allowInInput: true, handler: () => openRouteTab('/notes', '笔记', 'note') },
      { id: 'nav.knowledge', keys: 'Ctrl+4', description: '知识库', allowInInput: true, handler: () => openRouteTab('/knowledge', '知识库', 'knowledge') },
      { id: 'nav.files', keys: 'Ctrl+5', description: '文件', allowInInput: true, handler: () => openRouteTab('/files', '文件', 'files') },
      { id: 'nav.research', keys: 'Ctrl+R', description: '研究追踪', allowInInput: true, handler: () => openRouteTab('/research', '研究追踪', 'research') },
      { id: 'app.toggle-pet', keys: 'Ctrl+Shift+P', description: '显示/隐藏桌宠', allowInInput: true, handler: () => togglePet() },
      { id: 'app.bookmark', keys: 'Ctrl+D', description: '收藏当前页', allowInInput: true, handler: bookmark },
      { id: 'app.translate-hint', keys: 'Ctrl+T', description: '翻译（阅读器摘录面板）', allowInInput: true, handler: () =>
        notify(isReader ? '请在摘录面板中使用翻译功能' : '翻译面板仅在阅读器页面可用') },
      { id: 'app.zoom-in', keys: 'Ctrl+=', description: '放大界面', allowInInput: true, handler: () => applyWebviewZoom(zoomRef.current + 0.1) },
      { id: 'app.zoom-in-shift', keys: 'Ctrl+Shift+=', description: '放大界面', allowInInput: true, handler: () => applyWebviewZoom(zoomRef.current + 0.1) },
      { id: 'app.zoom-out', keys: 'Ctrl+-', description: '缩小界面', allowInInput: true, handler: () => applyWebviewZoom(zoomRef.current - 0.1) },
      { id: 'app.zoom-out-shift', keys: 'Ctrl+Shift+-', description: '缩小界面', allowInInput: true, handler: () => applyWebviewZoom(zoomRef.current - 0.1) },
      { id: 'app.zoom-reset', keys: 'Ctrl+0', description: '重置缩放', allowInInput: true, handler: () => applyWebviewZoom(1) },
      { id: 'app.toggle-sidebar', keys: 'Ctrl+B', description: '折叠/展开侧边栏', handler: () => { if (!isReader) toggleSidePanel(); } },
      { id: 'app.shortcuts-help', keys: 'Shift+?', description: '快捷键帮助（本面板）', handler: () => setShortcutsHelpOpen((v) => !v) },
    ];
    const unregisters = defs.map(registerShortcut);
    return () => unregisters.forEach((u) => u());
  }, [toggleSidePanel, isReader, selectAndImport, createNote, navigate, openRoute, openRouteTab, togglePet, applyWebviewZoom]);

  return (
    <div
      className={`flex flex-col h-dvh bg-background overflow-hidden border border-surface-hover ${isMaximized ? '' : 'rounded-xl'}`}
      style={{ position: 'relative' }}
    >
      <TitleBar />
      <TabSwitcher />

      <ToastHost />

      {/* 快捷键帮助（Shift+? 唤起） */}
      {shortcutsHelpOpen && (
        <Modal
          onClose={() => setShortcutsHelpOpen(false)}
          backdropClassName="bg-black/50 backdrop-blur-sm"
          className="w-[420px] max-w-[92vw] max-h-[70vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden"
        >
          <div className="flex items-center justify-between px-4 py-3 border-b border-surface-hover shrink-0">
            <span className="text-sm font-medium text-text-primary">快捷键</span>
            <button
              onClick={() => setShortcutsHelpOpen(false)}
              className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
              aria-label="关闭快捷键帮助"
            >
              ✕
            </button>
          </div>
          <div className="flex-1 overflow-y-auto px-4 py-2">
            {listShortcuts()
              .filter((s) => s.id !== 'app.shortcuts-help')
              .map((s) => (
                <div key={s.id} className="flex items-center justify-between py-1.5 text-[12px]">
                  <span className="text-text-secondary">{s.description}</span>
                  <kbd className="shrink-0 px-1.5 py-0.5 rounded bg-background border border-surface-hover text-[11px] text-text-primary font-mono">
                    {s.keys}
                  </kbd>
                </div>
              ))}
          </div>
        </Modal>
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
      <ZoteroImportDialog open={importZoteroOpen} onClose={() => setImportZoteroOpen(false)} />
      <BatchImportDialog />
      {/* Library AI dialogs are global: their backend runs outlive the
          library route, so they must not unmount with it. */}
      <AiTableDialog />
      <ReviewGenDialog />
      <AiClassifyDialog />
      <AiClassifyRulesDialog />
      <Pet />
      <GlobalSearch onImportPdf={selectAndImport} />
    </div>
  );
}
