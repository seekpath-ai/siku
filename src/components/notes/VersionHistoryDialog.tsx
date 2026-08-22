import { useEffect, useMemo, useRef, useState } from 'react';
import { X, History, RotateCcw, Bot } from 'lucide-react';
import { MergeView } from '@codemirror/merge';
import { EditorState } from '@codemirror/state';
import { EditorView, lineNumbers } from '@codemirror/view';
import { oneDark } from '@codemirror/theme-one-dark';
import { useDialog } from '@/hooks/useDialog';
import { noteVersionsList } from '@/lib/tauri';
import { diffLines } from '@/lib/diff';
import type { Note, NoteVersion } from '@/lib/types';

interface Props {
  note: Note;
  /** Restores the given version, then refreshes the editor (caller's job). */
  onRestore: (versionId: string) => Promise<void>;
  onClose: () => void;
}

function formatTime(iso: string): string {
  try {
    return new Date(iso).toLocaleString('zh-CN', {
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
    });
  } catch {
    return iso;
  }
}

const READONLY = [
  lineNumbers(),
  oneDark,
  EditorState.readOnly.of(true),
  EditorView.editable.of(false),
  EditorView.lineWrapping,
];

/** VSCode-style version history: narrow version list + CodeMirror merge diff
 *  (synchronized scrolling, character-level highlight, fold, gutter markers). */
export function VersionHistoryDialog({ note, onRestore, onClose }: Props) {
  const { confirm } = useDialog();
  const [versions, setVersions] = useState<NoteVersion[] | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [restoring, setRestoring] = useState(false);
  const mergeRef = useRef<HTMLDivElement>(null);
  const mvRef = useRef<MergeView | null>(null);

  useEffect(() => {
    noteVersionsList(note.id)
      .then(setVersions)
      .catch(() => setVersions([]));
  }, [note.id]);

  useEffect(() => {
    const onDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onDown);
    return () => window.removeEventListener('keydown', onDown);
  }, [onClose]);

  const selected = versions?.find((v) => v.id === selectedId) ?? null;

  // Line-count summary (independent of the merge view's own diff).
  const summary = useMemo(() => {
    if (!selected) return null;
    const d = diffLines(note.content, selected.content);
    return { added: d.added, removed: d.removed };
  }, [selected, note.content]);

  // Build the CodeMirror merge view (left = current, right = selected version).
  useEffect(() => {
    if (mvRef.current) {
      mvRef.current.destroy();
      mvRef.current = null;
    }
    if (!selected || !mergeRef.current) return;

    const mv = new MergeView({
      a: { doc: note.content, extensions: READONLY },
      b: { doc: selected.content, extensions: READONLY },
      parent: mergeRef.current,
    });
    mvRef.current = mv;

    return () => {
      mv.destroy();
      mvRef.current = null;
    };
  }, [selected, note.content]);

  const handleRestore = async () => {
    if (!selected || restoring) return;
    const ok = await confirm('将把笔记恢复到该版本（覆盖当前内容）。当前内容会自动保存为新版本，可随时回滚。');
    if (!ok) return;
    setRestoring(true);
    try {
      await onRestore(selected.id);
    } finally {
      setRestoring(false);
    }
  };

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div className="relative w-[960px] max-w-[94vw] h-[560px] max-h-[82vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-2.5 border-b border-surface-hover shrink-0">
          <span className="text-sm font-medium text-text-primary flex items-center gap-1.5">
            <History size={14} className="text-primary" />
            版本历史
          </span>
          <button
            onClick={onClose}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭"
          >
            <X size={14} />
          </button>
        </div>

        {versions === null ? (
          <div className="flex-1 flex items-center justify-center text-xs text-text-secondary/60">加载中...</div>
        ) : versions.length === 0 ? (
          <div className="flex-1 flex flex-col items-center justify-center text-xs text-text-secondary/60 gap-2">
            <History size={28} className="opacity-30" />
            暂无版本记录
          </div>
        ) : (
          <div className="flex flex-1 min-h-0">
            {/* Left: version list (narrow) */}
            <div className="w-[200px] shrink-0 overflow-y-auto py-1 border-r border-surface-hover">
              {versions.map((v) => (
                <button
                  key={v.id}
                  onClick={() => setSelectedId(v.id)}
                  className={`w-full text-left px-3 py-2 transition-colors ${
                    selectedId === v.id
                      ? 'bg-primary/10 text-text-primary'
                      : 'text-text-secondary hover:bg-surface-hover'
                  }`}
                >
                  <div className="flex items-center gap-1 text-[11px] text-text-secondary/70 mb-0.5">
                    {v.edited_by === 'agent' && <Bot size={10} className="text-primary" />}
                    {v.edited_by === 'restore' && <RotateCcw size={10} className="text-text-secondary/60" />}
                    <span>{formatTime(v.created_at)}</span>
                  </div>
                  <div className="text-[12px] truncate">{v.title || '未命名'}</div>
                </button>
              ))}
            </div>

            {/* Right: CodeMirror merge diff */}
            <div className="flex-1 min-w-0 flex flex-col">
              <div className="flex border-b border-surface-hover shrink-0">
                <div className="flex-1 px-3 py-2 text-[11px] text-text-secondary/70 font-medium">当前版本</div>
                <div className="flex-1 px-3 py-2 text-[11px] text-text-secondary/70 font-medium border-l border-surface-hover">
                  {selected ? `版本 · ${formatTime(selected.created_at)}` : '版本对比'}
                </div>
              </div>
              <div className="flex-1 min-h-0 overflow-hidden">
                {selected ? (
                  <div ref={mergeRef} className="h-full" />
                ) : (
                  <div className="h-full flex items-center justify-center text-xs text-text-secondary/50">
                    在左侧选择一个版本查看差异
                  </div>
                )}
              </div>
            </div>
          </div>
        )}

        {/* Footer: diff summary + restore */}
        <div className="px-4 py-2.5 border-t border-surface-hover flex items-center justify-between shrink-0">
          <div className="text-[11px] text-text-secondary/70 flex items-center gap-3">
            {summary ? (
              <>
                <span className="flex items-center gap-1">
                  <span className="w-2 h-2 rounded-sm bg-green-500/60" /> 新增 {summary.added}
                </span>
                <span className="flex items-center gap-1">
                  <span className="w-2 h-2 rounded-sm bg-red-500/60" /> 删除 {summary.removed}
                </span>
              </>
            ) : (
              <span>选择版本查看差异</span>
            )}
          </div>
          <button
            onClick={handleRestore}
            disabled={!selected || restoring}
            className="flex items-center gap-1.5 px-3.5 py-1.5 rounded-lg bg-primary/10 text-primary text-xs font-medium hover:bg-primary/20 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            <RotateCcw size={12} />
            {restoring ? '恢复中...' : '恢复到该版本'}
          </button>
        </div>
      </div>
    </div>
  );
}
