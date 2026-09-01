import { useState, useCallback, useMemo, useRef, useEffect } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import {
  Search,
  Plus,
  Link2,
  LayoutGrid,
  List,
  FileText,
  Calendar,
  User,
  BookOpen,
  File,
  ExternalLink,
  Trash2,
  RotateCcw,
  FileDown,
  Copy,
  Star,
  Filter,
  BookmarkPlus,
  BookMarked,
  Download,
  ChevronDown,
  ChevronUp,
  ChevronRight,
  Loader2,
  FileWarning,
  Inbox,
  Paperclip,
  StickyNote,
  FolderOpen,
  FolderPlus,
  FolderMinus,
  RefreshCw,
  Check,
} from 'lucide-react';
import {
  usePapers,
  useDeletePaper,
  useRestorePaper,
  usePurgePaper,
  usePaperSetFavorite,
  usePaperSetReadStatus,
  usePaperNotes,
  useCollections,
  useAddPapersToCollection,
  useRemovePapersFromCollection,
} from '@/hooks/useLibrary';
import { useLibraryStore } from '@/stores/libraryStore';
import { useTabStore } from '@/stores/tabStore';
import { openNoteTab } from '@/lib/openNote';
import { useDialog } from '@/hooks/useDialog';
import { parseJsonArray } from '@/lib/types';
import { isoToDisplay } from '@/lib/time';
import type { ActiveFilter } from '@/stores/libraryStore';
import {
  openPaperInSystem,
  revealPaperInSystem,
  paperExport,
  savedSearchesCreate,
  notesList,
  paperImportBibtex,
  paperReprocessIndex,
  paperFindDuplicates,
  paperMerge,
} from '@/lib/tauri';
import { ContextMenu, type ContextMenuItem } from '@/components/ui/ContextMenu';
import { PaperCard } from './PaperCard';
import type { Paper, ListPapersParams, Note, Collection } from '@/lib/types';

type SortField = 'title' | 'year' | 'imported_at';

/** Toggleable paper-list columns (the title column is always visible).
 *  Order matches the row layout. */
type ColumnKey = 'authors' | 'year' | 'journal' | 'pages' | 'date';

const COLUMN_DEFS: { key: ColumnKey; label: string }[] = [
  { key: 'authors', label: '作者' },
  { key: 'year', label: '年份' },
  { key: 'journal', label: '期刊' },
  { key: 'pages', label: '页数' },
  { key: 'date', label: '日期' },
];

/** Container-width thresholds: below `below` px the listed columns
 * auto-hide (least valuable first), so the title column is squeezed last.
 * Applied cumulatively — the first matching (narrowest) band wins. */
const AUTO_HIDE_BANDS: { below: number; keys: ColumnKey[] }[] = [
  { below: 520, keys: ['pages', 'journal', 'date', 'authors'] },
  { below: 680, keys: ['pages', 'journal', 'date'] },
  { below: 800, keys: ['pages', 'journal'] },
  { below: 900, keys: ['pages'] },
];

function autoHiddenColumns(width: number): Set<ColumnKey> {
  if (width <= 0) return new Set(); // not measured yet — show everything
  for (const band of AUTO_HIDE_BANDS) {
    if (width < band.below) return new Set(band.keys);
  }
  return new Set();
}

function SortIcon({ field, current, order }: { field: SortField; current: SortField; order: 'asc' | 'desc' }) {
  if (field !== current) return <span className="w-3.5" />;
  return order === 'asc' ? <ChevronUp size={14} /> : <ChevronDown size={14} />;
}

/**
 * Flatten collections into a pre-order tree for the "add to collection"
 * picker: parents before children, each node carrying its depth and parent.
 * `skipId` excludes one node (e.g. the collection currently being viewed)
 * while keeping its subtree.
 */
function buildCollectionTree(
  collections: Collection[],
  skipId: string | null
): { label: string; value: string; indent: number; parent: string | null; expandable: boolean }[] {
  const ids = new Set(collections.map((c) => c.id));
  const byParent = new Map<string, Collection[]>();
  for (const c of collections) {
    const key = c.parent_id && ids.has(c.parent_id) ? c.parent_id : '';
    if (!byParent.has(key)) byParent.set(key, []);
    byParent.get(key)!.push(c);
  }
  const sorted = (arr: Collection[]) => [...arr].sort((a, b) => a.sort_order - b.sort_order);

  const options: { label: string; value: string; indent: number; parent: string | null; expandable: boolean }[] = [];
  const walk = (parentKey: string, depth: number) => {
    for (const c of sorted(byParent.get(parentKey) ?? [])) {
      if (c.id === skipId) {
        // Skip the node itself but keep its subtree at the same depth.
        walk(c.id, depth);
        continue;
      }
      const children = byParent.get(c.id) ?? [];
      options.push({
        label: c.name,
        value: c.id,
        indent: depth,
        parent: parentKey === '' ? null : parentKey,
        expandable: children.length > 0,
      });
      walk(c.id, depth + 1);
    }
  };
  walk('', 0);
  return options;
}

function ChildRow({
  icon,
  label,
  sub,
  onClick,
  onDoubleClick,
}: {
  icon: React.ReactNode;
  label: string;
  sub?: string;
  onClick?: () => void;
  onDoubleClick?: () => void;
}) {
  return (
    <div
      onClick={onClick}
      onDoubleClick={onDoubleClick}
      className="flex items-center gap-2 px-3 py-1.5 pl-10 cursor-pointer hover:bg-surface-hover/40 transition-colors text-sm border-b border-surface-hover/50"
    >
      <span className="shrink-0 text-text-secondary/60">{icon}</span>
      <div className="flex-1 min-w-0">
        <div className="truncate text-text-primary text-xs">{label}</div>
        {sub && <div className="truncate text-[10px] text-text-secondary/50">{sub}</div>}
      </div>
    </div>
  );
}

