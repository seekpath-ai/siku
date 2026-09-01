import { create } from 'zustand';

export interface Tab {
  id: string;
  title: string;
  icon?: string;
  route: string;
  params?: Record<string, string>;
  /** Route search params (e.g. { note: '<id>' } for note tabs). */
  search?: Record<string, unknown>;
  closable?: boolean; // false = persistent tab like home
}

interface TabState {
  tabs: Tab[];
  activeTabId: string | null;
  open: (tab: Tab) => void;
  openRoute: (route: string, config: Omit<Tab, 'id' | 'route'>) => Tab;
  close: (id: string) => void;
  closeAll: () => void;
  closeOthers: (keepId: string) => void;
  closeToRight: (id: string) => void;
  closeToLeft: (id: string) => void;
  activate: (id: string) => void;
  findById: (id: string) => Tab | undefined;
  ensureHome: () => void;
  setHomeTab: (route: string) => void;
  updateTab: (id: string, patch: Partial<Pick<Tab, 'title' | 'icon'>>) => void;
}

const HOME_TAB_ID = 'home';
const DEFAULT_HOME_ROUTE = '/library';

const HOME_ROUTE_CONFIG: Record<string, Pick<Tab, 'title' | 'icon'>> = {
  '/library': { title: '图书馆', icon: 'home' },
  '/chat': { title: '对话', icon: 'chat' },
  '/notes': { title: '笔记', icon: 'note' },
  '/knowledge': { title: '知识库', icon: 'knowledge' },
  '/research': { title: '科研追踪', icon: 'research' },
  '/graph': { title: '知识图谱', icon: 'graph' },
  '/bookmarks': { title: '书签', icon: 'bookmark' },
  '/timeline': { title: '时间轴', icon: 'clock' },
  '/files': { title: '文件列表', icon: 'files' },
  '/settings': { title: '设置', icon: 'settings' },
};

function createHomeTab(route: string = DEFAULT_HOME_ROUTE): Tab {
  const config = HOME_ROUTE_CONFIG[route] ?? HOME_ROUTE_CONFIG[DEFAULT_HOME_ROUTE];
  return {
    id: HOME_TAB_ID,
    title: config.title,
    icon: config.icon,
    route,
    closable: false,
  };
}

// Prevent re-opening a tab immediately after it was closed. The window is
// deliberately short: it only exists to swallow the automatic re-activation
// that closing the active tab can trigger, but a longer window also eats
// legitimate re-opens (e.g. closing a note tab then clicking the same note in
// the list right away).
const recentlyClosed = new Set<string>();
const BLOCK_MS = 150;

