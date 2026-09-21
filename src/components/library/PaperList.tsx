import { useState, useCallback, useMemo, useRef, useEffect } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { useVirtualizer } from '@tanstack/react-virtual';
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
  FolderInput,
  BookCopy,
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
import { isoToDisplayFull } from '@/lib/time';
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

type SortField = 'title' | 'year' | 'imported_at' | 'updated_at' | 'last_read_at';

/** Toggleable paper-list columns (the title column is always visible).
 *  Order matches the row layout. Date columns are ordered by display
 *  priority: 最后阅读 > 修改日期 > 导入日期. */
type ColumnKey = 'authors' | 'year' | 'journal' | 'pages' | 'lastRead' | 'modified' | 'date';

const COLUMN_DEFS: { key: ColumnKey; label: string }[] = [
  { key: 'authors', label: '作者' },
  { key: 'year', label: '年份' },
  { key: 'journal', label: '期刊' },
  { key: 'pages', label: '页数' },
  { key: 'lastRead', label: '最后阅读' },
  { key: 'modified', label: '修改日期' },
  { key: 'date', label: '导入日期' },
];

/** Container-width thresholds: below `below` px the listed columns
 * auto-hide (least valuable first), so the title column is squeezed last.
 * Applied cumulatively — the first matching (narrowest) band wins.
 * Date columns hide in reverse display priority: 导入日期 → 修改日期 →
 * 最后阅读. */
const AUTO_HIDE_BANDS: { below: number; keys: ColumnKey[] }[] = [
  { below: 460, keys: ['pages', 'journal', 'date', 'modified', 'authors', 'lastRead'] },
  { below: 580, keys: ['pages', 'journal', 'date', 'modified', 'authors'] },
  { below: 700, keys: ['pages', 'journal', 'date', 'modified'] },
  { below: 800, keys: ['pages', 'journal', 'date'] },
  { below: 920, keys: ['pages', 'journal'] },
  { below: 1020, keys: ['pages'] },
];

function autoHiddenColumns(width: number): Set<ColumnKey> {
  if (width <= 0) return new Set(); // not measured yet — show everything
  for (const band of AUTO_HIDE_BANDS) {
    if (width < band.below) return new Set(band.keys);
  }
  return new Set();
}

/** Default / minimum widths (px) for the resizable secondary columns.
 *  Title always takes the remaining space (flex-1). */
const DEFAULT_COL_WIDTHS: Record<ColumnKey, number> = {
  authors: 128, year: 64, journal: 144, pages: 56, lastRead: 144, modified: 144, date: 144,
};
const MIN_COL_WIDTHS: Record<ColumnKey, number> = {
  authors: 56, year: 44, journal: 56, pages: 40, lastRead: 96, modified: 96, date: 96,
};

/** Header cell config, in row order. `sortField` marks click-to-sort
 *  columns (disabled in the "recently read" filter, which has a fixed order). */
const HEADER_COLS: {
  key: ColumnKey;
  label: string;
  align: 'left' | 'center' | 'right';
  sortField?: SortField;
  icon: React.ReactNode;
}[] = [
  { key: 'authors', label: '作者', align: 'left', icon: <User size={12} /> },
  { key: 'year', label: '年份', align: 'center', sortField: 'year', icon: <Calendar size={12} /> },
  { key: 'journal', label: '期刊', align: 'left', icon: <BookOpen size={12} /> },
  { key: 'pages', label: '页数', align: 'center', icon: <FileText size={12} /> },
  { key: 'lastRead', label: '最后阅读', align: 'right', sortField: 'last_read_at', icon: null },
  { key: 'modified', label: '修改日期', align: 'right', sortField: 'updated_at', icon: null },
  { key: 'date', label: '导入日期', align: 'right', sortField: 'imported_at', icon: null },
];

