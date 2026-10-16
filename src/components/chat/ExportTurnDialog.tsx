import { useEffect, useState } from 'react';
import { X, Download } from 'lucide-react';

/** Pre-export options for one turn: the snapshot checkbox replaces a
 * confirm dialog whose 确认/取消 read as "export / don't export". Here
 * 取消 unambiguously aborts; 导出 proceeds with the chosen options. */
export function ExportTurnDialog({
  onConfirm,
  onClose,
}: {
  /** includeContext = the snapshot checkbox. */
  onConfirm: (includeContext: boolean) => void;
  onClose: () => void;
}) {
  const [includeContext, setIncludeContext] = useState(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div className="fixed inset-0 z-[6000] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div className="relative w-full max-w-sm mx-4 bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-2 px-4 py-3 border-b border-surface-hover">
          <Download size={14} className="text-primary" />
          <span className="text-sm font-medium text-text-primary">导出本轮</span>
          <button
            onClick={onClose}
            className="ml-auto p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
          >
            <X size={14} />
          </button>
        </div>

        <div className="px-4 py-3 space-y-3">
          <p className="text-xs text-text-secondary leading-relaxed">
            将本轮的用户消息、推理过程、助手回复导出为 Markdown 文件。
          </p>
          <label className="flex items-start gap-2 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={includeContext}
              onChange={(e) => setIncludeContext(e.target.checked)}
              className="mt-0.5 accent-[var(--color-primary,#7c9aff)]"
            />
            <span>
              <span className="block text-[13px] text-text-primary">附带本轮上下文快照</span>
              <span className="block text-[11px] text-text-secondary/60 mt-0.5">
                含系统提示词与历史消息，可能包含敏感信息，一般仅排障时需要
              </span>
            </span>
          </label>
        </div>

        <div className="flex justify-end gap-2 px-4 py-3 border-t border-surface-hover">
          <button
            onClick={onClose}
            className="px-3 py-1.5 rounded-lg border border-surface-hover text-[13px] text-text-secondary hover:bg-surface-hover"
          >
            取消
          </button>
          <button
            onClick={() => onConfirm(includeContext)}
            className="px-3 py-1.5 rounded-lg bg-primary text-black text-[13px] font-medium hover:bg-primary/90"
          >
            导出
          </button>
        </div>
      </div>
    </div>
  );
}