export const useTabStore = create<TabState>((set, get) => ({
  tabs: [createHomeTab()],
  activeTabId: HOME_TAB_ID,

  open: (tab) => {
    if (recentlyClosed.has(tab.id)) return; // blocked: was just closed
    const { tabs } = get();
    const existing = tabs.find((t) => t.id === tab.id);
    if (existing) {
      set({ activeTabId: tab.id });
      return;
    }
    set({ tabs: [...tabs, { ...tab, closable: tab.closable ?? true }], activeTabId: tab.id });
  },

  openRoute: (route, config) => {
    const { tabs } = get();
    // Only dedupe against plain route tabs — parameterized tabs (note tabs
    // with ?note=, reader tabs with paperId) are distinct documents, not
    // instances of the route's home page.
    const existing = tabs.find((t) => t.route === route && !t.params && !t.search);
    if (existing) {
      set({ activeTabId: existing.id });
      return existing;
    }
    const id = `${route.replace(/\//g, '_')}_${Date.now()}`;
    const tab: Tab = { id, route, ...config, closable: config.closable ?? true };
    set({ tabs: [...tabs, tab], activeTabId: id });
    return tab;
  },

  close: (id) => {
    const { tabs, activeTabId } = get();
    const tab = tabs.find((t) => t.id === id);
    if (tab && tab.closable === false) return; // can't close home
    recentlyClosed.add(id);
    setTimeout(() => recentlyClosed.delete(id), BLOCK_MS);
    const remaining = tabs.filter((t) => t.id !== id);
    let nextActive = activeTabId;
    if (activeTabId === id) {
      const idx = tabs.findIndex((t) => t.id === id);
      if (remaining.length === 0) {
        nextActive = null;
      } else {
        const next = remaining[Math.min(idx, remaining.length - 1)];
        nextActive = next?.id ?? null;
      }
    }
    set({ tabs: remaining, activeTabId: nextActive });
  },

  activate: (id) => set({ activeTabId: id }),

  closeAll: () => {
    const { tabs } = get();
    const remaining = tabs.filter((t) => t.closable === false);
    if (remaining.length === tabs.length) return;
    tabs.forEach((t) => {
      if (t.closable !== false) recentlyClosed.add(t.id);
    });
    setTimeout(() => {
      tabs.forEach((t) => {
        if (t.closable !== false) recentlyClosed.delete(t.id);
      });
    }, BLOCK_MS);
    const nextActive = remaining[0]?.id ?? null;
    set({ tabs: remaining, activeTabId: nextActive });
  },

  closeOthers: (keepId) => {
    const { tabs } = get();
    const remaining = tabs.filter((t) => t.id === keepId || t.closable === false);
    if (remaining.length === tabs.length) return;
    tabs.forEach((t) => {
      if (t.id !== keepId && t.closable !== false) recentlyClosed.add(t.id);
    });
    setTimeout(() => {
      tabs.forEach((t) => {
        if (t.id !== keepId && t.closable !== false) recentlyClosed.delete(t.id);
      });
    }, BLOCK_MS);
    set({ tabs: remaining, activeTabId: keepId });
  },

  closeToRight: (id) => {
    const { tabs, activeTabId } = get();
    const idx = tabs.findIndex((t) => t.id === id);
    if (idx === -1) return;
    const toClose = tabs.slice(idx + 1).filter((t) => t.closable !== false);
    if (toClose.length === 0) return;
    toClose.forEach((t) => recentlyClosed.add(t.id));
    setTimeout(() => toClose.forEach((t) => recentlyClosed.delete(t.id)), BLOCK_MS);
    const remaining = tabs.filter((t) => !toClose.includes(t));
    const nextActive = remaining.find((t) => t.id === activeTabId) ? activeTabId : id;
    set({ tabs: remaining, activeTabId: nextActive });
  },

  closeToLeft: (id) => {
    const { tabs, activeTabId } = get();
    const idx = tabs.findIndex((t) => t.id === id);
    if (idx === -1) return;
    const toClose = tabs.slice(0, idx).filter((t) => t.closable !== false);
    if (toClose.length === 0) return;
    toClose.forEach((t) => recentlyClosed.add(t.id));
    setTimeout(() => toClose.forEach((t) => recentlyClosed.delete(t.id)), BLOCK_MS);
    const remaining = tabs.filter((t) => !toClose.includes(t));
    const nextActive = remaining.find((t) => t.id === activeTabId) ? activeTabId : id;
    set({ tabs: remaining, activeTabId: nextActive });
  },

  findById: (id) => get().tabs.find((t) => t.id === id),

  ensureHome: () => {
    const { tabs } = get();
    if (!tabs.find((t) => t.id === HOME_TAB_ID)) {
      set({ tabs: [createHomeTab(), ...tabs] });
    }
  },

  setHomeTab: (route: string) => {
    const { tabs, activeTabId } = get();
    const config = HOME_ROUTE_CONFIG[route] ?? HOME_ROUTE_CONFIG[DEFAULT_HOME_ROUTE];
    const nextTabs = tabs.map((t) =>
      t.id === HOME_TAB_ID
        ? { ...t, route, title: config.title, icon: config.icon }
        : t
    );
    set({ tabs: nextTabs, activeTabId: activeTabId ?? HOME_TAB_ID });
  },

  updateTab: (id, patch) => {
    const { tabs } = get();
    const exists = tabs.find((t) => t.id === id);
    if (!exists) return; // don't re-create a closed tab
    set({ tabs: tabs.map((t) => (t.id === id ? { ...t, ...patch } : t)) });
  },
}));
