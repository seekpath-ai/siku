import { useMemo, useEffect, useRef } from 'react';
import { X, Folder, ArrowUpToLine } from 'lucide-react';
import type { Note } from '@/lib/types';

interface Props {
  notes: Note[];
  /** The note(s) being moved. */
  noteIds: string[];
  /** Current parent of the first note (for display). */
  currentParentId: string | null;
  onMove: (parentId: string | null) => void;
  onClose: () => void;
}

/** Modal listing all folders to move note(s) into (Obsidian-style). */
export function MoveNoteDialog({ notes, noteIds, currentParentId, onMove, onClose }: Props) {
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (panelRef.current && !panelRef.current.contains(e.target as Node)) onClose();
    };
    window.addEventListener('mousedown', onDown);
    return () => window.removeEventListener('mousedown', onDown);
  }, [onClose]);

  const folders = useMemo(() => notes.filter((n) => n.is_folder === 1), [notes]);
  const noteMap = useMemo(() => new Map(notes.map((n) => [n.id, n])), [notes]);

  const childrenMap = useMemo(() => {
    const map = new Map<string, Note[]>();
    for (const f of folders) {
      const pid = f.parent_id ?? '';
      if (!map.has(pid)) map.set(pid, []);
      map.get(pid)!.push(f);
    }
    return map;
  }, [folders]);

  // True when `targetId` is any selected note or one of their descendants —
  // moving a folder into its own subtree would create a cycle.
  const isInSubtree = (targetId: string): boolean => {
    const selectedSet = new Set(noteIds);
    let cur: string | null = targetId;
    let hops = 0;
    while (cur && hops < 100) {
      if (selectedSet.has(cur)) return true;
      const n = noteMap.get(cur);
      cur = n?.parent_id ?? null;
      hops += 1;
    }
    return false;
  };

  const renderFolder = (folder: Note, depth: number) => {
    const isCurrent = currentParentId === folder.id;
    const disabled = isInSubtree(folder.id);
    return (
      <div key={folder.id}>
        <button
          disabled={disabled}
          onClick={() => onMove(folder.id)}
          className={`w-full flex items-center gap-2 px-3 py-1.5 text-[13px] text-left transition-colors ${
            isCurrent
              ? 'bg-primary/10 text-text-primary'
              : disabled
                ? 'text-text-secondary/35 cursor-not-allowed'
                : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
          }`}
          style={{ paddingLeft: `${12 + depth * 16}px` }}
          title={disabled ? '不能移动到自身或其子目录' : `移动到「${folder.title}」`}
        >
          <Folder size={14} className="shrink-0 text-text-secondary/60" />
          <span className="flex-1 truncate">{folder.title || '未命名'}</span>
          {isCurrent && <span className="text-[10px] text-primary shrink-0">当前所在</span>}
        </button>
        {(childrenMap.get(folder.id) || []).map((child) => renderFolder(child, depth + 1))}
      </div>
    );
  };

  const rootFolders = childrenMap.get('') || [];

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div
        ref={panelRef}
        className="relative w-[340px] max-h-[70vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden"
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-surface-hover">
          <span className="text-sm font-medium text-text-primary">
            {noteIds.length > 1 ? `将 ${noteIds.length} 个对象移动到` : '将文件移动到'}
          </span>
          <button
            onClick={onClose}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭"
          >
            <X size={14} />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto py-1">
          <button
            onClick={() => onMove(null)}
            className={`w-full flex items-center gap-2 px-3 py-1.5 text-[13px] text-left transition-colors ${
              currentParentId === null
                ? 'bg-primary/10 text-text-primary'
                : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
            }`}
          >
            <ArrowUpToLine size={14} className="shrink-0 text-text-secondary/60" />
            <span className="flex-1 truncate">（根目录）</span>
            {currentParentId === null && <span className="text-[10px] text-primary shrink-0">当前所在</span>}
          </button>
          {rootFolders.length === 0 && (
            <p className="text-xs text-text-secondary/60 text-center py-6">暂无目录</p>
          )}
          {rootFolders.map((folder) => renderFolder(folder, 0))}
        </div>
      </div>
    </div>
  );
}
