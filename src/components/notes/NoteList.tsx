import { useState, useEffect, useMemo, useCallback, useRef, type MouseEvent } from 'react';
import {
  Plus, Folder, FileText, ChevronRight, ChevronsUpDown, ChevronsDownUp, Search, Trash2,
  MoreHorizontal, FolderPlus, FilePlus, ArrowUpToLine, X, Crosshair,
  Settings, HelpCircle, Database, Move, Bookmark,
} from 'lucide-react';
import type { Note } from '@/lib/types';
import { parseNoteTags } from '@/lib/types';
import { notesSearch, type NoteSearchResult } from '@/lib/tauri';
import { ContextMenu, type ContextMenuItem } from '@/components/ui/ContextMenu';
import { MoveNoteDialog } from '@/components/notes/MoveNoteDialog';
import { useDialog } from '@/hooks/useDialog';

interface Props {
  notes: Note[];
  activeNoteId: string | null;
  onSelect: (id: string) => void;
  /** Create a new note under the given parent folder (undefined = root). */
  onCreate: (parentId?: string) => void;
  onCreateFolder: () => Promise<string>;
  onCreateSubNote: (parentId: string) => void;
  onCreateSubFolder: (parentId: string) => Promise<string>;
  onRename: (id: string, title: string) => void;
  /** `confirmed` skips the handler's own confirmation dialog (the caller
   *  already showed one). */
  onDelete: (id: string, confirmed?: boolean) => void;
  /** Bulk delete with a single confirmation; falls back to per-item onDelete. */
  onBulkDelete?: (ids: string[]) => void;
  onToggleFavorite?: (id: string, favorite: boolean) => void;
  onMoveToRoot?: (id: string) => void;
  /** Move a note under a folder (or to root when parentId is null). */
  onMoveToFolder?: (id: string, parentId: string | null) => void;
  /** Bulk create a folder and move selected notes into it. */
  onBulkCreateFolder?: (ids: string[]) => Promise<string>;
  /** Bulk move selected notes under a folder (or to root). */
  onBulkMove?: (ids: string[], parentId: string | null) => void;
  onClose?: () => void;
  title?: string;
  /** Current vault name shown in the footer. */
  currentVaultName?: string;
  onOpenVault?: () => void;
  onOpenHelp?: () => void;
  onOpenSettings?: () => void;
}

const ROOT_KEY = '__root__';

