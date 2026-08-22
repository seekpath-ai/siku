import { useState } from 'react';
import { Link2, Loader2, X, AlertCircle, FileText, CheckCircle2 } from 'lucide-react';
import { usePreviewPaperFromLink, useImportPaperFromLink } from '@/hooks/useLibrary';
import { parseLinkImportError } from '@/lib/tauri';
import type { PaperLinkMetadata, LinkImportError } from '@/lib/types';

interface ImportFromLinkDialogProps {
  open: boolean;
  onClose: () => void;
}

const EXAMPLES = [
  'https://doi.org/10.1145/276675.276685',
  'https://arxiv.org/abs/2401.12345',
  'https://pubmed.ncbi.nlm.nih.gov/12345678/',
];

export function ImportFromLinkDialog({ open, onClose }: ImportFromLinkDialogProps) {
  const [url, setUrl] = useState('');
  const [preview, setPreview] = useState<PaperLinkMetadata | null>(null);
  const [error, setError] = useState<LinkImportError | null>(null);
  const [warning, setWarning] = useState<string | null>(null);

  const previewMutation = usePreviewPaperFromLink();
  const importMutation = useImportPaperFromLink();

  if (!open) return null;

  const handleParse = async () => {
    const trimmed = url.trim();
    if (!trimmed) return;
    setPreview(null);
    setError(null);
    try {
      const meta = await previewMutation.mutateAsync(trimmed);
      setPreview(meta);
    } catch (err) {
      setError(parseLinkImportError(err));
    }
  };

  const handleImport = async () => {
    const trimmed = url.trim();
    if (!trimmed || !preview) return;
    setError(null);
    setWarning(null);
    try {
      const result = await importMutation.mutateAsync({ url: trimmed, metadata: preview });
      if (result.warning) {
        setWarning(result.warning);
      } else {
        resetAndClose();
      }
    } catch (err) {
      setError(parseLinkImportError(err));
    }
  };

  const resetAndClose = () => {
    setUrl('');
    setPreview(null);
    setError(null);
    setWarning(null);
    onClose();
  };

  const isBusy = previewMutation.isPending || importMutation.isPending;

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={resetAndClose} />
      <div className="relative w-full max-w-lg mx-4 bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-3 px-4 py-3 border-b border-surface-hover">
          <Link2 size={18} className="text-primary" />
          <span className="text-sm font-medium text-text-primary">从链接导入文献</span>
          <button
            onClick={resetAndClose}
            className="ml-auto p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
          >
            <X size={14} />
          </button>
        </div>

        <div className="px-4 py-4 space-y-4">
          <div>
            <label className="block text-xs text-text-secondary mb-1.5">
              粘贴 DOI、arXiv、PubMed、Semantic Scholar 或 PDF 链接
            </label>
            <div className="flex gap-2">
              <input
                type="text"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !preview && !isBusy) handleParse();
                }}
                placeholder="https://doi.org/10.xxxx/xx"
                className="flex-1 h-9 rounded-lg bg-background border border-surface-hover px-3 text-sm text-text-primary placeholder:text-text-secondary/40 focus:outline-none focus:border-primary/50"
              />
              {!preview ? (
                <button
                  onClick={handleParse}
                  disabled={!url.trim() || isBusy}
                  className="px-3.5 h-9 rounded-lg bg-primary/10 text-primary text-xs font-medium hover:bg-primary/20 transition-colors disabled:opacity-40 disabled:cursor-not-allowed flex items-center gap-1.5"
                >
                  {previewMutation.isPending && <Loader2 size={13} className="animate-spin" />}
                  解析
                </button>
              ) : (
                <button
                  onClick={() => { setPreview(null); setError(null); }}
                  className="px-3.5 h-9 rounded-lg text-xs text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
                >
                  重新输入
                </button>
              )}
            </div>
          </div>

          {error && (
            <div className="flex items-start gap-2 rounded-lg bg-red-500/10 border border-red-500/20 p-3 text-xs text-red-400">
              <AlertCircle size={14} className="shrink-0 mt-0.5" />
              <div className="leading-relaxed break-all">
                <span className="font-medium uppercase tracking-wider">{error.code}</span>
                <p className="mt-0.5">{error.message}</p>
              </div>
            </div>
          )}

          {warning && (
            <div className="flex items-start gap-2 rounded-lg bg-yellow-500/10 border border-yellow-500/20 p-3 text-xs text-yellow-500">
              <AlertCircle size={14} className="shrink-0 mt-0.5" />
              <span className="leading-relaxed break-all">{warning}</span>
            </div>
          )}

          {preview && (
            <div className="rounded-lg border border-surface-hover bg-background p-3 space-y-3">
              <div className="flex items-start gap-3">
                <div className="mt-0.5 shrink-0 w-8 h-8 rounded bg-primary/10 flex items-center justify-center text-primary">
                  <FileText size={14} />
                </div>
                <div className="min-w-0 flex-1">
                  <h4 className="text-sm font-medium text-text-primary leading-snug">
                    {preview.title || '未识别标题'}
                  </h4>
                  <p className="text-xs text-text-secondary mt-1 truncate">
                    {preview.authors.length > 0
                      ? preview.authors.join(', ')
                      : '作者未知'}
                    {preview.year && ` · ${preview.year}`}
                    {preview.journal && ` · ${preview.journal}`}
                  </p>
                </div>
              </div>

              {preview.abstract_text && (
                <p className="text-xs text-text-secondary leading-relaxed line-clamp-4">
                  {preview.abstract_text}
                </p>
              )}

              <div className="flex flex-wrap items-center gap-2 text-[11px]">
                {preview.doi && (
                  <span className="px-1.5 py-0.5 rounded bg-surface-hover text-text-secondary">
                    DOI: {preview.doi}
                  </span>
                )}
                {preview.pdf_url ? (
                  <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-green-500/10 text-green-400">
                    <CheckCircle2 size={11} />
                    可获取 PDF
                  </span>
                ) : (
                  <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-yellow-500/10 text-yellow-400">
                    <AlertCircle size={11} />
                    未找到开放获取 PDF
                  </span>
                )}
              </div>
            </div>
          )}

          {!url && !preview && (
            <div className="text-[11px] text-text-secondary/60 space-y-1">
              <p>支持示例：</p>
              <ul className="space-y-0.5 font-mono text-[10px]">
                {EXAMPLES.map((ex) => (
                  <li key={ex} className="truncate">{ex}</li>
                ))}
              </ul>
            </div>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-surface-hover bg-surface/50">
          {warning ? (
            <button
              onClick={resetAndClose}
              className="px-3.5 py-1.5 rounded-lg bg-primary/10 text-primary text-xs font-medium hover:bg-primary/20 transition-colors flex items-center gap-1.5"
            >
              <CheckCircle2 size={13} />
              完成
            </button>
          ) : (
            <>
              <button
                onClick={resetAndClose}
                className="px-3.5 py-1.5 rounded-lg text-xs text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
              >
                取消
              </button>
              <button
                onClick={handleImport}
                disabled={!preview || isBusy}
                className="px-3.5 py-1.5 rounded-lg bg-primary/10 text-primary text-xs font-medium hover:bg-primary/20 transition-colors disabled:opacity-40 disabled:cursor-not-allowed flex items-center gap-1.5"
              >
                {importMutation.isPending && <Loader2 size={13} className="animate-spin" />}
                导入
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
