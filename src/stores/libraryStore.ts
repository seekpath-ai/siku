import { create } from 'zustand';
import { persist } from 'zustand/middleware';

type SortField = 'title' | 'year' | 'imported_at';
type SortOrder = 'asc' | 'desc';
type ViewMode = 'table' | 'card';

export type ActiveFilter =
  | { type: 'all'; tagIds: string[]; tagLogic: 'and' | 'or' }
  | { type: 'collection'; id: string; tagIds: string[]; tagLogic: 'and' | 'or' }
  | { type: 'trash'; tagIds: string[]; tagLogic: 'and' | 'or' }
  | { type: 'recent'; tagIds: string[]; tagLogic: 'and' | 'or' };

interface LibraryState {
  // Navigation/filter
  activeFilter: ActiveFilter;
  searchQuery: string;
  // Advanced filters (year range, journal)
  yearFrom: string;
  yearTo: string;
  journalFilter: string;
  // Quick status filter: 'all' | 'favorites' | 'unread'
  statusFilter: 'all' | 'favorites' | 'unread';

  // Sorting
  sortBy: SortField;
  sortOrder: SortOrder;

  // Selection
  selectedPaperIds: string[];
  lastSelectedId: string | null;

  // View
  viewMode: ViewMode;

  /** Paper-list columns hidden via the header context menu (title exempt). */
  hiddenColumns: string[];

  // Panel widths (px)
  leftPanelWidth: number;
  rightPanelWidth: number;
  rightPanelCollapsed: boolean;

  // Tree state
  expandedCollectionIds: string[];

  // Dialogs
  importDialogOpen: boolean;

  // Actions
  setActiveFilter: (filter: ActiveFilter) => void;
  setActiveCollection: (id: string | null) => void;
  openTrash: () => void;
  openRecentReads: () => void;
  toggleActiveTag: (id: string) => void;
  setTagFilterLogic: (logic: 'and' | 'or') => void;
  setSearchQuery: (query: string) => void;
  setYearFrom: (v: string) => void;
  setYearTo: (v: string) => void;
  setJournalFilter: (v: string) => void;
  setStatusFilter: (s: 'all' | 'favorites' | 'unread') => void;
  clearAdvancedFilters: () => void;
  /** Apply a saved search: its params set search + advanced filters. */
  applySavedSearch: (paramsJson: string) => void;
  setSortBy: (field: SortField) => void;
  setSortOrder: (order: SortOrder) => void;
  toggleSort: (field: SortField) => void;
  setViewMode: (mode: ViewMode) => void;
  toggleHiddenColumn: (key: string) => void;
  selectPaper: (id: string, multi?: boolean, range?: boolean) => void;
  clearSelection: () => void;
  setLeftPanelWidth: (width: number) => void;
  setRightPanelWidth: (width: number) => void;
  toggleRightPanelCollapsed: () => void;
  setRightPanelCollapsed: (collapsed: boolean) => void;
  toggleExpandedCollection: (id: string) => void;
  setExpandedCollectionIds: (ids: string[]) => void;
  setImportDialogOpen: (open: boolean) => void;
}

const MIN_PANEL_WIDTH = 160;
const MAX_PANEL_WIDTH = 600;

function clampPanelWidth(width: number) {
  return Math.max(MIN_PANEL_WIDTH, Math.min(MAX_PANEL_WIDTH, width));
}

