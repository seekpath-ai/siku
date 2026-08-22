import { useState } from 'react';
import { X, Upload, Check } from 'lucide-react';
import type { DetectedRegion, RegionType } from './regions';

const VALID_REGION_TYPES: RegionType[] = [
  'title', 'authors', 'abstract', 'body', 'heading',
  'figure', 'table', 'equation', 'references', 'unknown',
];

interface ImportRegionsDialogProps {
  isOpen: boolean;
  onClose: () => void;
  totalPages: number;
  currentPage: number;
  /** Called for each page group. pageIndex is auto-detected from JSON or falls back to manual selection. */
  onImport: (regions: DetectedRegion[], pageIndex: number) => void;
}

export function ImportRegionsDialog({
  isOpen,
  onClose,
  totalPages,
  currentPage,
  onImport,
}: ImportRegionsDialogProps) {
  const [importJson, setImportJson] = useState('');
  const [importPage, setImportPage] = useState(currentPage);
  const [autoMode, setAutoMode] = useState(false);
  const [autoPages, setAutoPages] = useState<number[]>([]);
  const [imported, setImported] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!isOpen) return null;

  const clamp = (v: number) => Math.max(1, Math.min(totalPages, v));

  const parseRegion = (obj: unknown, idx: number): DetectedRegion | null => {
    if (typeof obj !== 'object' || obj === null) {
      setError(`第 ${idx + 1} 个区域不是有效对象`);
      return null;
    }
    const record = obj as Record<string, unknown>;
    if (typeof record.type !== 'string' || !VALID_REGION_TYPES.includes(record.type as RegionType)) {
      setError(`第 ${idx + 1} 个区域缺少有效的 type 字段，收到: "${record.type}"`);
      return null;
    }
    if (typeof record.yRatio !== 'number' || typeof record.xRatio !== 'number' ||
        typeof record.heightRatio !== 'number' || typeof record.widthRatio !== 'number') {
      setError(`第 ${idx + 1} 个区域缺少坐标字段`);
      return null;
    }
    const pageIndex = typeof record.page === 'number' && record.page >= 1 ? record.page : importPage;
    return {
      id: `${record.type}-import-${pageIndex}-${idx}`,
      type: record.type as RegionType,
      pageIndex,
      yRatio: Math.max(0, Math.min(1, record.yRatio)),
      xRatio: Math.max(0, Math.min(1, record.xRatio)),
      heightRatio: Math.max(0, Math.min(1, record.heightRatio)),
      widthRatio: Math.max(0, Math.min(1, record.widthRatio)),
      text: typeof record.text === 'string' ? record.text : '',
      confidence: typeof record.confidence === 'number' ? record.confidence : 0.9,
    };
  };

  const handleImport = () => {
    setError(null);
    setImported(false);
    setAutoMode(false);

    let raw: unknown[];
    try {
      raw = JSON.parse(importJson) as unknown[];
    } catch {
      setError('JSON 格式无效，请检查粘贴内容');
      setTimeout(() => setError(null), 3000);
      return;
    }

    if (!Array.isArray(raw) || raw.length === 0) {
      setError('需要是包含至少一个区域对象的 JSON 数组');
      setTimeout(() => setError(null), 3000);
      return;
    }

    // Check if regions have page field → auto-distribute
    const hasPageField = raw.some(
      (obj) => typeof obj === 'object' && obj !== null && typeof (obj as Record<string, unknown>).page === 'number',
    );
    if (hasPageField) {
      // Group by page
      const pageMap = new Map<number, DetectedRegion[]>();
      for (let i = 0; i < raw.length; i++) {
        const r = parseRegion(raw[i], i);
        if (!r) return;
        const list = pageMap.get(r.pageIndex) || [];
        list.push(r);
        pageMap.set(r.pageIndex, list);
      }
      // Import each page group
      const pages = Array.from(pageMap.keys()).sort((a, b) => a - b);
      for (const p of pages) {
        onImport(pageMap.get(p)!, p);
      }
      setAutoMode(true);
      setAutoPages(pages);
      setImported(true);
      setTimeout(() => setImported(false), 3000);
      return;
    }

    // No page field → fall back to manual page selection
    const regions: DetectedRegion[] = [];
    for (let i = 0; i < raw.length; i++) {
      const r = parseRegion(raw[i], i);
      if (!r) return;
      regions.push(r);
    }
    onImport(regions, importPage);
    setImported(true);
    setTimeout(() => setImported(false), 3000);
  };

  const handleClose = () => {
    setImportJson('');
    setError(null);
    setImported(false);
    setAutoMode(false);
    setAutoPages([]);
    onClose();
  };

  return (
    <div
      className="fixed inset-0 bg-black/40 z-50 flex items-center justify-center"
      onClick={(e) => { if (e.target === e.currentTarget) handleClose(); }}
    >
      <div className="bg-background border border-surface-hover rounded-lg shadow-2xl w-full max-w-md mx-4">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-surface-hover">
          <span className="text-sm font-medium text-text-primary">导入 LLM 区域结果</span>
          <button
            onClick={handleClose}
            className="p-1 rounded hover:bg-surface-hover text-text-secondary transition-colors"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="px-4 py-3 space-y-3">
          <p className="text-[10px] text-text-secondary/60">
            将外部 LLM 返回的 JSON 数组粘贴到下方。若包含 <code className="bg-surface px-1 rounded">page</code> 字段则自动分发到对应页，否则使用下方指定的页码。
          </p>

          <div className="flex items-center gap-3">
            <div className="flex items-center gap-1.5">
              <label className="text-xs text-text-secondary">默认页码</label>
              <input
                type="number"
                min={1}
                max={totalPages}
                value={importPage}
                onChange={(e) => setImportPage(clamp(parseInt(e.target.value) || 1))}
                className="w-14 bg-surface border border-surface-hover rounded px-2 py-1 text-xs text-text-primary text-center"
              />
            </div>
            <span className="text-[10px] text-text-secondary/50">/ {totalPages} 页</span>
            {autoMode && (
              <span className="text-[10px] text-emerald-400">
                自动分发到第 {autoPages.join(', ')} 页
              </span>
            )}
          </div>

          <textarea
            value={importJson}
            onChange={(e) => { setImportJson(e.target.value); setImported(false); setAutoMode(false); }}
            placeholder={`[{"page":1,"type":"title","yRatio":0.09,...}, ...]`}
            className="w-full h-40 bg-surface border border-surface-hover rounded px-2 py-1 text-[10px] text-text-primary font-mono resize-none"
          />

          <button
            onClick={handleImport}
            disabled={!importJson.trim()}
            className="flex items-center gap-2 px-3 py-1.5 rounded text-xs bg-emerald-500/10 text-emerald-400 hover:bg-emerald-500/20 transition-colors disabled:opacity-50 w-full justify-center"
          >
            {imported ? (
              <><Check size={13} />已导入并显示 ✓</>
            ) : (
              <><Upload size={13} />导入并显示</>
            )}
          </button>

          {/* Error toast */}
          {error && (
            <div className="px-3 py-2 rounded-lg bg-red-500/10 text-red-400 text-xs">
              {error}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
