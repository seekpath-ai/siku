import { useState, useEffect } from 'react';
import { ArrowLeft, ExternalLink, ZoomIn, ZoomOut, Maximize } from 'lucide-react';
import { PdfViewer } from '@/components/reader/PdfViewer';
import { CodeBlock } from '@/components/chat/CodeBlock';
import type { FileItem, TextPreview } from '@/lib/types';
import { filesGet, filesResolveUrl, filesReadText, filesOpen } from '@/lib/tauri';

function isPdf(f: FileItem): boolean {
  return f.mime_type === 'application/pdf' || f.name.toLowerCase().endsWith('.pdf');
}

function isImage(f: FileItem): boolean {
  return (f.mime_type ?? '').startsWith('image/') || /\.(png|jpe?g|gif|svg|webp|bmp)$/i.test(f.name);
}

/** In-app preview for a vault-managed file: PDFs use the reader's PdfViewer,
 *  images get a simple zoomable viewer, and everything else is attempted as
 *  text (the backend sniffs content and rejects binaries, which fall back to
 *  "open with system application"). Used by the /file/$fileId route and the
 *  notes page's inline preview pane; pass `onBack` to show a back button. */
export function FilePreview({ fileId, onBack }: { fileId: string; onBack?: () => void }) {
  const [file, setFile] = useState<FileItem | null>(null);
  const [url, setUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Text preview: null = loading/not applicable, false = binary (no preview).
  const [text, setText] = useState<TextPreview | false | null>(null);
  // Image zoom: null = fit to container.
  const [zoom, setZoom] = useState<number | null>(null);

  useEffect(() => {
    let active = true;
    setFile(null);
    setUrl(null);
    setError(null);
    setText(null);
    setZoom(null);
    (async () => {
      try {
        const [f, u] = await Promise.all([filesGet(fileId), filesResolveUrl(fileId)]);
        if (!active) return;
        setFile(f);
        setUrl(u);
        // Text preview for everything that is not PDF/image; the backend
        // sniffs content and errors out on binary files.
        if (!isPdf(f) && !isImage(f)) {
          try {
            const preview = await filesReadText(fileId);
            if (active) setText(preview);
          } catch {
            if (active) setText(false);
          }
        }
      } catch (err) {
        if (active) setError(String(err));
      }
    })();
    return () => {
      active = false;
    };
  }, [fileId]);

  const zoomStep = (delta: number) =>
    setZoom((z) => Math.min(8, Math.max(0.1, (z ?? 1) + delta)));

  const fileExt = file?.name.split('.').pop()?.toLowerCase() ?? 'text';

  return (
    <div className="flex flex-col h-full bg-background">
      {/* Header */}
      <div className="flex items-center gap-2 px-3 h-10 border-b border-surface-hover shrink-0">
        {onBack && (
          <button
            onClick={onBack}
            className="w-7 h-7 rounded flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
            title="返回"
          >
            <ArrowLeft size={15} />
          </button>
        )}
        <span className="flex-1 min-w-0 truncate text-[13px] text-text-primary">
          {file?.name ?? '…'}
        </span>
        {file && isImage(file) && (
          <div className="flex items-center gap-0.5">
            <button
              onClick={() => zoomStep(-0.25)}
              className="w-7 h-7 rounded flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
              title="缩小"
            >
              <ZoomOut size={14} />
            </button>
            <span className="text-[11px] text-text-secondary tabular-nums w-11 text-center">
              {zoom === null ? '适应' : `${Math.round(zoom * 100)}%`}
            </span>
            <button
              onClick={() => zoomStep(0.25)}
              className="w-7 h-7 rounded flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
              title="放大"
            >
              <ZoomIn size={14} />
            </button>
            <button
              onClick={() => setZoom(null)}
              className="w-7 h-7 rounded flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
              title="适应窗口"
            >
              <Maximize size={14} />
            </button>
          </div>
        )}
        <button
          onClick={() => filesOpen(fileId).catch(() => {})}
          className="w-7 h-7 rounded flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
          title="用系统应用打开"
        >
          <ExternalLink size={14} />
        </button>
      </div>

      {/* Body */}
      <div className="flex-1 min-h-0">
        {error ? (
          <div className="flex items-center justify-center h-full text-sm text-text-secondary">
            打开失败:{error}
          </div>
        ) : !file || !url ? (
          <div className="flex items-center justify-center h-full text-sm text-text-secondary/60">
            加载中…
          </div>
        ) : isPdf(file) ? (
          <PdfViewer src={url} />
        ) : isImage(file) ? (
          <div className="h-full overflow-auto flex items-center justify-center p-4">
            <img
              src={url}
              alt={file.name}
              className="rounded"
              style={
                zoom === null
                  ? { maxWidth: '100%', maxHeight: '100%', objectFit: 'contain' }
                  : { width: `${zoom * 100}%` }
              }
            />
          </div>
        ) : text === false ? (
          <div className="flex flex-col items-center justify-center h-full gap-3 text-text-secondary">
            <p className="text-sm">二进制文件，无法应用内预览</p>
            <button
              onClick={() => filesOpen(fileId).catch(() => {})}
              className="px-3 py-1.5 rounded border border-surface-hover text-[13px] hover:bg-surface-hover transition-colors"
            >
              用系统应用打开
            </button>
          </div>
        ) : text ? (
          <div className="h-full overflow-y-auto px-4">
            {text.truncated && (
              <p className="sticky top-0 py-2 text-[11px] text-amber-400/90 bg-background">
                文件过大，仅预览前 2 MB
              </p>
            )}
            <CodeBlock code={text.content} language={fileExt} />
          </div>
        ) : (
          <div className="flex items-center justify-center h-full text-sm text-text-secondary/60">
            加载中…
          </div>
        )}
      </div>
    </div>
  );
}