export const useLibraryStore = create<LibraryState>()(
  persist(
    (set, get) => ({
      activeFilter: { type: 'all', tagIds: [], tagLogic: 'or' },
      searchQuery: '',
      yearFrom: '',
      yearTo: '',
      journalFilter: '',
      statusFilter: 'all',
      sortBy: 'imported_at',
      sortOrder: 'desc',
      selectedPaperIds: [],
      lastSelectedId: null,
      viewMode: 'table',
      hiddenColumns: [],
      leftPanelWidth: 256,
      rightPanelWidth: 320,
      rightPanelCollapsed: false,
      expandedCollectionIds: [],
      importDialogOpen: false,

      setActiveFilter: (filter) =>
        set({ activeFilter: filter, selectedPaperIds: [], lastSelectedId: null }),
      // Switch the collection scope while keeping any active tag filter.
      setActiveCollection: (id) =>
        set((state) => {
          const { tagIds, tagLogic } = state.activeFilter;
          return {
            activeFilter: id
              ? { type: 'collection', id, tagIds, tagLogic }
              : { type: 'all', tagIds, tagLogic },
            selectedPaperIds: [],
            lastSelectedId: null,
          };
        }),
      // Trash view: list only soft-deleted papers.
      openTrash: () =>
        set({
          activeFilter: { type: 'trash', tagIds: [], tagLogic: 'or' },
          selectedPaperIds: [],
          lastSelectedId: null,
        }),
      // Recently-read view: papers opened in the reader, sorted by last_read_at.
      openRecentReads: () =>
        set({
          activeFilter: { type: 'recent', tagIds: [], tagLogic: 'or' },
          selectedPaperIds: [],
          lastSelectedId: null,
        }),
      // Toggle a tag in the filter while keeping the current collection scope.
      toggleActiveTag: (id) =>
        set((state) => {
          const { activeFilter } = state;
          const tagIds = activeFilter.tagIds.includes(id)
            ? activeFilter.tagIds.filter((x) => x !== id)
            : [...activeFilter.tagIds, id];
          return {
            activeFilter: { ...activeFilter, tagIds },
            selectedPaperIds: [],
            lastSelectedId: null,
          };
        }),
      setTagFilterLogic: (logic) =>
        set((state) => ({ activeFilter: { ...state.activeFilter, tagLogic: logic } })),
      setSearchQuery: (query) => set({ searchQuery: query }),
      setYearFrom: (v) => set({ yearFrom: v }),
      setYearTo: (v) => set({ yearTo: v }),
      setJournalFilter: (v) => set({ journalFilter: v }),
      setStatusFilter: (s) => set({ statusFilter: s }),
      clearAdvancedFilters: () =>
        set({ yearFrom: '', yearTo: '', journalFilter: '', statusFilter: 'all' }),
      applySavedSearch: (paramsJson) => {
        try {
          const p = JSON.parse(paramsJson) as {
            search?: string;
            year_from?: number;
            year_to?: number;
            journal?: string;
            read_status?: string;
            is_favorite?: boolean;
          };
          set({
            searchQuery: p.search ?? '',
            yearFrom: p.year_from != null ? String(p.year_from) : '',
            yearTo: p.year_to != null ? String(p.year_to) : '',
            journalFilter: p.journal ?? '',
            statusFilter:
              p.is_favorite ? 'favorites' : p.read_status === 'unread' ? 'unread' : 'all',
            activeFilter: { type: 'all', tagIds: [], tagLogic: 'or' },
            selectedPaperIds: [],
            lastSelectedId: null,
          });
        } catch {
          // Ignore malformed saved params.
        }
      },
      setSortBy: (field) => set({ sortBy: field }),
      setSortOrder: (order) => set({ sortOrder: order }),
      toggleSort: (field) => {
        const { sortBy, sortOrder } = get();
        if (sortBy === field) {
          set({ sortOrder: sortOrder === 'asc' ? 'desc' : 'asc' });
        } else {
          set({ sortBy: field, sortOrder: 'desc' });
        }
      },
      setViewMode: (mode) => set({ viewMode: mode }),
      toggleHiddenColumn: (key) =>
        set((state) => ({
          hiddenColumns: state.hiddenColumns.includes(key)
            ? state.hiddenColumns.filter((k) => k !== key)
            : [...state.hiddenColumns, key],
        })),
      selectPaper: (id, multi, range) => {
        const { selectedPaperIds, lastSelectedId } = get();
        if (multi) {
          const next = selectedPaperIds.includes(id)
            ? selectedPaperIds.filter((x) => x !== id)
            : [...selectedPaperIds, id];
          set({ selectedPaperIds: next, lastSelectedId: id });
        } else if (range && lastSelectedId) {
          // Range selection requires ordered list; handled in PaperList via
          // direct setSelectedPaperIds calls. Here we just fallback to single.
          set({ selectedPaperIds: [id], lastSelectedId: id });
        } else {
          set({ selectedPaperIds: [id], lastSelectedId: id });
        }
      },
      clearSelection: () => set({ selectedPaperIds: [], lastSelectedId: null }),
      setLeftPanelWidth: (width) => set({ leftPanelWidth: clampPanelWidth(width) }),
      setRightPanelWidth: (width) => set({ rightPanelWidth: clampPanelWidth(width) }),
      toggleRightPanelCollapsed: () =>
        set((state) => ({ rightPanelCollapsed: !state.rightPanelCollapsed })),
      setRightPanelCollapsed: (collapsed) => set({ rightPanelCollapsed: collapsed }),
      toggleExpandedCollection: (id) =>
        set((state) => ({
          expandedCollectionIds: state.expandedCollectionIds.includes(id)
            ? state.expandedCollectionIds.filter((x) => x !== id)
            : [...state.expandedCollectionIds, id],
        })),
      setExpandedCollectionIds: (ids) => set({ expandedCollectionIds: ids }),
      setImportDialogOpen: (open) => set({ importDialogOpen: open }),
    }),
    {
      name: 'siku-library-ui',
      partialize: (state) => ({
        leftPanelWidth: state.leftPanelWidth,
        rightPanelWidth: state.rightPanelWidth,
        rightPanelCollapsed: state.rightPanelCollapsed,
        viewMode: state.viewMode,
        hiddenColumns: state.hiddenColumns,
        sortBy: state.sortBy,
        sortOrder: state.sortOrder,
        expandedCollectionIds: state.expandedCollectionIds,
      }),
    }
  )
);