function PaperChildren({ paper }: { paper: Paper }) {
  const navigate = useNavigate();
  const { data: notes, isLoading } = usePaperNotes(paper.id);

  const openPdf = () => {
    useTabStore.getState().open({
      id: `reader-${paper.id}`,
      title: paper.title || '未命名',
      icon: 'pdf',
      route: '/reader/$paperId',
      params: { paperId: paper.id },
    });
    navigate({ to: '/reader/$paperId', params: { paperId: paper.id } });
  };

  const openNote = (note: Note) => {
    openNoteTab(navigate, note);
  };

  return (
    <div className="bg-surface/20">
      {paper.file_path && (
        <ChildRow
          icon={<FileText size={14} className="text-primary/80" />}
          label={paper.file_path.split('/').pop() || 'PDF'}
          sub="PDF"
          onClick={openPdf}
          onDoubleClick={openPdf}
        />
      )}
      {isLoading ? (
        <div className="pl-10 py-2 text-xs text-text-secondary/50">
          <Loader2 size={12} className="animate-spin inline mr-1" />
          加载中...
        </div>
      ) : (
        notes?.map((note) => (
          <ChildRow
            key={note.id}
            icon={<StickyNote size={14} className="text-yellow-500/80" />}
            label={note.title || '未命名笔记'}
            sub={note.content_plain.slice(0, 60)}
            onClick={() => openNote(note)}
          />
        ))
      )}
    </div>
  );
}