function colWidthOf(columnWidths: Record<string, number>, key: ColumnKey): number {
  return columnWidths[key] ?? DEFAULT_COL_WIDTHS[key];
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
  columnWidths,
}: {
  paper: Paper;
  isSelected: boolean;
  selectedIds: string[];
  onSelect: (e: React.MouseEvent, id: string) => void;
  onRangeSelect: (toIndex: number) => void;
  index: number;
  onDelete: (ids: string[]) => void;
  onRestore: (ids: string[]) => void;
  onPurge: (ids: string[]) => void;
  onToggleFavorite: (ids: string[], favorite: boolean) => void;
  onSetReadStatus: (ids: string[], status: string) => void;
  activeFilter: ActiveFilter;
  collections: Collection[] | undefined;
  /** Columns currently visible (manual hide ∪ container-width auto-hide). */
  visibleColumns: Set<ColumnKey>;
  /** User-adjusted column widths (missing keys use defaults). */
  columnWidths: Record<string, number>;
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
      onClick: () => onToggleFavorite(targetPaperIds, paper.is_favorite !== 1),
    },
    {
      label: paper.read_status === 'read' ? '标记为未读' : '标记为已读',
      icon: <BookOpen size={14} />,
      onClick: () => onSetReadStatus(targetPaperIds, paper.read_status === 'read' ? 'unread' : 'read'),
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
            label: `恢复${targetPaperIds.length > 1 ? ` (${targetPaperIds.length})` : ''}`,
            icon: <RotateCcw size={14} />,
            onClick: () => onRestore(targetPaperIds),
          },
          {
            label: `永久删除${targetPaperIds.length > 1 ? ` (${targetPaperIds.length})` : ''}`,
            icon: <Trash2 size={14} />,
            destructive: true,
            onClick: () => onPurge(targetPaperIds),
          },
        ]
      : [
          {
            label: `删除文献${targetPaperIds.length > 1 ? ` (${targetPaperIds.length})` : ''}`,
            icon: <Trash2 size={14} />,
            destructive: true,
            onClick: () => onDelete(targetPaperIds),
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
                onToggleFavorite([paper.id], paper.is_favorite !== 1);
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

        {/* Secondary columns: width comes from the resizable header (user
            drag), falling back to defaults. Text columns truncate. */}
        {visibleColumns.has('authors') && (
          <div className="shrink-0 min-w-0" style={{ width: colWidthOf(columnWidths, 'authors') }}>
            <span className="block truncate text-xs text-text-secondary">{displayAuthors}</span>
          </div>
        )}

        {visibleColumns.has('year') && (
          <div className="shrink-0 text-xs text-text-secondary text-center" style={{ width: colWidthOf(columnWidths, 'year') }}>
            {paper.year || '—'}
          </div>
        )}

        {visibleColumns.has('journal') && (
          <div className="shrink-0 min-w-0" style={{ width: colWidthOf(columnWidths, 'journal') }}>
            <span className="block truncate text-xs text-text-secondary">{paper.journal || '—'}</span>
          </div>
        )}

        {visibleColumns.has('pages') && (
          <div className="shrink-0 text-xs text-text-secondary text-center" style={{ width: colWidthOf(columnWidths, 'pages') }}>
            {paper.page_count || '—'}
          </div>
        )}

        {visibleColumns.has('lastRead') && (
          <div className="shrink-0 text-xs text-text-secondary/60 text-right" style={{ width: colWidthOf(columnWidths, 'lastRead') }}>
            {paper.last_read_at ? isoToDisplayFull(paper.last_read_at) : '—'}
          </div>
        )}

        {visibleColumns.has('modified') && (
          <div className="shrink-0 text-xs text-text-secondary/60 text-right" style={{ width: colWidthOf(columnWidths, 'modified') }}>
            {paper.updated_at ? isoToDisplayFull(paper.updated_at) : '—'}
          </div>
        )}

        {visibleColumns.has('date') && (
          <div className="shrink-0 text-xs text-text-secondary/60 text-right" style={{ width: colWidthOf(columnWidths, 'date') }}>
            {paper.imported_at ? isoToDisplayFull(paper.imported_at) : '—'}
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
  const headerRef = useRef<HTMLDivElement>(null);
  const [headerHeight, setHeaderHeight] = useState(33);
  const [focusedIndex, setFocusedIndex] = useState<number>(-1);
  const [showFilters, setShowFilters] = useState(false);
  const hiddenColumns = useLibraryStore((s) => s.hiddenColumns);
  const toggleHiddenColumn = useLibraryStore((s) => s.toggleHiddenColumn);
  const columnWidths = useLibraryStore((s) => s.columnWidths);
  const setColumnWidth = useLibraryStore((s) => s.setColumnWidth);
  const resetColumnWidth = useLibraryStore((s) => s.resetColumnWidth);
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
    (ids: string[], favorite: boolean) => {
      for (const id of ids) favoriteMutation.mutate({ id, favorite });
    },
    [favoriteMutation]
  );
  const handleSetReadStatus = useCallback(
    (ids: string[], status: string) => {
      for (const id of ids) readStatusMutation.mutate({ id, status });
    },
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
      // Only papers actually opened; sorting by last_read_at alone no longer
      // excludes unread papers (column sorting must not hide rows).
      base.has_been_read = true;
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

  // Track the sticky header's height so virtualized scroll-into-view leaves
  // clearance for it (scrollPaddingStart).
  useEffect(() => {
    const el = headerRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      setHeaderHeight(entries[0].contentRect.height);
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

  /** Drag-to-resize a column header (Zotero-style). Double-clicking the
   *  handle resets to the default width. */
  const startColumnResize = (e: React.MouseEvent, key: ColumnKey) => {
    e.preventDefault();
    e.stopPropagation();
    const startX = e.clientX;
    const startWidth = colWidthOf(columnWidths, key);
    const minW = MIN_COL_WIDTHS[key];
    const onMove = (ev: MouseEvent) => {
      setColumnWidth(key, Math.max(minW, Math.round(startWidth + ev.clientX - startX)));
    };
    const onUp = () => {
      document.removeEventListener('mousemove', onMove);
      document.removeEventListener('mouseup', onUp);
    };
    document.addEventListener('mousemove', onMove);
    document.addEventListener('mouseup', onUp);
  };

  /** Virtualized rows: only the viewport ± overscan is mounted, so libraries
   *  with thousands of papers render as cheaply as small ones. Row heights
   *  are measured (expanded child rows are taller than the estimate).
   *  scrollPaddingStart keeps scrolled-to rows clear of the sticky header. */
  const rowVirtualizer = useVirtualizer({
    count: papers?.length ?? 0,
    getScrollElement: () => listRef.current,
    estimateSize: () => 37,
    overscan: 15,
    getItemKey: (index) => papers?.[index]?.id ?? index,
    scrollPaddingStart: headerHeight,
  });

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
    async (ids: string[]) => {
      const ok = await confirm(
        ids.length > 1
          ? `删除后可在「回收站」恢复，确定删除选中的 ${ids.length} 篇文献？`
          : '删除后可在「回收站」恢复，确定删除该文献？',
        '删除文献'
      );
      if (!ok) return;
      for (const id of ids) deleteMutation.mutate(id);
    },
    [confirm, deleteMutation]
  );

  const restoreMutation = useRestorePaper();
  const handleRestore = useCallback(
    (ids: string[]) => {
      for (const id of ids) restoreMutation.mutate(id);
    },
    [restoreMutation]
  );

  const purgeMutation = usePurgePaper();
  const handlePurge = useCallback(
    async (ids: string[]) => {
      const ok = await confirm(
        ids.length > 1
          ? `永久删除后不可恢复（附件、笔记、标注一并删除），确定删除选中的 ${ids.length} 篇文献？`
          : '永久删除后不可恢复（附件、笔记、标注一并删除），确定？',
        '永久删除'
      );
      if (!ok) return;
      for (const id of ids) purgeMutation.mutate(id);
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
      if (selectedIds.length > 0) {
        handleDelete(selectedIds);
      } else {
        const idx = focusedIndex >= 0 ? focusedIndex : -1;
        if (idx >= 0 && papers[idx]) {
          handleDelete([papers[idx].id]);
        }
      }
    }
  };

  const scrollRowIntoView = (index: number) => {
    rowVirtualizer.scrollToIndex(index, { align: 'auto' });
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
                  window.dispatchEvent(new CustomEvent('siku:import-folder'));
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary hover:bg-surface-hover text-left"
              >
                <FolderInput size={13} />
                从文件夹导入
              </button>
              <button
                onClick={() => {
                  setImportMenuOpen(false);
                  window.dispatchEvent(new CustomEvent('siku:import-zotero'));
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary hover:bg-surface-hover text-left"
              >
                <BookCopy size={13} />
                从 Zotero 导入
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
            ref={headerRef}
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
            {HEADER_COLS.filter((c) => visibleColumns.has(c.key)).map((col) => {
              const sortable = col.sortField && activeFilter.type !== 'recent';
              const alignCls =
                col.align === 'center' ? 'justify-center' : col.align === 'right' ? 'justify-end' : '';
              return (
                <div
                  key={col.key}
                  className="relative shrink-0 flex items-center min-w-0"
                  style={{ width: colWidthOf(columnWidths, col.key) }}
                >
                  {sortable ? (
                    <button
                      onClick={() => toggleSort(col.sortField!)}
                      className={`flex-1 min-w-0 flex items-center gap-1 ${alignCls} hover:text-text-secondary`}
                    >
                      {col.icon}
                      <span className="truncate">{col.label}</span>
                      <SortIcon field={col.sortField!} current={sortBy} order={sortOrder} />
                    </button>
                  ) : (
                    <span className={`flex-1 min-w-0 flex items-center gap-1 ${alignCls}`}>
                      {col.icon}
                      <span className="truncate">{col.label}</span>
                    </span>
                  )}
                  <span
                    onMouseDown={(e) => startColumnResize(e, col.key)}
                    onDoubleClick={(e) => {
                      e.stopPropagation();
                      resetColumnWidth(col.key);
                    }}
                    onClick={(e) => e.stopPropagation()}
                    className="absolute -right-1.5 top-0 bottom-0 w-3 z-10 cursor-col-resize touch-none rounded hover:bg-primary/30"
                    title="拖动调整列宽，双击恢复默认"
                  />
                </div>
              );
            })}
          </div>

          {/* Rows (virtualized — only viewport rows are mounted) */}
          <div style={{ height: rowVirtualizer.getTotalSize(), position: 'relative' }}>
            {rowVirtualizer.getVirtualItems().map((vi) => {
              const paper = papers[vi.index];
              if (!paper) return null;
              return (
                <div
                  key={vi.key}
                  data-index={vi.index}
                  ref={rowVirtualizer.measureElement}
                  style={{
                    position: 'absolute',
                    top: 0,
                    left: 0,
                    width: '100%',
                    transform: `translateY(${vi.start}px)`,
                  }}
                >
                  <div
                    onClick={(e) => {
                      e.stopPropagation();
                      setFocusedIndex(vi.index);
                    }}
                    className={focusedIndex === vi.index ? 'ring-1 ring-inset ring-primary/30' : ''}
                  >
                    <PaperRow
                      paper={paper}
                      isSelected={selectedIds.includes(paper.id)}
                      selectedIds={selectedIds}
                      onSelect={handleSelect}
                      onRangeSelect={handleRangeSelect}
                      index={vi.index}
                      onDelete={handleDelete}
                      onRestore={handleRestore}
                      onPurge={handlePurge}
                      onToggleFavorite={handleToggleFavorite}
                      onSetReadStatus={handleSetReadStatus}
                      activeFilter={activeFilter}
                      collections={collections}
                      visibleColumns={visibleColumns}
                      columnWidths={columnWidths}
                    />
                  </div>
                </div>
              );
            })}
          </div>
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
