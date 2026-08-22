import { useState, useEffect } from 'react';
import { X, Loader2, ScanSearch } from 'lucide-react';

interface RegionRangeDialogProps {
  isOpen: boolean;
  onClose: () => void;
  totalPages: number;
  currentPage: number;
  onDetect: (startPage: number, endPage: number) => Promise<void>;
  detecting?: boolean;
  progress?: string;
}

export function RegionRangeDialog({
  isOpen,
  onClose,
  totalPages,
  currentPage,
  onDetect,
  detecting = false,
  progress = '',
}: RegionRangeDialogProps) {
  const [startPage, setStartPage] = useState(currentPage);
  const [endPage, setEndPage] = useState(currentPage);
  const [error, setError] = useState<string | null>(null);

  // Reset values when dialog opens
  useEffect(() => {
    if (isOpen) {
      setStartPage(currentPage);
      setEndPage(currentPage);
      setError(null);
    }
  }, [isOpen, currentPage]);

  if (!isOpen) return null;

  const clamp = (v: number) => Math.max(1, Math.min(totalPages, v));

  const handleDetect = async () => {
    const s = clamp(startPage);
    const e = clamp(endPage);
    if (s > e) {
      setError('起始页不能大于结束页');
      setTimeout(() => setError(null), 3000);
      return;
    }
    setError(null);
    await onDetect(s, e);
  };

  return (
    <div
      className="fixed inset-0 bg-black/40 z-50 flex items-center justify-center"
      onClick={(e) => { if (e.target === e.currentTarget && !detecting) onClose(); }}
    >
      <div className="bg-background border border-surface-hover rounded-lg shadow-2xl w-full max-w-sm mx-4">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-surface-hover">
          <span className="text-sm font-medium text-text-primary">检测结构区域</span>
          <button
            onClick={onClose}
            disabled={detecting}
            className="p-1 rounded hover:bg-surface-hover text-text-secondary transition-colors disabled:opacity-40"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="px-4 py-4 space-y-4">
          <p className="text-xs text-text-secondary leading-relaxed">
            选择要检测结构区域的页面范围。检测完成后，当前页的区域将直接显示在 PDF 上。
          </p>

          {/* Page range inputs */}
          <div className="flex items-center gap-3">
            <div className="flex items-center gap-1.5">
              <label className="text-xs text-text-secondary">起始页</label>
              <input
                type="number"
                min={1}
                max={totalPages}
                value={startPage}
                disabled={detecting}
                onChange={(e) => setStartPage(clamp(parseInt(e.target.value) || 1))}
                className="w-16 bg-surface border border-surface-hover rounded px-2 py-1 text-xs text-text-primary text-center disabled:opacity-50"
              />
            </div>
            <span className="text-text-secondary/40">—</span>
            <div className="flex items-center gap-1.5">
              <label className="text-xs text-text-secondary">结束页</label>
              <input
                type="number"
                min={1}
                max={totalPages}
                value={endPage}
                disabled={detecting}
                onChange={(e) => setEndPage(clamp(parseInt(e.target.value) || 1))}
                className="w-16 bg-surface border border-surface-hover rounded px-2 py-1 text-xs text-text-primary text-center disabled:opacity-50"
              />
            </div>
            <span className="text-[10px] text-text-secondary/50">/ {totalPages} 页</span>
          </div>

          {/* Quick select */}
          <div className="flex gap-2">
            <button
              onClick={() => { setStartPage(currentPage); setEndPage(currentPage); }}
              disabled={detecting}
              className="px-2 py-0.5 rounded text-[10px] text-text-secondary hover:bg-surface-hover border border-surface-hover transition-colors disabled:opacity-50"
            >
              当前页
            </button>
            <button
              onClick={() => { setStartPage(1); setEndPage(totalPages); }}
              disabled={detecting}
              className="px-2 py-0.5 rounded text-[10px] text-text-secondary hover:bg-surface-hover border border-surface-hover transition-colors disabled:opacity-50"
            >
              全部
            </button>
          </div>

          {/* Detect button */}
          <button
            onClick={handleDetect}
            disabled={detecting}
            className="flex items-center justify-center gap-2 px-3 py-1.5 rounded text-xs bg-primary/10 text-primary hover:bg-primary/20 transition-colors disabled:opacity-50 w-full"
          >
            {detecting ? (
              <><Loader2 size={13} className="animate-spin" />{progress || '检测中...'}</>
            ) : (
              <><ScanSearch size={13} />开始检测</>
            )}
          </button>

          {/* Error */}
          {error && (
            <p className="text-xs text-red-400 text-center">{error}</p>
          )}
        </div>
      </div>
    </div>
  );
}