function PaperRow({
  paper,
  isSelected,
  selectedIds,
  onSelect,
  onRangeSelect,
  index,
  onDelete,
  onRestore,
  onPurge,
  onToggleFavorite,
  onSetReadStatus,
  activeFilter,
  collections,
  visibleColumns,
}: {
  paper: Paper;
  isSelected: boolean;
  selectedIds: string[];
  onSelect: (e: React.MouseEvent, id: string) => void;
  onRangeSelect: (toIndex: number) => void;
  index: number;
  onDelete: (id: string) => void;
  onRestore: (id: string) => void;
  onPurge: (id: string) => void;
  onToggleFavorite: (id: string, favorite: boolean) => void;
  onSetReadStatus: (id: string, status: string) => void;
  activeFilter: ActiveFilter;
  collections: Collection[] | undefined;
  /** Columns currently visible (manual hide ∪ container-width auto-hide). */
  visibleColumns: Set<ColumnKey>;
}) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const { alert, prompt, select } = useDialog();
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const [expanded, setExpanded] = useState(false);
  const authors = parseJsonArray(paper.authors);
  const displayAuthors = authors.length > 0 ? authors.slice(0, 2).join(', ') + (authors.length > 2 ? ' 等' : '') : '—';
  const hasAttachment = !!paper.file_path;
  const addToCollection = useAddPapersToCollection();
  const removeFromCollection = useRemovePapersFromCollection();

  const openPdf = () => {
    useTabStore.getState().open({
      id: `reader-${paper.id}`,
      title: paper.title || '未命名',
      icon: 'pdf',
      route: '/reader/$paperId',
      params: { paperId: paper.id },
    });
    navigate({ to: '/reader/$paperId', params: { paperId: paper.id } });
  };

  const handleClick = (e: React.MouseEvent) => {
    if (e.shiftKey) {
      e.preventDefault();
      onRangeSelect(index);
    } else {
      onSelect(e, paper.id);
    }
  };

  const handleDoubleClick = () => openPdf();

  const handleDragStart = (e: React.DragEvent) => {
    const idsToDrag = isSelected && selectedIds.length > 0 ? selectedIds : [paper.id];
    e.dataTransfer.setData('application/siku-papers', JSON.stringify(idsToDrag));
    e.dataTransfer.effectAllowed = 'move';
  };

  const handleContextMenu = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    setMenu({ x: e.clientX, y: e.clientY });
  }, []);

  const handleImportBibtex = async () => {
    const bibtex = await prompt('粘贴 BibTeX 条目（多条取第一条）：', {
      defaultValue: paper.bibtex || '',
      title: '导入 BibTeX',
      multiline: true,
    });
    if (!bibtex?.trim()) return;
    try {
      await paperImportBibtex(paper.id, bibtex);
      // Refresh list + detail caches so the imported metadata shows up.
      queryClient.invalidateQueries({ queryKey: ['papers'] });
      queryClient.invalidateQueries({ queryKey: ['paper', paper.id] });
      await alert('BibTeX 元数据已导入');
    } catch (err) {
      await alert(`导入失败: ${err}`);
    }
  };

  // Duplicate detection + merge: find papers matching by DOI / normalized
  // title, let the user pick one to merge into the current entry.
  const handleFindDuplicates = async () => {
    try {
      const dups = await paperFindDuplicates(paper.id);
      if (dups.length === 0) {
        await alert('未发现重复项', '查重');
        return;
      }
      const reasons = [...new Set(dups.map((d) => (d.match_reason === 'doi' ? 'DOI' : '标题')))].join('、');
      const choice = await select(
        `发现 ${dups.length} 个疑似重复条目（${reasons}匹配），选择要合并到当前条目的：`,
        {
          title: '合并重复项',
          options: dups.map((d) => ({
            label: `${d.title}${d.year ? ` (${d.year})` : ''}${d.doi ? ` · ${d.doi}` : ''}`,
            value: d.id,
          })),
        }
      );
      if (!choice) return;
      await paperMerge(paper.id, choice);
      await alert('已合并到当前条目', '合并重复项');
      queryClient.invalidateQueries({ queryKey: ['paper', paper.id] });
      queryClient.invalidateQueries({ queryKey: ['papers'] });
    } catch (err) {
      console.error('查重失败:', err);
      await alert(`查重失败: ${err}`, '查重');
    }
  };

  const [reprocessing, setReprocessing] = useState(false);

  const handleReprocessIndex = async () => {
    if (reprocessing) return;
    setReprocessing(true);
    try {
      const ids = targetPaperIds;
      let totalChunks = 0;
      let noText = 0;
      let failed = 0;
      for (const id of ids) {
        try {
          const chunkCount = await paperReprocessIndex(id);
          if (chunkCount > 0) totalChunks += chunkCount;
          else noText += 1;
          // page_count may have changed — refresh the detail cache.
          queryClient.invalidateQueries({ queryKey: ['paper', id] });
        } catch {
          failed += 1;
        }
      }
      if (ids.length === 1) {
        await alert(
          failed > 0
            ? '重建索引失败（详见日志）'
            : totalChunks > 0
              ? `索引已重建，共生成 ${totalChunks} 个文本分块`
              : '未提取到可索引的文本（可能为扫描版 PDF）',
          '重建索引'
        );
      } else {
        await alert(
          `重建完成：${ids.length - failed}/${ids.length} 篇成功，共 ${totalChunks} 个分块` +
            (noText > 0 ? `，${noText} 篇无文本（可能为扫描版）` : '') +
            (failed > 0 ? `，${failed} 篇失败` : ''),
          '重建索引'
        );
      }
    } catch (err) {
      await alert(`重建索引失败: ${err}`, '重建索引');
    } finally {
      setReprocessing(false);
    }
  };

  const handleExportNotes = async () => {
    try {
      const notes = await notesList(paper.id);
      if (notes.length === 0) {
        await alert('该文献没有笔记');
        return;
      }
      const md = notes.map((n) => `## ${n.title}\n\n${n.content}\n\n---\n`).join('\n');
      const title = paper.title || 'notes';
      const blob = new Blob([`# ${title}\n\n${md}`], { type: 'text/markdown' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `${title.slice(0, 40)}_notes.md`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (err) {
      await alert(`导出失败: ${err}`);
    }
  };

  const targetPaperIds = useMemo(
    () => (isSelected && selectedIds.length > 0 ? selectedIds : [paper.id]),
    [isSelected, selectedIds, paper.id]
  );

  // Export the target paper(s) as BibTeX / RIS / CSL-JSON, copying to clipboard.
  const handleExportCitation = async () => {
    const ids = targetPaperIds.length > 1 ? targetPaperIds : [paper.id];
    const fmt = await select('选择导出格式（结果将复制到剪贴板）：', {
      title: '导出引用',
      options: [
        { label: 'BibTeX (.bib)', value: 'bibtex' },
        { label: 'RIS', value: 'ris' },
        { label: 'CSL-JSON', value: 'csl-json' },
      ],
    });
    if (!fmt) return;
    try {
      const text = await paperExport(ids, fmt as 'bibtex' | 'ris' | 'csl-json');
      await navigator.clipboard.writeText(text);
      await alert('已复制到剪贴板', '导出引用');
    } catch (err) {
      await alert(`导出失败: ${err}`, '导出引用');
    }
  };

  // Copy just the citation key (fallback: first author last token + year).
  const handleCopyCitationKey = async () => {
    const firstAuthor = parseJsonArray(paper.authors)[0] ?? '';
    const lastToken = firstAuthor.split(/\s+/).pop() ?? '';
    const fallback = (lastToken + (paper.year ?? '')) || 'paper';
    const key = (paper.citation_key?.trim() || fallback).replace(/\s+/g, '');
    try {
      await navigator.clipboard.writeText(key);
      await alert(`已复制引用键：${key}`, '复制引用键');
    } catch (err) {
      await alert(`复制失败: ${err}`, '复制引用键');
    }
  };

  const handleAddToCollection = async () => {
    const currentId = activeFilter.type === 'collection' ? activeFilter.id : null;
    const options = buildCollectionTree(collections ?? [], currentId);
    if (options.length === 0) {
      await alert('没有可用的分类');
      return;
    }
    const collectionId = await select('选择要添加到的分类', {
      title: '添加到分类',
      options,
    });
    if (!collectionId) return;
    addToCollection.mutate({ collectionId, paperIds: targetPaperIds });
  };

  const handleRemoveFromCurrentCollection = () => {
    if (activeFilter.type !== 'collection') return;
    removeFromCollection.mutate({ collectionId: activeFilter.id, paperIds: targetPaperIds });
  };

  const handleRevealInSystem = async () => {
    if (!paper.id) return;
    try {
      await revealPaperInSystem(paper.id);
    } catch (err) {
      await alert(`打开目录失败: ${err}`);
    }
  };

  const menuItems: ContextMenuItem[] = [
    {
      label: '打开 PDF',
      icon: <File size={14} />,
      onClick: openPdf,
    },
    {
      label: '在系统中打开',
      icon: <ExternalLink size={14} />,
      disabled: !hasAttachment,
      onClick: () => openPaperInSystem(paper.id).catch((err) => console.error('打开失败:', err)),
    },
    {
      label: '打开文件所在目录',
      icon: <FolderOpen size={14} />,
      disabled: !hasAttachment,
      onClick: handleRevealInSystem,
    },
    {
      label: '导出引用',
      icon: <FileDown size={14} />,
      onClick: handleExportCitation,
    },
    {
      label: '复制引用键',
      icon: <Copy size={14} />,
      onClick: handleCopyCitationKey,
    },
    {
      label: paper.is_favorite ? '取消星标' : '星标',
      icon: <Star size={14} />,
      onClick: () => onToggleFavorite(paper.id, paper.is_favorite !== 1),
    },
    {
      label: paper.read_status === 'read' ? '标记为未读' : '标记为已读',
      icon: <BookOpen size={14} />,
      onClick: () => onSetReadStatus(paper.id, paper.read_status === 'read' ? 'unread' : 'read'),
    },
    {
      label: '导入 BibTeX 元数据',
      icon: <BookMarked size={14} />,
      onClick: handleImportBibtex,
    },
    {
      label: '查重（合并重复项）',
      icon: <Copy size={14} />,
      onClick: handleFindDuplicates,
    },
    {
      label: '导出笔记',
      icon: <Download size={14} />,
      onClick: handleExportNotes,
    },
    {
      label: reprocessing
        ? '重建索引中…'
        : `重建索引${targetPaperIds.length > 1 ? ` (${targetPaperIds.length})` : ''}`,
      icon: reprocessing ? <Loader2 size={14} className="animate-spin" /> : <RefreshCw size={14} />,
      disabled: reprocessing || !hasAttachment,
      onClick: handleReprocessIndex,
    },
    {
      label: `添加到分类${targetPaperIds.length > 1 ? ` (${targetPaperIds.length})` : ''}`,
      icon: <FolderPlus size={14} />,
      disabled: !collections || collections.length === 0,
      onClick: handleAddToCollection,
    },
    ...(activeFilter.type === 'collection'
      ? [
          {
            label: `从当前分类移除${targetPaperIds.length > 1 ? ` (${targetPaperIds.length})` : ''}`,
            icon: <FolderMinus size={14} />,
            onClick: handleRemoveFromCurrentCollection,
          } as ContextMenuItem,
        ]
      : []),
    ...(activeFilter.type === 'trash'
      ? [
          {
            label: '恢复',
            icon: <RotateCcw size={14} />,
            onClick: () => onRestore(paper.id),
          },
          {
            label: '永久删除',
            icon: <Trash2 size={14} />,
            destructive: true,
            onClick: () => onPurge(paper.id),
          },
        ]
      : [
          {
            label: '删除文献',
            icon: <Trash2 size={14} />,
            destructive: true,
            onClick: () => onDelete(paper.id),
          },
        ]),
  ];

  return (
    <>
      <div
        draggable
        onDragStart={handleDragStart}
        onClick={handleClick}
        onDoubleClick={handleDoubleClick}
        onContextMenu={handleContextMenu}
        className={`flex items-center gap-2 px-3 py-2 cursor-pointer transition-colors border-b border-surface-hover text-sm ${
          isSelected ? 'bg-primary/10 text-text-primary' : 'hover:bg-surface-hover/50 text-text-primary'
        }`}
      >
        <button
          onClick={(e) => {
            e.stopPropagation();
            setExpanded((v) => !v);
          }}
          className="shrink-0 p-0.5 rounded hover:bg-surface-hover text-text-secondary/60 hover:text-text-secondary transition-colors"
          title={expanded ? '折叠子项' : '展开子项'}
        >
          {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        </button>

        <div className="flex-1 min-w-24">
          <div className="flex items-center gap-2">
            {paper.read_status === 'unread' && (
              <span className="w-1.5 h-1.5 rounded-full bg-primary shrink-0" title="未读" />
            )}
            <button
              onClick={(e) => {
                e.stopPropagation();
                onToggleFavorite(paper.id, paper.is_favorite !== 1);
              }}
              className={`shrink-0 transition-colors ${
                paper.is_favorite ? 'text-amber-400' : 'text-text-secondary/40 hover:text-text-secondary'
              }`}
              title={paper.is_favorite ? '取消星标' : '星标'}
            >
              <Star size={13} fill={paper.is_favorite ? 'currentColor' : 'none'} />
            </button>
            {hasAttachment && <Paperclip size={12} className="text-text-secondary/50 shrink-0" />}
            <span className="truncate">{paper.title || '未命名文献'}</span>
          </div>
        </div>

        {/* Secondary columns: authors/journal shrink (with truncation) before
            the title does; year/pages/date stay fixed since their content is
            short. Visibility is container-width driven, not viewport. */}
        {visibleColumns.has('authors') && (
          <div className="w-32 shrink min-w-12">
            <span className="block truncate text-xs text-text-secondary">{displayAuthors}</span>
          </div>
        )}

        {visibleColumns.has('year') && (
          <div className="w-16 shrink-0 text-xs text-text-secondary text-center">
            {paper.year || '—'}
          </div>
        )}

        {visibleColumns.has('journal') && (
          <div className="w-36 shrink min-w-12">
            <span className="block truncate text-xs text-text-secondary">{paper.journal || '—'}</span>
          </div>
        )}

        {visibleColumns.has('pages') && (
          <div className="w-14 shrink-0 text-xs text-text-secondary text-center">
            {paper.page_count || '—'}
          </div>
        )}

        {visibleColumns.has('date') && (
          <div className="w-24 shrink-0 text-xs text-text-secondary/60 text-right">
            {isoToDisplay(paper.imported_at).split(' ')[0]}
          </div>
        )}
      </div>

      {expanded && <PaperChildren paper={paper} />}

      {menu && <ContextMenu x={menu.x} y={menu.y} items={menuItems} onClose={() => setMenu(null)} />}
    </>
  );
}

export function PaperList() {
  const activeFilter = useLibraryStore((s) => s.activeFilter);
  const searchQuery = useLibraryStore((s) => s.searchQuery);
  const sortBy = useLibraryStore((s) => s.sortBy);
  const sortOrder = useLibraryStore((s) => s.sortOrder);
  const viewMode = useLibraryStore((s) => s.viewMode);
  const selectedIds = useLibraryStore((s) => s.selectedPaperIds);
  const lastSelectedId = useLibraryStore((s) => s.lastSelectedId);
  const setSearchQuery = useLibraryStore((s) => s.setSearchQuery);
  const toggleSort = useLibraryStore((s) => s.toggleSort);
  const setViewMode = useLibraryStore((s) => s.setViewMode);
  const selectPaper = useLibraryStore((s) => s.selectPaper);
  const clearSelection = useLibraryStore((s) => s.clearSelection);
  const setActiveFilter = useLibraryStore((s) => s.setActiveFilter);
  const deleteMutation = useDeletePaper();
  const favoriteMutation = usePaperSetFavorite();
  const readStatusMutation = usePaperSetReadStatus();
  const navigate = useNavigate();
  const { confirm, prompt, alert } = useDialog();
  const { data: collections } = useCollections();
  const queryClient = useQueryClient();
  const listRef = useRef<HTMLDivElement>(null);
  const [focusedIndex, setFocusedIndex] = useState<number>(-1);
  const [showFilters, setShowFilters] = useState(false);
  const hiddenColumns = useLibraryStore((s) => s.hiddenColumns);
  const toggleHiddenColumn = useLibraryStore((s) => s.toggleHiddenColumn);
  const [listWidth, setListWidth] = useState(0);
  const [colMenu, setColMenu] = useState<{ x: number; y: number } | null>(null);

  const yearFrom = useLibraryStore((s) => s.yearFrom);
  const yearTo = useLibraryStore((s) => s.yearTo);
  const journalFilter = useLibraryStore((s) => s.journalFilter);
  const statusFilter = useLibraryStore((s) => s.statusFilter);
  const setYearFrom = useLibraryStore((s) => s.setYearFrom);
  const setYearTo = useLibraryStore((s) => s.setYearTo);
  const setJournalFilter = useLibraryStore((s) => s.setJournalFilter);
  const setStatusFilter = useLibraryStore((s) => s.setStatusFilter);
  const clearAdvancedFilters = useLibraryStore((s) => s.clearAdvancedFilters);

  const handleToggleFavorite = useCallback(
    (id: string, favorite: boolean) => favoriteMutation.mutate({ id, favorite }),
    [favoriteMutation]
  );
  const handleSetReadStatus = useCallback(
    (id: string, status: string) => readStatusMutation.mutate({ id, status }),
    [readStatusMutation]
  );

  const handleSaveSearch = async () => {
    const name = await prompt('保存当前搜索，输入名称：', { title: '保存搜索', placeholder: '例如：2024-2026 机器学习' });
    if (!name) return;
    const params = {
      search: searchQuery || undefined,
      year_from: yearFrom ? Number(yearFrom) : undefined,
      year_to: yearTo ? Number(yearTo) : undefined,
      journal: journalFilter || undefined,
      read_status: statusFilter === 'unread' ? 'unread' : undefined,
      is_favorite: statusFilter === 'favorites' ? true : undefined,
    };
    try {
      await savedSearchesCreate(name.trim(), JSON.stringify(params));
      queryClient.invalidateQueries({ queryKey: ['saved-searches'] });
      await alert('已保存搜索', '保存搜索');
    } catch (err) {
      await alert(`保存搜索失败: ${err}`, '保存搜索');
    }
  };

  const params: ListPapersParams = useMemo(() => {
    const base: ListPapersParams = {
      search: searchQuery || undefined,
      sort_by: activeFilter.type === 'recent' ? 'last_read_at' : sortBy,
      sort_order: activeFilter.type === 'recent' ? 'desc' : sortOrder,
    };
    if (activeFilter.type === 'trash') {
      base.include_deleted = true;
      return base;
    }
    if (activeFilter.type === 'recent') {
      // list_papers excludes last_read_at IS NULL when sorting by it.
      return base;
    }
    if (activeFilter.type === 'collection') base.collection_id = activeFilter.id;
    if (activeFilter.tagIds.length > 0) {
      base.tag_ids = activeFilter.tagIds;
      base.tag_logic = activeFilter.tagLogic;
    }
    if (yearFrom) base.year_from = Number(yearFrom);
    if (yearTo) base.year_to = Number(yearTo);
    if (journalFilter) base.journal = journalFilter;
    if (statusFilter === 'favorites') base.is_favorite = true;
    if (statusFilter === 'unread') base.read_status = 'unread';
    return base;
  }, [activeFilter, searchQuery, sortBy, sortOrder, yearFrom, yearTo, journalFilter, statusFilter]);

  const { data: papers, isLoading, isError, refetch } = usePapers(params);

  const paperIds = useMemo(() => papers?.map((p) => p.id) ?? [], [papers]);

  useEffect(() => {
    setFocusedIndex(-1);
  }, [paperIds.join(',')]);

  // Measure the list container: column auto-hide reacts to the LIST width,
  // not the viewport — side panels resize independently of the window.
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      setListWidth(entries[0].contentRect.width);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [viewMode, papers?.length]);

  /** Columns actually rendered: manually hidden columns and columns
   * auto-hidden because the list got too narrow are both excluded. */
  const visibleColumns = useMemo<Set<ColumnKey>>(() => {
    const auto = autoHiddenColumns(listWidth);
    return new Set(
      COLUMN_DEFS.map((c) => c.key).filter((k) => !auto.has(k) && !hiddenColumns.includes(k))
    );
  }, [listWidth, hiddenColumns]);

  const columnMenuItems: ContextMenuItem[] = COLUMN_DEFS.map((col) => ({
    label: col.label,
    icon: hiddenColumns.includes(col.key) ? (
      <span className="w-3.5" />
    ) : (
      <Check size={14} className="text-primary" />
    ),
    onClick: () => toggleHiddenColumn(col.key),
  }));

  // Focus the list only when view mode changes, not when papers data changes,
  // so that typing in the search box is not interrupted.
  useEffect(() => {
    if (papers && papers.length > 0 && viewMode === 'table' && listRef.current) {
      listRef.current.focus({ preventScroll: true });
    }
  }, [viewMode]);

  const handleSelect = (e: React.MouseEvent, id: string) => {
    const multi = e.ctrlKey || e.metaKey;
    selectPaper(id, multi);
  };

  const handleRangeSelect = (toIndex: number) => {
    if (!lastSelectedId || paperIds.length === 0) return;
    const fromIndex = paperIds.indexOf(lastSelectedId);
    if (fromIndex === -1) return;
    const start = Math.min(fromIndex, toIndex);
    const end = Math.max(fromIndex, toIndex);
    const rangeIds = paperIds.slice(start, end + 1);
    useLibraryStore.setState({ selectedPaperIds: rangeIds, lastSelectedId: paperIds[toIndex] });
  };

  const handleDelete = useCallback(
    async (id: string) => {
      const ok = await confirm('删除后可在「回收站」恢复，确定删除该文献？', '删除文献');
      if (!ok) return;
      deleteMutation.mutate(id);
    },
    [confirm, deleteMutation]
  );

  const restoreMutation = useRestorePaper();
  const handleRestore = useCallback(
    (id: string) => restoreMutation.mutate(id),
    [restoreMutation]
  );

  const purgeMutation = usePurgePaper();
  const handlePurge = useCallback(
    async (id: string) => {
      const ok = await confirm('永久删除后不可恢复（附件、笔记、标注一并删除），确定？', '永久删除');
      if (!ok) return;
      purgeMutation.mutate(id);
    },
    [confirm, purgeMutation]
  );

  const openPaperPdf = useCallback(
    (paper: Paper) => {
      useTabStore.getState().open({
        id: `reader-${paper.id}`,
        title: paper.title || '未命名',
        icon: 'pdf',
        route: '/reader/$paperId',
        params: { paperId: paper.id },
      });
      navigate({ to: '/reader/$paperId', params: { paperId: paper.id } });
    },
    [navigate]
  );

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (!papers || papers.length === 0) return;

    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setFocusedIndex((prev) => {
        const next = prev < 0 ? 0 : Math.min(prev + 1, papers.length - 1);
        scrollRowIntoView(next);
        selectPaper(papers[next].id, false);
        return next;
      });
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setFocusedIndex((prev) => {
        const next = prev < 0 ? papers.length - 1 : Math.max(prev - 1, 0);
        scrollRowIntoView(next);
        selectPaper(papers[next].id, false);
        return next;
      });
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const idx = focusedIndex >= 0 ? focusedIndex : selectedIds.length === 1 ? paperIds.indexOf(selectedIds[0]) : -1;
      if (idx >= 0 && papers[idx]) {
        openPaperPdf(papers[idx]);
      }
    } else if (e.key === 'Delete' || e.key === 'Backspace') {
      e.preventDefault();
      const idx = focusedIndex >= 0 ? focusedIndex : selectedIds.length === 1 ? paperIds.indexOf(selectedIds[0]) : -1;
      if (idx >= 0 && papers[idx]) {
        handleDelete(papers[idx].id);
      }
    }
  };

  const scrollRowIntoView = (index: number) => {
    const row = listRef.current?.querySelector(`[data-row-index="${index}"]`) as HTMLElement | null;
    row?.scrollIntoView({ block: 'nearest' });
  };

  const [importMenuOpen, setImportMenuOpen] = useState(false);
  const importMenuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!importMenuOpen) return;
    const onClick = (e: MouseEvent) => {
      if (importMenuRef.current && !importMenuRef.current.contains(e.target as Node)) {
        setImportMenuOpen(false);
      }
    };
    document.addEventListener('mousedown', onClick);
    return () => document.removeEventListener('mousedown', onClick);
  }, [importMenuOpen]);

  return (
    <div className="flex flex-col h-full">
      {/* Toolbar */}
      <div className="flex items-center gap-2 px-3 py-2 border-b border-surface-hover shrink-0">
        <div className="flex-1 min-w-0 relative">
          <Search size={14} className="absolute left-2.5 top-1/2 -translate-y-1/2 text-text-secondary/50" />
          <input
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="搜索标题、作者、PDF 全文..."
            className="w-full h-8 pl-8 pr-3 rounded-lg bg-surface border border-surface-hover text-sm text-text-primary placeholder:text-text-secondary/40 focus:outline-none focus:border-primary/50"
          />
        </div>

        <button
          onClick={() => setShowFilters((v) => !v)}
          title="高级筛选"
          className={`h-8 w-8 flex items-center justify-center rounded-lg border transition-colors ${
            showFilters || yearFrom || yearTo || journalFilter || statusFilter !== 'all'
              ? 'bg-primary/15 text-primary border-primary/30'
              : 'bg-surface border-surface-hover text-text-secondary hover:text-text-primary'
          }`}
        >
          <Filter size={14} />
        </button>

        {activeFilter.type !== 'trash' && (
          <button
            onClick={handleSaveSearch}
            title="保存当前搜索"
            className="h-8 px-2.5 flex items-center justify-center rounded-lg bg-surface border border-surface-hover text-text-secondary hover:text-text-primary transition-colors"
          >
            <BookmarkPlus size={14} />
          </button>
        )}

        <div className="relative shrink-0" ref={importMenuRef}>
          <button
            onClick={() => setImportMenuOpen((o) => !o)}
            className="flex items-center gap-1.5 h-8 px-3 rounded-lg bg-primary/10 text-primary text-xs font-medium hover:bg-primary/20 transition-colors"
          >
            <Plus size={14} />
            导入
            <ChevronDown size={12} className={`transition-transform ${importMenuOpen ? 'rotate-180' : ''}`} />
          </button>
          {importMenuOpen && (
            <div className="absolute right-0 top-full mt-1 w-40 bg-surface border border-surface-hover rounded-lg shadow-xl py-1 z-50">
              <button
                onClick={() => {
                  setImportMenuOpen(false);
                  window.dispatchEvent(new CustomEvent('siku:import-pdf'));
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary hover:bg-surface-hover text-left"
              >
                <File size={13} />
                导入 PDF
              </button>
              <button
                onClick={() => {
                  setImportMenuOpen(false);
                  window.dispatchEvent(new CustomEvent('siku:import-from-link'));
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary hover:bg-surface-hover text-left"
              >
                <Link2 size={13} />
                从链接导入
              </button>
            </div>
          )}
        </div>

        <div className="flex items-center border border-surface-hover rounded-lg overflow-hidden shrink-0">
          <button
            onClick={() => setViewMode('table')}
            className={`h-8 w-8 flex items-center justify-center transition-colors ${
              viewMode === 'table' ? 'bg-surface-hover text-text-primary' : 'text-text-secondary hover:text-text-primary'
            }`}
            title="表格视图"
          >
            <List size={14} />
          </button>
          <button
            onClick={() => setViewMode('card')}
            className={`h-8 w-8 flex items-center justify-center transition-colors ${
              viewMode === 'card' ? 'bg-surface-hover text-text-primary' : 'text-text-secondary hover:text-text-primary'
            }`}
            title="卡片视图"
          >
            <LayoutGrid size={14} />
          </button>
        </div>
      </div>

      {/* Advanced filters */}
      {showFilters && (
        <div className="flex flex-wrap items-center gap-2 px-3 py-2 border-b border-surface-hover bg-surface/20 shrink-0">
          <input
            type="number"
            value={yearFrom}
            onChange={(e) => setYearFrom(e.target.value)}
            placeholder="年份从"
            className="w-20 h-7 px-2 rounded-lg bg-surface border border-surface-hover text-xs text-text-primary focus:outline-none focus:border-primary/50"
          />
          <span className="text-xs text-text-secondary/50">—</span>
          <input
            type="number"
            value={yearTo}
            onChange={(e) => setYearTo(e.target.value)}
            placeholder="年份至"
            className="w-20 h-7 px-2 rounded-lg bg-surface border border-surface-hover text-xs text-text-primary focus:outline-none focus:border-primary/50"
          />
          <input
            type="text"
            value={journalFilter}
            onChange={(e) => setJournalFilter(e.target.value)}
            placeholder="期刊（包含）"
            className="w-36 h-7 px-2 rounded-lg bg-surface border border-surface-hover text-xs text-text-primary focus:outline-none focus:border-primary/50"
          />
          <div className="flex items-center border border-surface-hover rounded-lg overflow-hidden h-7">
            {(['all', 'favorites', 'unread'] as const).map((s) => (
              <button
                key={s}
                onClick={() => setStatusFilter(s)}
                className={`px-2.5 text-xs transition-colors ${
                  statusFilter === s ? 'bg-primary/15 text-primary' : 'text-text-secondary hover:text-text-primary'
                }`}
              >
                {s === 'all' ? '全部' : s === 'favorites' ? '星标' : '未读'}
              </button>
            ))}
          </div>
          <button
            onClick={clearAdvancedFilters}
            className="text-xs text-text-secondary hover:text-text-primary underline"
          >
            清除筛选
          </button>
        </div>
      )}

      {/* Active filter breadcrumb */}
      {(activeFilter.type === 'collection' || activeFilter.type === 'recent' || activeFilter.tagIds.length > 0) && (
        <div className="flex items-center gap-2 px-3 py-1.5 border-b border-surface-hover text-xs text-text-secondary bg-surface/20 shrink-0">
          <span className="opacity-60">当前筛选：</span>
          {activeFilter.type === 'collection' && (
            <span className="px-2 py-0.5 rounded-full bg-primary/10 text-primary">
              {collections?.find((c) => c.id === activeFilter.id)?.name ?? '集合'}
            </span>
          )}
          {activeFilter.type === 'recent' && (
            <span className="px-2 py-0.5 rounded-full bg-primary/10 text-primary">最近阅读</span>
          )}
          {activeFilter.tagIds.length > 0 && (
            <span className="px-2 py-0.5 rounded-full bg-primary/10 text-primary">
              标签 ×{activeFilter.tagIds.length}（{activeFilter.tagLogic === 'and' ? '全部' : '任一'}）
            </span>
          )}
          <button
            onClick={() => setActiveFilter({ type: 'all', tagIds: [], tagLogic: 'or' })}
            className="hover:text-text-primary underline"
          >
            清除
          </button>
        </div>
      )}

      {/* Content */}
      {isLoading ? (
        <div className="flex-1 flex items-center justify-center text-text-secondary">
          <Loader2 size={24} className="animate-spin" />
        </div>
      ) : isError ? (
        <div className="flex flex-col items-center justify-center flex-1 text-text-secondary">
          <FileWarning size={40} className="mb-3 text-red-400" />
          <p className="text-sm mb-2">加载文献失败</p>
          <button
            onClick={() => refetch()}
            className="px-3 py-1.5 rounded-lg bg-surface border border-surface-hover text-xs hover:bg-surface-hover"
          >
            重试
          </button>
        </div>
      ) : !papers || papers.length === 0 ? (
        <div className="flex flex-col items-center justify-center flex-1 text-text-secondary">
          <Inbox size={48} className="mb-3 text-text-secondary/40" />
          <p className="text-sm">
            {searchQuery
              ? '没有找到匹配的文献'
              : activeFilter.type === 'all' && activeFilter.tagIds.length === 0
                ? '还没有导入任何文献'
                : activeFilter.type === 'recent'
                  ? '还没有阅读过任何文献'
                  : '该筛选条件下没有文献'}
          </p>
          {!searchQuery && activeFilter.type === 'all' && activeFilter.tagIds.length === 0 && (
            <p className="text-xs mt-1 opacity-60">点击右上角「导入」开始添加。</p>
          )}
          {!searchQuery && activeFilter.type === 'recent' && (
            <p className="text-xs mt-1 opacity-60">打开任意文献开始阅读后会出现在这里。</p>
          )}
        </div>
      ) : viewMode === 'table' ? (
        <div
          ref={listRef}
          tabIndex={0}
          onKeyDown={handleKeyDown}
          onClick={clearSelection}
          className="flex-1 overflow-y-auto outline-none focus:bg-surface-hover/10"
        >
          {/* Column headers (right-click to show/hide columns) */}
          <div
            className="sticky top-0 z-10 flex items-center gap-2 px-3 py-2 border-b border-surface-hover bg-surface/80 backdrop-blur text-xs text-text-secondary/70"
            onClick={(e) => e.stopPropagation()}
            onContextMenu={(e) => {
              e.preventDefault();
              e.stopPropagation();
              setColMenu({ x: e.clientX, y: e.clientY });
            }}
          >
            <div className="w-5 shrink-0" />
            {activeFilter.type === 'recent' ? (
              <div className="flex-1 min-w-24 flex items-center gap-1">
                <FileText size={12} /> 标题
              </div>
            ) : (
              <button onClick={() => toggleSort('title')} className="flex-1 min-w-24 flex items-center gap-1 text-left hover:text-text-secondary">
                <FileText size={12} /> 标题 <SortIcon field="title" current={sortBy} order={sortOrder} />
              </button>
            )}
            {visibleColumns.has('authors') && (
              <div className="w-32 shrink min-w-12 flex items-center gap-1">
                <User size={12} /> 作者
              </div>
            )}
            {visibleColumns.has('year') && (
              activeFilter.type === 'recent' ? (
                <div className="w-16 shrink-0 flex items-center justify-center gap-1">
                  <Calendar size={12} /> 年份
                </div>
              ) : (
                <button onClick={() => toggleSort('year')} className="w-16 shrink-0 flex items-center justify-center gap-1 hover:text-text-secondary">
                  <Calendar size={12} /> 年份 <SortIcon field="year" current={sortBy} order={sortOrder} />
                </button>
              )
            )}
            {visibleColumns.has('journal') && (
              <div className="w-36 shrink min-w-12 flex items-center gap-1">
                <BookOpen size={12} /> 期刊
              </div>
            )}
            {visibleColumns.has('pages') && (
              <div className="w-14 shrink-0 flex items-center justify-center gap-1">
                <FileText size={12} /> 页数
              </div>
            )}
            {visibleColumns.has('date') && (
              activeFilter.type === 'recent' ? (
                <div className="w-24 shrink-0 flex items-center justify-end gap-1">
                  最近阅读
                </div>
              ) : (
                <button onClick={() => toggleSort('imported_at')} className="w-24 shrink-0 flex items-center justify-end gap-1 hover:text-text-secondary">
                  导入日期 <SortIcon field="imported_at" current={sortBy} order={sortOrder} />
                </button>
              )
            )}
          </div>

          {/* Rows */}
          {papers.map((paper, index) => (
            <div
              key={paper.id}
              data-row-index={index}
              onClick={(e) => {
                e.stopPropagation();
                setFocusedIndex(index);
              }}
              className={focusedIndex === index ? 'ring-1 ring-inset ring-primary/30' : ''}
            >
              <PaperRow
                paper={paper}
                isSelected={selectedIds.includes(paper.id)}
                selectedIds={selectedIds}
                onSelect={handleSelect}
                onRangeSelect={handleRangeSelect}
                index={index}
                onDelete={handleDelete}
                onRestore={handleRestore}
                onPurge={handlePurge}
                onToggleFavorite={handleToggleFavorite}
                onSetReadStatus={handleSetReadStatus}
                activeFilter={activeFilter}
                collections={collections}
                visibleColumns={visibleColumns}
              />
            </div>
          ))}
        </div>
      ) : (
        <div className="flex-1 overflow-y-auto p-4" onClick={clearSelection}>
          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-3">
            {papers.map((paper) => (
              <div
                key={paper.id}
                onClick={(e) => handleSelect(e, paper.id)}
                onDoubleClick={() => {
                  useTabStore.getState().open({
                    id: `reader-${paper.id}`,
                    title: paper.title || '未命名',
                    icon: 'pdf',
                    route: '/reader/$paperId',
                    params: { paperId: paper.id },
                  });
                }}
              >
                <PaperCard paper={paper} isSelected={selectedIds.includes(paper.id)} onClick={() => {}} />
              </div>
            ))}
          </div>
        </div>
      )}

      {colMenu && (
        <ContextMenu x={colMenu.x} y={colMenu.y} items={columnMenuItems} onClose={() => setColMenu(null)} />
      )}
    </div>
  );
}
