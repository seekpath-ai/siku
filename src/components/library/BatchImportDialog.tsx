import { CheckCircle2, FileStack, Loader2, MinusCircle, X, XCircle } from 'lucide-react';
import { useBatchImportStore } from '@/stores/batchImportStore';

/** Progress dialog for batch imports (multi-select files / folder / Zotero).
 *  Driven entirely by useBatchImportStore; the backend streams per-item
 *  progress events. Stays open after completion so the user can review
 *  failures — closing mid-run requires cancelling first. */
export function BatchImportDialog() {
  const s = useBatchImportStore();
  if (!s.open) return null;

  const pct = s.total > 0 ? Math.round((s.current / s.total) * 100) : 0;

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div
        className="absolute inset-0 bg-black/50 backdrop-blur-sm"
        onClick={() => { if (!s.running) s.close(); }}
      />
      <div className="relative w-full max-w-md mx-4 bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-3 px-4 py-3 border-b border-surface-hover">
          <FileStack size={18} className="text-primary" />
          <span className="text-sm font-medium text-text-primary">{s.label}</span>
          {!s.running && (
            <button
              onClick={s.close}
              className="ml-auto p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            >
              <X size={14} />
            </button>
          )}
        </div>

        <div className="px-4 py-4 space-y-3">
          {/* Progress bar */}
          <div className="h-1.5 rounded-full bg-surface-hover overflow-hidden">
            <div
              className="h-full bg-primary transition-all duration-200"
              style={{ width: `${pct}%` }}
            />
          </div>
          <div className="flex items-center justify-between text-xs text-text-secondary">
            <span className="truncate max-w-[70%]" title={s.file}>
              {s.running ? `正在导入：${s.file}` : s.cancelled ? '已取消' : '完成'}
            </span>
            <span className="shrink-0 tabular-nums">
              {s.current} / {s.total}
            </span>
          </div>

          {/* Counts */}
          <div className="flex items-center gap-4 text-xs">
            <span className="flex items-center gap-1 text-emerald-400">
              <CheckCircle2 size={13} /> {s.imported} 成功
            </span>
            <span className="flex items-center gap-1 text-text-secondary">
              <MinusCircle size={13} /> {s.skipped} 跳过（已存在）
            </span>
            <span className="flex items-center gap-1 text-red-400">
              <XCircle size={13} /> {s.failed} 失败
            </span>
            {s.running && <Loader2 size={13} className="animate-spin text-primary ml-auto" />}
          </div>

          {/* Failures, once finished */}
          {s.done && s.errors.length > 0 && (
            <div className="max-h-40 overflow-y-auto rounded-lg border border-surface-hover bg-background/40 px-3 py-2">
              {s.errors.map((e, i) => (
                <div key={i} className="text-[11px] leading-5 text-red-400/90 break-all">
                  {e}
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="flex justify-end gap-2 px-4 py-3 border-t border-surface-hover">
          {s.running ? (
            <button
              onClick={s.cancel}
              className="px-3 py-1.5 rounded-lg text-xs text-red-400 hover:bg-red-500/10 border border-surface-hover transition-colors"
            >
              取消（当前篇会完成）
            </button>
          ) : (
            <button
              onClick={s.close}
              className="px-3 py-1.5 rounded-lg text-xs bg-primary/15 text-primary hover:bg-primary/25 transition-colors"
            >
              关闭
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
