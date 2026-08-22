import { useState } from 'react';
import { X, Loader2, Download, Copy, Check } from 'lucide-react';
import { generateExportText } from '@/lib/exportLayout';
import type { PdfViewerHandle } from './PdfViewer';

interface ExportLayoutDialogProps {
  isOpen: boolean;
  onClose: () => void;
  totalPages: number;
  currentPage: number;
  pdfViewerRef: React.RefObject<PdfViewerHandle | null>;
}

export function ExportLayoutDialog({
  isOpen,
  onClose,
  totalPages,
  currentPage,
  pdfViewerRef,
}: ExportLayoutDialogProps) {
  const [startPage, setStartPage] = useState(currentPage);
  const [endPage, setEndPage] = useState(currentPage);
  const [generating, setGenerating] = useState(false);
  const [lineText, setLineText] = useState<string | null>(null);
  const [jsonText, setJsonText] = useState<string | null>(null);
  const [copiedLine, setCopiedLine] = useState(false);
  const [copiedJson, setCopiedJson] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState('');

  if (!isOpen) return null;

  const clamp = (v: number) => Math.max(1, Math.min(totalPages, v));

  const handleGenerate = async () => {
    const s = clamp(startPage);
    const e = clamp(endPage);
    if (s > e) {
      setError('起始页不能大于结束页');
      setTimeout(() => setError(null), 3000);
      return;
    }

    setGenerating(true);
    setError(null);
    setLineText(null);
    setJsonText(null);

    try {
      const result = await generateExportText(
        async (pageNum) => {
          setProgress(`导出中 (${pageNum}/${e})`);
          return pdfViewerRef.current?.getPageTextContent(pageNum) ?? null;
        },
        s,
        e,
        totalPages,
      );

      setLineText(result.lineText);
      setJsonText(result.jsonText);
    } catch (e: unknown) {
      setError(`导出失败: ${e instanceof Error ? e.message : String(e)}`);
      setTimeout(() => setError(null), 3000);
    } finally {
      setGenerating(false);
      setProgress('');
    }
  };

  const handleCopy = async (text: string, kind: 'line' | 'json') => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Fallback for Tauri/older browsers
      const ta = document.createElement('textarea');
      ta.value = text;
      ta.style.position = 'fixed';
      ta.style.opacity = '0';
      document.body.appendChild(ta);
      ta.select();
      document.execCommand('copy');
      document.body.removeChild(ta);
    }
    if (kind === 'line') {
      setCopiedLine(true);
      setTimeout(() => setCopiedLine(false), 3000);
    } else {
      setCopiedJson(true);
      setTimeout(() => setCopiedJson(false), 3000);
    }
  };

  const handleClose = () => {
    setLineText(null);
    setJsonText(null);
    setError(null);
    setGenerating(false);
    onClose();
  };

  return (
    <div
      className="fixed inset-0 bg-black/40 z-50 flex items-center justify-center"
      onClick={(e) => { if (e.target === e.currentTarget) handleClose(); }}
    >
      <div className="bg-background border border-surface-hover rounded-lg shadow-2xl w-full max-w-lg mx-4 max-h-[90vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-surface-hover">
          <span className="text-sm font-medium text-text-primary">导出文本布局</span>
          <button
            onClick={handleClose}
            className="p-1 rounded hover:bg-surface-hover text-text-secondary transition-colors"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="px-4 py-3 space-y-3 overflow-y-auto flex-1">
          {/* Page range inputs */}
          <div className="flex items-center gap-3">
            <div className="flex items-center gap-1.5">
              <label className="text-xs text-text-secondary">起始页</label>
              <input
                type="number"
                min={1}
                max={totalPages}
                value={startPage}
                onChange={(e) => setStartPage(clamp(parseInt(e.target.value) || 1))}
                className="w-16 bg-surface border border-surface-hover rounded px-2 py-1 text-xs text-text-primary text-center"
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
                onChange={(e) => setEndPage(clamp(parseInt(e.target.value) || 1))}
                className="w-16 bg-surface border border-surface-hover rounded px-2 py-1 text-xs text-text-primary text-center"
              />
            </div>
            <span className="text-[10px] text-text-secondary/50">/ {totalPages} 页</span>
          </div>

          {/* Quick select */}
          <div className="flex gap-2">
            <button
              onClick={() => { setStartPage(currentPage); setEndPage(currentPage); }}
              className="px-2 py-0.5 rounded text-[10px] text-text-secondary hover:bg-surface-hover border border-surface-hover transition-colors"
            >
              当前页
            </button>
            <button
              onClick={() => { setStartPage(1); setEndPage(totalPages); }}
              className="px-2 py-0.5 rounded text-[10px] text-text-secondary hover:bg-surface-hover border border-surface-hover transition-colors"
            >
              全部
            </button>
          </div>

          {/* Generate button */}
          <button
            onClick={handleGenerate}
            disabled={generating}
            className="flex items-center gap-2 px-3 py-1.5 rounded text-xs bg-primary/10 text-primary hover:bg-primary/20 transition-colors disabled:opacity-50 w-full justify-center"
          >
            {generating ? (
              <><Loader2 size={13} className="animate-spin" />{progress || '导出中...'}</>
            ) : (
              <><Download size={13} />生成</>
            )}
          </button>

          {/* Results */}
          {(lineText !== null || jsonText !== null) && (
            <div className="space-y-3">
              {/* LINE format */}
              <div>
                <div className="flex items-center justify-between mb-1">
                  <span className="text-[10px] text-text-secondary">AI 聊天用（复制粘贴到 ChatGPT/Claude 等对话中）</span>
                  <button
                    onClick={() => handleCopy(lineText!, 'line')}
                    className="flex items-center gap-1 px-2 py-0.5 rounded text-[10px] text-text-secondary hover:bg-surface-hover border border-surface-hover transition-colors"
                  >
                    {copiedLine ? <Check size={11} className="text-emerald-400" /> : <Copy size={11} />}
                    {copiedLine ? '已复制 ✓' : '复制文本'}
                  </button>
                </div>
                <textarea
                  readOnly
                  value={lineText!}
                  className="w-full h-32 bg-surface border border-surface-hover rounded px-2 py-1 text-[10px] text-text-primary font-mono resize-none"
                />
              </div>

              {/* JSON format */}
              <div>
                <div className="flex items-center justify-between mb-1">
                  <span className="text-[10px] text-text-secondary">JSON 原始数据（调试用）</span>
                  <button
                    onClick={() => handleCopy(jsonText!, 'json')}
                    className="flex items-center gap-1 px-2 py-0.5 rounded text-[10px] text-text-secondary hover:bg-surface-hover border border-surface-hover transition-colors"
                  >
                    {copiedJson ? <Check size={11} className="text-emerald-400" /> : <Copy size={11} />}
                    {copiedJson ? '已复制 ✓' : '复制 JSON'}
                  </button>
                </div>
                <textarea
                  readOnly
                  value={jsonText!}
                  className="w-full h-32 bg-surface border border-surface-hover rounded px-2 py-1 text-[10px] text-text-primary font-mono resize-none"
                />
              </div>
            </div>
          )}
        </div>

        {/* Error toast */}
        {error && (
          <div className="absolute bottom-4 left-1/2 -translate-x-1/2 px-4 py-2 rounded-lg bg-red-500/90 text-white text-xs shadow-lg z-30">
            {error}
          </div>
        )}
      </div>
    </div>
  );
}