export function NoteList({
  notes,
  activeNoteId,
  onSelect,
  onCreate,
  onCreateFolder,
  onCreateSubNote,
  onCreateSubFolder,
  onRename,
  onDelete,
  onBulkDelete,
  onToggleFavorite,
  onMoveToRoot,
  onMoveToFolder,
  onBulkCreateFolder,
  onBulkMove,
  onClose,
  title = '文件列表',
  currentVaultName = 'cognitive-archive',
  onOpenVault,
  onOpenHelp,
  onOpenSettings,
}: Props) {
  const { alert } = useDialog();
  const [search, setSearch] = useState('');
  const [tagFilter, setTagFilter] = useState<string | null>(null);
  const [aiOnly, setAiOnly] = useState(false);
  const [searchResults, setSearchResults] = useState<NoteSearchResult[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [flashIds, setFlashIds] = useState<Set<string>>(new Set());
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [lastSelectedId, setLastSelectedId] = useState<string | null>(null);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; noteId: string; bulk: boolean } | null>(null);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const [sortEnabled, setSortEnabled] = useState(false);
  const [moveTargetIds, setMoveTargetIds] = useState<string[] | null>(null);
  // Pointer-based drag (HTML5 DnD is unreliable in WebView2).
  const [dragPos, setDragPos] = useState<{ x: number; y: number; id: string; title: string } | null>(null);
  const [draggingId, setDraggingId] = useState<string | null>(null);
  const [dropTarget, setDropTarget] = useState<
    { kind: 'folder' | 'note'; id: string; valid: boolean } | { kind: 'root'; valid: boolean } | null
  >(null);
  const [justMovedId, setJustMovedId] = useState<string | null>(null);
  const dragCandidateRef = useRef<string | null>(null);
  const dragStartPosRef = useRef<{ x: number; y: number } | null>(null);
  const dragActiveRef = useRef(false);
  const hoverExpandTimerRef = useRef<number | null>(null);
  const itemRefs = useRef<Map<string, HTMLDivElement>>(new Map());
  const treeRef = useRef<HTMLDivElement>(null);
  const dropTargetRef = useRef<
    { kind: 'folder' | 'note'; id: string; valid: boolean } | { kind: 'root'; valid: boolean } | null
  >(null);
  const autoScrollTimerRef = useRef<number | null>(null);
  const lastMouseYRef = useRef<number | null>(null);

  const allTags = useMemo(() => {
    const set = new Set<string>();
    for (const n of notes) parseNoteTags(n).forEach((t) => set.add(t));
    return [...set].sort((a, b) => a.localeCompare(b, 'zh-CN'));
  }, [notes]);

  const noteMap = useMemo(() => new Map(notes.map((n) => [n.id, n])), [notes]);
  const folderIds = useMemo(() => notes.filter((n) => n.is_folder === 1).map((n) => n.id), [notes]);
  const allFoldersExpanded = folderIds.length > 0 && folderIds.every((id) => expanded.has(id));

  const childrenMap = useMemo(() => {
    const map = new Map<string, Note[]>();
    for (const n of notes) {
      const pid = n.parent_id ?? ROOT_KEY;
      if (!map.has(pid)) map.set(pid, []);
      map.get(pid)!.push(n);
    }
    for (const list of map.values()) {
      // Folders always come before files at the same level (Obsidian-style),
      // so dragging a root-level note can't land it between folders.
      // The system library folder "我的图书馆" is always pinned to the top.
      list.sort((a, b) => {
        if (a.title === '我的图书馆' && b.title !== '我的图书馆') return -1;
        if (b.title === '我的图书馆' && a.title !== '我的图书馆') return 1;
        const af = a.is_folder === 1 ? 0 : 1;
        const bf = b.is_folder === 1 ? 0 : 1;
        if (af !== bf) return af - bf;
        if (sortEnabled) {
          if (a.title === b.title) return 0;
          return a.title.localeCompare(b.title, 'zh-CN');
        }
        if (a.sort_order !== b.sort_order) return a.sort_order - b.sort_order;
        return b.updated_at.localeCompare(a.updated_at);
      });
    }
    return map;
  }, [notes, sortEnabled]);

  const visibleIds = useMemo(() => {
    const q = search.trim().toLowerCase();
    const matchSelf = (n: Note) => {
      if (aiOnly && n.agent_edit_count <= 0) return false;
      if (tagFilter && !parseNoteTags(n).includes(tagFilter)) return false;
      if (!q) return true;
      return (
        n.title.toLowerCase().includes(q) || n.content_plain.toLowerCase().includes(q)
      );
    };

    const ids = new Set<string>();
    const dfs = (id: string): boolean => {
      const n = noteMap.get(id);
      if (!n) return false;
      let selfMatch = matchSelf(n);
      for (const c of childrenMap.get(id) || []) {
        if (dfs(c.id)) selfMatch = true;
      }
      if (selfMatch) ids.add(id);
      return selfMatch;
    };
    for (const r of childrenMap.get(ROOT_KEY) || []) dfs(r.id);
    return ids;
  }, [search, tagFilter, aiOnly, noteMap, childrenMap]);

  // Flat list of visible note ids in tree order (for Shift+click range selection).
  const visibleOrderedIds = useMemo(() => {
    const ids: string[] = [];
    const dfs = (id: string) => {
      if (!visibleIds.has(id)) return;
      ids.push(id);
      for (const c of childrenMap.get(id) || []) dfs(c.id);
    };
    for (const r of childrenMap.get(ROOT_KEY) || []) dfs(r.id);
    return ids;
  }, [visibleIds, childrenMap]);

  // Full-text search (debounced): switches the list to ranked FTS results.
  useEffect(() => {
    const q = search.trim();
    if (!q) {
      setSearchResults(null);
      setSearching(false);
      return;
    }
    setSearching(true);
    const timer = setTimeout(async () => {
      try {
        setSearchResults(await notesSearch(q, 30));
      } catch (err) {
        console.error('notes search:', err);
        setSearchResults([]);
      } finally {
        setSearching(false);
      }
    }, 300);
    return () => clearTimeout(timer);
  }, [search]);

  const toggleExpand = useCallback((id: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const collapseAll = useCallback(() => {
    setExpanded(new Set());
  }, []);

  const expandAll = useCallback(() => {
    const next = new Set<string>();
    for (const n of notes) {
      if (n.is_folder === 1) next.add(n.id);
    }
    setExpanded(next);
  }, [notes]);

  const revealActiveNote = useCallback(() => {
    if (!activeNoteId) return;
    setExpanded((prev) => {
      const next = new Set(prev);
      let id = activeNoteId;
      while (true) {
        const n = noteMap.get(id);
        if (!n?.parent_id) break;
        next.add(n.parent_id);
        id = n.parent_id;
      }
      return next;
    });
    // Scroll after the next render when parents are expanded.
    requestAnimationFrame(() => {
      const el = itemRefs.current.get(activeNoteId);
      if (el) {
        el.scrollIntoView({ behavior: 'smooth', block: 'center' });
      }
    });
  }, [activeNoteId, noteMap]);

  const startRename = useCallback((note: Note) => {
    setRenamingId(note.id);
    setRenameValue(note.title);
  }, []);

  const cancelRename = useCallback(() => {
    setRenamingId(null);
    setRenameValue('');
  }, []);

  const commitRename = useCallback(() => {
    if (renamingId) {
      const name = renameValue.trim();
      // The system library folder name is reserved.
      if (name === '我的图书馆') {
        alert('「我的图书馆」为系统保留名称，不能用于新建目录');
        cancelRename();
        return;
      }
      const current = noteMap.get(renamingId)?.title ?? '';
      if (name && name !== current) {
        onRename(renamingId, name);
      }
    }
    setRenamingId(null);
    setRenameValue('');
  }, [renamingId, renameValue, onRename, cancelRename, alert, noteMap]);

  const openContextMenu = useCallback((e: MouseEvent, note: Note) => {
    e.preventDefault();
    e.stopPropagation();
    const bulk = selectedIds.size > 1 && selectedIds.has(note.id);
    // If right-clicked note is not in the current selection, select it alone.
    if (!selectedIds.has(note.id)) {
      setSelectedIds(new Set([note.id]));
      setLastSelectedId(note.id);
    }
    setContextMenu({ x: e.clientX, y: e.clientY, noteId: note.id, bulk });
  }, [selectedIds]);

  const handleNodeClick = useCallback((e: React.MouseEvent, note: Note) => {
    if (e.shiftKey && lastSelectedId) {
      e.preventDefault();
      e.stopPropagation();
      const idx1 = visibleOrderedIds.indexOf(lastSelectedId);
      const idx2 = visibleOrderedIds.indexOf(note.id);
      if (idx1 !== -1 && idx2 !== -1) {
        const start = Math.min(idx1, idx2);
        const end = Math.max(idx1, idx2);
        const range = visibleOrderedIds.slice(start, end + 1);
        setSelectedIds((prev) => {
          const next = new Set(prev);
          for (const id of range) next.add(id);
          return next;
        });
      }
      setLastSelectedId(note.id);
    } else if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      e.stopPropagation();
      setSelectedIds((prev) => {
        const next = new Set(prev);
        if (next.has(note.id)) next.delete(note.id);
        else next.add(note.id);
        return next;
      });
      setLastSelectedId(note.id);
    } else {
      setSelectedIds(new Set([note.id]));
      setLastSelectedId(note.id);
      onSelect(note.id);
    }
  }, [lastSelectedId, visibleOrderedIds, onSelect]);

  // Create a folder and immediately start renaming it (Obsidian-style):
  // the new "未命名文件夹" appears with its > chevron and an inline editor.
  const createFolderPending = useCallback(
    async (parentId?: string) => {
      const id = parentId ? await onCreateSubFolder(parentId) : await onCreateFolder();
      if (!id) return;
      if (parentId) {
        setExpanded((prev) => new Set(prev).add(parentId));
      }
      setRenamingId(id);
      setRenameValue('未命名文件夹');
    },
    [onCreateFolder, onCreateSubFolder]
  );

  const contextNote = contextMenu && !contextMenu.bulk ? noteMap.get(contextMenu.noteId) : undefined;
  const selectedNotes = useMemo(
    () => notes.filter((n) => selectedIds.has(n.id)),
    [notes, selectedIds]
  );

  // True when `targetId` is inside `ancestorId`'s subtree (used to prevent
  // dropping a folder onto itself or one of its own children).
  const isInSubtree = useCallback(
    (targetId: string, ancestorId: string): boolean => {
      let cur: string | null = targetId;
      let hops = 0;
      while (cur && hops < 100) {
        if (cur === ancestorId) return true;
        const n = noteMap.get(cur);
        cur = n?.parent_id ?? null;
        hops += 1;
      }
      return false;
    },
    [noteMap]
  );

  // ── Pointer-based drag & drop (HTML5 DnD is unreliable in WebView2) ──
  const resolveDropTarget = useCallback(
    (
      clientX: number,
      clientY: number
    ): { kind: 'folder'; id: string } | { kind: 'note'; id: string } | { kind: 'root' } => {
      // Use elementsFromPoint and skip the floating drag indicator so the
      // pointer can't accidentally land on it (WebView2 sometimes reports it
      // even though it is pointer-events-none).
      const elements = document.elementsFromPoint(clientX, clientY);
      for (const el of elements) {
        if ((el as HTMLElement).dataset?.dragIndicator) continue;
        const node = el.closest('[data-note-id]') as HTMLElement | null;
        if (!node) continue;
        const note = noteMap.get(node.dataset.noteId!);
        if (note) {
          return note.is_folder === 1 ? { kind: 'folder', id: note.id } : { kind: 'note', id: note.id };
        }
      }
      return { kind: 'root' };
    },
    [noteMap]
  );

  const isValidDrop = useCallback(
    (target: ReturnType<typeof resolveDropTarget>, candId: string): boolean => {
      if (target.kind === 'root') return true;
      // Dropping onto itself is invalid.
      if (target.id === candId) return false;
      // Dropping into its own subtree is invalid.
      if (target.kind === 'folder' && isInSubtree(target.id, candId)) return false;
      return true;
    },
    [isInSubtree]
  );

  const handleNodeMouseDown = useCallback((e: React.MouseEvent, note: Note) => {
    if (e.button !== 0) return;
    // Don't initiate drag when using multi-selection modifiers.
    if (e.shiftKey || e.ctrlKey || e.metaKey) return;
    dragCandidateRef.current = note.id;
    dragStartPosRef.current = { x: e.clientX, y: e.clientY };
    dragActiveRef.current = false;
  }, []);

  // Global tracking: starts the drag after a small threshold, updates the
  // floating indicator, highlights the hovered target, and auto-expands folders.
  useEffect(() => {
    const SCROLL_THRESHOLD = 28;
    const SCROLL_SPEED = 8;

    const stopAutoScroll = () => {
      if (autoScrollTimerRef.current) {
        window.clearInterval(autoScrollTimerRef.current);
        autoScrollTimerRef.current = null;
      }
    };

    const startAutoScroll = (direction: 'up' | 'down') => {
      if (autoScrollTimerRef.current) return;
      autoScrollTimerRef.current = window.setInterval(() => {
        const tree = treeRef.current;
        const y = lastMouseYRef.current;
        if (!tree || y == null) {
          stopAutoScroll();
          return;
        }
        const rect = tree.getBoundingClientRect();
        const inZone =
          direction === 'up'
            ? y < rect.top + SCROLL_THRESHOLD
            : y > rect.bottom - SCROLL_THRESHOLD;
        if (!inZone || !dragActiveRef.current) {
          stopAutoScroll();
          return;
        }
        tree.scrollTop += direction === 'up' ? -SCROLL_SPEED : SCROLL_SPEED;
      }, 16);
    };

    const onMouseMove = (e: globalThis.MouseEvent) => {
      const candId = dragCandidateRef.current;
      const start = dragStartPosRef.current;
      if (!candId || !start) return;
      const note = noteMap.get(candId);
      if (!note) return;

      if (!dragActiveRef.current) {
        if (Math.hypot(e.clientX - start.x, e.clientY - start.y) < 6) return;
        dragActiveRef.current = true;
        setDraggingId(candId);
      }

      lastMouseYRef.current = e.clientY;
      setDragPos({ x: e.clientX, y: e.clientY, id: note.id, title: note.title });
      const target = resolveDropTarget(e.clientX, e.clientY);
      const valid = isValidDrop(target, candId);
      const nextTarget: typeof dropTarget =
        target.kind === 'root'
          ? { kind: 'root', valid }
          : { kind: target.kind, id: target.id, valid };
      dropTargetRef.current = nextTarget;
      setDropTarget(nextTarget);

      // Auto-scroll the tree when hovering near the top/bottom edges.
      const tree = treeRef.current;
      if (tree) {
        const rect = tree.getBoundingClientRect();
        if (e.clientY < rect.top + SCROLL_THRESHOLD) {
          startAutoScroll('up');
        } else if (e.clientY > rect.bottom - SCROLL_THRESHOLD) {
          startAutoScroll('down');
        } else {
          stopAutoScroll();
        }
      }

      // Auto-expand a collapsed folder after hovering for a short delay.
      if (target.kind === 'folder' && valid && !expanded.has(target.id)) {
        if (hoverExpandTimerRef.current) window.clearTimeout(hoverExpandTimerRef.current);
        hoverExpandTimerRef.current = window.setTimeout(() => {
          setExpanded((prev) => new Set(prev).add(target.id));
        }, 600);
      } else {
        if (hoverExpandTimerRef.current) {
          window.clearTimeout(hoverExpandTimerRef.current);
          hoverExpandTimerRef.current = null;
        }
      }
    };

    const onMouseUp = (e: globalThis.MouseEvent) => {
      const candId = dragCandidateRef.current;
      const wasActive = dragActiveRef.current;
      // Capture the last tracked drop target before clearing state.
      const finalTarget = dropTargetRef.current;
      dragCandidateRef.current = null;
      dragStartPosRef.current = null;
      dragActiveRef.current = false;
      setDragPos(null);
      setDraggingId(null);
      setDropTarget(null);
      dropTargetRef.current = null;
      if (hoverExpandTimerRef.current) {
        window.clearTimeout(hoverExpandTimerRef.current);
        hoverExpandTimerRef.current = null;
      }
      stopAutoScroll();
      // Only left-button release should commit a drop; ignore right/middle
      // mouseup so it doesn't accidentally act as a drop.
      if (e.button !== 0) return;
      if (!candId || !wasActive) return;

      // Use the last tracked drop target instead of re-hitting elementFromPoint,
      // because the drag indicator or a re-render can interfere on mouseup.
      const target = finalTarget ?? resolveDropTarget(e.clientX, e.clientY);
      if (!target || !isValidDrop(target, candId)) return;

      if (target.kind === 'folder') {
        onMoveToFolder?.(candId, target.id);
      } else if (target.kind === 'note') {
        // Dropping on a regular note places the dragged note in the same
        // directory as the target note (the target's parent folder, or root).
        const targetNote = noteMap.get(target.id);
        onMoveToFolder?.(candId, targetNote?.parent_id ?? null);
      } else {
        // Dropping on empty area moves the note to the root level.
        onMoveToFolder?.(candId, null);
      }
      setJustMovedId(candId);
      window.setTimeout(() => setJustMovedId(null), 900);
    };

    const onContextMenu = (e: globalThis.MouseEvent) => {
      // Suppress the browser context menu while a drag is in progress.
      if (dragActiveRef.current || draggingId) {
        e.preventDefault();
      }
    };

    window.addEventListener('mousemove', onMouseMove);
    window.addEventListener('mouseup', onMouseUp);
    window.addEventListener('contextmenu', onContextMenu);
    return () => {
      window.removeEventListener('mousemove', onMouseMove);
      window.removeEventListener('mouseup', onMouseUp);
      window.removeEventListener('contextmenu', onContextMenu);
      if (hoverExpandTimerRef.current) {
        window.clearTimeout(hoverExpandTimerRef.current);
      }
      stopAutoScroll();
    };
  }, [noteMap, resolveDropTarget, isValidDrop, onMoveToFolder, expanded, draggingId]);

  const handleMoveDialog = useCallback(
    (parentId: string | null) => {
      if (!moveTargetIds) return;
      if (moveTargetIds.length === 1) {
        onMoveToFolder?.(moveTargetIds[0], parentId);
      } else if (onBulkMove) {
        onBulkMove(moveTargetIds, parentId);
      }
      setMoveTargetIds(null);
      setSelectedIds(new Set());
    },
    [moveTargetIds, onMoveToFolder, onBulkMove]
  );

  const buildContextItems = (note: Note): ContextMenuItem[] => {
    const isSystem = note.is_system === 1;
    const items: ContextMenuItem[] = [
      {
        label: '新建子笔记',
        icon: <FilePlus size={12} />,
        onClick: () => onCreateSubNote(note.id),
      },
      {
        label: '新建文件夹',
        icon: <FolderPlus size={12} />,
        onClick: () => createFolderPending(note.id),
      },
    ];
    if (!isSystem) {
      items.push(
        {
          label: '重命名',
          icon: <MoreHorizontal size={12} />,
          onClick: () => startRename(note),
        },
        {
          label: '移动到...',
          icon: <Move size={12} />,
          onClick: () => setMoveTargetIds([note.id]),
        }
      );
      if (note.parent_id && onMoveToRoot) {
        items.push({
          label: '移出文件夹',
          icon: <ArrowUpToLine size={12} />,
          onClick: () => onMoveToRoot(note.id),
        });
      }
      if (onToggleFavorite) {
        items.push({
          label: note.is_favorite === 1 ? '取消收藏' : '收藏',
          icon: <Bookmark size={12} />,
          onClick: () => onToggleFavorite(note.id, note.is_favorite !== 1),
        });
      }
      items.push({
        label: '删除',
        destructive: true,
        icon: <Trash2 size={12} />,
        onClick: () => onDelete(note.id),
      });
    }
    return items;
  };

  const buildBulkContextItems = (): ContextMenuItem[] => {
    const count = selectedIds.size;
    const ids = selectedNotes.map((n) => n.id);
    const allFav = selectedNotes.every((n) => n.is_favorite === 1);
    const items: ContextMenuItem[] = [];

    if (onBulkCreateFolder) {
      items.push({
        label: `使用选中的 ${count} 个对象创建新文件夹`,
        icon: <FolderPlus size={12} />,
        onClick: async () => {
          await onBulkCreateFolder(ids);
          setSelectedIds(new Set());
        },
      });
    }
    if (onBulkMove) {
      items.push({
        label: `将 ${count} 个文件移动到...`,
        icon: <Move size={12} />,
        onClick: () => setMoveTargetIds(selectedNotes.map((n) => n.id)),
      });
    }
    if (onToggleFavorite) {
      items.push({
        label: allFav ? `取消收藏 ${count} 个` : `收藏 ${count} 个`,
        icon: <Bookmark size={12} />,
        onClick: () => {
          for (const n of selectedNotes) onToggleFavorite!(n.id, !allFav);
          setSelectedIds(new Set());
        },
      });
    }
    items.push({
      label: `删除 ${count} 个`,
      destructive: true,
      icon: <Trash2 size={12} />,
      onClick: () => {
        if (onBulkDelete) {
          // One confirmation covering the union of all subtrees.
          onBulkDelete(ids);
        } else {
          for (const n of selectedNotes) onDelete(n.id);
        }
        setSelectedIds(new Set());
      },
    });
    return items;
  };

  const renderNode = (note: Note, depth: number) => {
    if (!visibleIds.has(note.id)) return null;

    const children = childrenMap.get(note.id) || [];
    const hasChildren = children.length > 0;
    const isExpanded = expanded.has(note.id);
    const isActive = activeNoteId === note.id;
    const isSelected = selectedIds.has(note.id);
    const isRenaming = renamingId === note.id;
    const isFolder = note.is_folder === 1;
    const isSystem = note.is_system === 1;
    const isDropTarget = dropTarget?.kind !== 'root' && dropTarget?.id === note.id;
    const dropTargetInvalid = isDropTarget && !dropTarget!.valid;
    const isDraggingSource = draggingId === note.id;
    const isJustMoved = justMovedId === note.id;
    const isFlashing = flashIds.has(note.id);

    return (
      <div key={note.id}>
        <div
          ref={(el) => {
            if (el) itemRefs.current.set(note.id, el);
            else itemRefs.current.delete(note.id);
          }}
          data-note-id={note.id}
          onClick={(e) => handleNodeClick(e, note)}
          onDoubleClick={() => !isSystem && startRename(note)}
          onContextMenu={(e) => openContextMenu(e as unknown as MouseEvent, note)}
          onMouseDown={(e) => !isSystem && handleNodeMouseDown(e, note)}
          className={`flex items-center gap-1 h-7 px-3 text-[13px] cursor-pointer group select-none transition-opacity ${
            isDropTarget
              ? dropTargetInvalid
                ? 'bg-red-500/10 ring-1 ring-inset ring-red-500/40'
                : dropTarget!.kind === 'folder'
                  ? 'bg-primary/10 ring-1 ring-inset ring-primary/50'
                  : 'bg-surface-hover ring-1 ring-inset ring-primary/30'
              : isActive && isSelected
                ? 'bg-primary/20 text-text-primary'
                : isSelected
                  ? 'bg-primary/10 text-text-primary'
                  : isActive
                    ? 'bg-[#353842] text-text-primary'
                    : 'text-text-secondary hover:bg-surface-hover'
          } ${isDraggingSource ? 'opacity-40' : ''} ${isJustMoved ? 'flash-success' : ''} ${isFlashing ? 'flash-match' : ''}`}
          style={{ paddingLeft: `${12 + depth * 16}px` }}
        >
          {isFolder || hasChildren ? (
            <button
              onClick={(e) => { e.stopPropagation(); toggleExpand(note.id); }}
              className="shrink-0 p-0.5 rounded hover:bg-surface-hover/60 text-text-secondary/60"
            >
              <ChevronRight
                size={14}
                className={`transition-transform duration-150 ${isExpanded ? 'rotate-90' : ''}`}
              />
            </button>
          ) : (
            <span className="w-[22px] shrink-0" />
          )}

          {!isFolder &&
            (hasChildren ? (
              <Folder size={14} className="shrink-0 text-text-secondary/70" />
            ) : (
              <FileText
                size={14}
                className={`shrink-0 ${note.is_excerpt === 1 ? 'text-primary' : 'text-text-secondary/60'}`}
              />
            ))}

          {isRenaming ? (
            <input
              autoFocus
              value={renameValue}
              onChange={(e) => setRenameValue(e.target.value)}
              onBlur={commitRename}
              onKeyDown={(e) => {
                if (e.key === 'Enter') commitRename();
                if (e.key === 'Escape') cancelRename();
                e.stopPropagation();
              }}
              onClick={(e) => e.stopPropagation()}
              // Keep drag-to-reorder from arming while editing the name, and
              // re-enable text selection inside the input (the row is
              // select-none).
              onMouseDown={(e) => e.stopPropagation()}
              className="flex-1 min-w-0 bg-background text-text-primary text-[13px] px-1 py-0.5 rounded border border-primary/30 focus:outline-none select-text"
            />
          ) : (
            <span className="flex-1 truncate flex items-center gap-1.5">
              {note.title || 'Untitled'}
              {note.is_excerpt === 1 && (
                <span className="shrink-0 text-[9px] px-1 py-px rounded bg-primary/15 text-primary leading-none">
                  摘录
                </span>
              )}
              {isSystem && (
                <span className="shrink-0 text-[9px] px-1 py-px rounded bg-primary/15 text-primary leading-none">
                  系统
                </span>
              )}
            </span>
          )}

        </div>

        {hasChildren && isExpanded && (
          <div>
            {children.map((child) => renderNode(child, depth + 1))}
          </div>
        )}
      </div>
    );
  };





  // Auto-reveal the active note in the tree (expand parents + scroll into view).
  useEffect(() => {
    if (!activeNoteId) return;
    setExpanded((prev) => {
      const next = new Set(prev);
      let id = activeNoteId;
      while (true) {
        const n = noteMap.get(id);
        if (!n?.parent_id) break;
        next.add(n.parent_id);
        id = n.parent_id;
      }
      return next;
    });
    requestAnimationFrame(() => {
      const el = itemRefs.current.get(activeNoteId);
      if (el) {
        el.scrollIntoView({ behavior: 'smooth', block: 'center' });
      }
    });
  }, [activeNoteId, noteMap]);

  // Auto-expand parents of search / tag / ai-filter matches and flash the hits.
  useEffect(() => {
    const hasFilter = search.trim() || tagFilter || aiOnly;
    if (!hasFilter) {
      setFlashIds(new Set());
      return;
    }
    setExpanded((prev) => {
      const next = new Set(prev);
      for (const id of visibleIds) {
        const n = noteMap.get(id);
        if (n?.parent_id) next.add(n.parent_id);
      }
      return next;
    });
    setFlashIds(new Set(visibleIds));
    const timer = window.setTimeout(() => setFlashIds(new Set()), 1800);
    // Scroll the first visible match into view after expansion renders.
    requestAnimationFrame(() => {
      const first = visibleIds.values().next().value;
      if (first) {
        const el = itemRefs.current.get(first);
        if (el) el.scrollIntoView({ behavior: 'smooth', block: 'center' });
      }
    });
    return () => window.clearTimeout(timer);
  }, [search, tagFilter, aiOnly, visibleIds, noteMap]);

  const rootNotes = childrenMap.get(ROOT_KEY) || [];

  return (
    <div className="flex flex-col h-full bg-surface">
      {/* Toolbar */}
      <div className="flex items-center justify-between px-2.5 py-2 border-b border-surface-hover">
        <span className="text-xs font-semibold text-text-secondary uppercase tracking-wider">{title}</span>
        <div className="flex items-center gap-0.5">
          <button
            onClick={() => {
              let parentId: string | undefined;
              if (selectedIds.size === 1) {
                const id = selectedIds.values().next().value as string | undefined;
                const note = id ? noteMap.get(id) : undefined;
                if (note) {
                  parentId = note.is_folder === 1 ? note.id : (note.parent_id ?? undefined);
                }
              }
              onCreate(parentId);
            }}
            className="w-6 h-6 rounded flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
            title="新建笔记"
          >
            <Plus size={14} />
          </button>
          <button
            onClick={() => createFolderPending()}
            className="w-6 h-6 rounded flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
            title="新建文件夹"
          >
            <FolderPlus size={14} />
          </button>
          <button
            onClick={() => setSortEnabled((s) => !s)}
            className={`w-6 h-6 rounded flex items-center justify-center transition-colors ${sortEnabled ? 'text-primary bg-primary/10' : 'text-text-secondary hover:text-text-primary hover:bg-surface-hover'}`}
            title="排序"
          >
            <ArrowUpToLine size={14} />
          </button>
          <button
            onClick={() => (allFoldersExpanded ? collapseAll() : expandAll())}
            className="w-6 h-6 rounded flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
            title={allFoldersExpanded ? '折叠全部' : '展开全部'}
          >
            {allFoldersExpanded ? (
              <ChevronsDownUp size={16} />
            ) : (
              <ChevronsUpDown size={16} />
            )}
          </button>
          <button
            onClick={revealActiveNote}
            className="w-6 h-6 rounded flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
            title="定位到当前笔记"
          >
            <Crosshair size={14} />
          </button>

          {onClose && (
            <button
              onClick={onClose}
              className="w-6 h-6 rounded flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
              title="关闭侧边栏"
            >
              <X size={14} />
            </button>
          )}
        </div>
      </div>

      {/* Search */}
      <div className="px-2.5 py-2">
        <div className="relative">
          <Search size={12} className="absolute left-2 top-1/2 -translate-y-1/2 text-text-secondary/50" />
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="搜索..."
            className="w-full bg-background text-text-primary text-xs pl-7 pr-2 py-1.5 rounded border border-surface-hover focus:border-primary/30 focus:outline-none placeholder:text-text-secondary/40"
          />
        </div>
        <div className="flex flex-wrap gap-1 mt-1.5">
          <button
            onClick={() => setAiOnly((v) => !v)}
            className={`px-1.5 py-0.5 rounded text-[10px] transition-colors ${
              aiOnly
                ? 'bg-primary/15 text-primary'
                : 'text-text-secondary/60 hover:bg-surface-hover hover:text-text-primary'
            }`}
            title="只显示被 AI 整理过的笔记"
          >
            AI 整理
          </button>
          {allTags.map((t) => (
              <button
                key={t}
                onClick={() => setTagFilter(tagFilter === t ? null : t)}
                className={`px-1.5 py-0.5 rounded text-[10px] transition-colors ${
                  tagFilter === t
                    ? 'bg-primary/15 text-primary'
                    : 'bg-surface-hover text-text-secondary hover:text-text-primary'
                }`}
              >
                #{t}
              </button>
            ))}
          </div>
      </div>

      {/* Tree / search results */}
      <div
        ref={treeRef}
        className={`flex-1 overflow-y-auto py-1.5 ${
          dropTarget?.kind === 'root'
            ? dropTarget.valid
              ? 'ring-2 ring-inset ring-primary/20 bg-primary/5'
              : 'ring-2 ring-inset ring-red-500/20 bg-red-500/5'
            : ''
        }`}
      >
        {searching ? (
          <p className="text-xs text-text-secondary/50 text-center py-6">搜索中…</p>
        ) : searchResults !== null ? (
          searchResults.length === 0 ? (
            <p className="text-xs text-text-secondary/50 text-center py-6">无结果</p>
          ) : (
            <div className="space-y-0.5 px-1.5">
              {searchResults.map((r) => (
                <button
                  key={r.id}
                  onClick={() => onSelect(r.id)}
                  className={`w-full text-left px-2 py-1.5 rounded transition-colors ${
                    activeNoteId === r.id
                      ? 'bg-[#353842] text-text-primary'
                      : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
                  }`}
                >
                  <div className="text-[12px] truncate">{r.title || 'Untitled'}</div>
                  {r.snippet && (
                    <div className="text-[11px] text-text-secondary/60 truncate mt-0.5">{r.snippet}</div>
                  )}
                </button>
              ))}
            </div>
          )
        ) : rootNotes.length === 0 ? (
          <p className="text-xs text-text-secondary/50 text-center py-6">暂无笔记</p>
        ) : (
          rootNotes.map((note) => renderNode(note, 0))
        )}
      </div>

      {/* Footer: vault switcher + help + settings */}
      <div className="h-8 border-t border-surface-hover flex items-center justify-between px-2.5 text-xs text-text-secondary shrink-0">
        <button
          onClick={onOpenVault}
          className="flex-1 min-w-0 h-full flex items-center gap-1.5 text-left cursor-pointer hover:text-text-primary hover:bg-surface-hover transition-colors"
          title="打开库切换器"
        >
          <Database size={12} className="shrink-0" />
          <span className="truncate">{currentVaultName}</span>
        </button>
        <div className="flex items-center gap-0.5 shrink-0">
          {onOpenHelp && (
            <button
              onClick={onOpenHelp}
              className="w-5 h-5 rounded flex items-center justify-center hover:text-text-primary hover:bg-surface-hover/60 transition-colors"
              title="帮助"
            >
              <HelpCircle size={12} />
            </button>
          )}
          {onOpenSettings && (
            <button
              onClick={onOpenSettings}
              className="w-5 h-5 rounded flex items-center justify-center hover:text-text-primary hover:bg-surface-hover/60 transition-colors"
              title="设置"
            >
              <Settings size={12} />
            </button>
          )}
        </div>
      </div>

      {/* Context menu */}
      {contextMenu && (
        <ContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          items={contextMenu.bulk ? buildBulkContextItems() : contextNote ? buildContextItems(contextNote) : []}
          onClose={() => setContextMenu(null)}
        />
      )}

      {/* Drag indicator (pointer-based drag) */}
      {dragPos && (
        <div
          data-drag-indicator
          className="fixed z-[300] pointer-events-none"
          style={{ left: dragPos.x + 14, top: dragPos.y - 10 }}
        >
          <div className="flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg bg-surface border border-primary/40 shadow-xl text-[12px] text-text-primary">
            <FileText size={12} className="text-primary/80 shrink-0" />
            <span className="truncate max-w-[200px]">{dragPos.title}</span>
          </div>
        </div>
      )}

      {/* Move-to-folder dialog */}
      {moveTargetIds && (
        <MoveNoteDialog
          notes={notes}
          noteIds={moveTargetIds}
          currentParentId={noteMap.get(moveTargetIds[0])?.parent_id ?? null}
          onMove={handleMoveDialog}
          onClose={() => setMoveTargetIds(null)}
        />
      )}
    </div>
  );
}
