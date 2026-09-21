import { useCallback, useEffect, useState } from 'react';
import { BookCopy, FolderOpen, Loader2, X } from 'lucide-react';
import { zoteroDetect, zoteroPreview, type ZoteroPreview } from '@/lib/tauri';
import { runZoteroImport } from '@/stores/batchImportStore';

interface ZoteroImportDialogProps {
  open: boolean;
  onClose: () => void;
}

/** Zotero import wizard: auto-detect the data directory (or let the user
 *  pick one), preview item/PDF/collection/tag counts, then hand off to the
 *  shared batch-import progress dialog. */
export function ZoteroImportDialog({ open, onClose }: ZoteroImportDialogProps) {
  const [path, setPath] = useState('');
  const [preview, setPreview] = useState<ZoteroPreview | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const scan = useCallback(async (dir: string) => {
    setScanning(true);
    setError(null);
    setPreview(null);
    try {
      setPreview(await zoteroPreview(dir || undefined));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setScanning(false);
    }
  }, []);

  // Auto-detect on open; scan immediately when the default dir exists.
  useEffect(() => {
    if (!open) return;
    setPreview(null);
    setError(null);
    zoteroDetect()
      .then((dir) => {
        if (dir) {
          setPath(dir);
          scan(dir);
        }
      })
      .catch(() => {});
  }, [open, scan]);

  if (!open) return null;

  const handleBrowse = async () => {
    try {
      const { open: openDialog } = await import('@tauri-apps/plugin-dialog');
      const selected = await openDialog({ directory: true, title: '选择 Zotero 数据目录' });
      if (typeof selected === 'string') {
        setPath(selected);
        scan(selected);
      }
    } catch {
      // dialog cancelled or unavailable
    }
  };

  const handleImport = () => {
    if (!preview) return;
    onClose();
    runZoteroImport(preview.items, path.trim() || undefined).catch(() => {});
  };

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div className="relative w-full max-w-lg mx-4 bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-3 px-4 py-3 border-b border-surface-hover">
          <BookCopy size={18} className="text-primary" />
          <span className="text-sm font-medium text-text-primary">从 Zotero 导入</span>
          <button
            onClick={onClose}
            className="ml-auto p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
          >
            <X size={14} />
          </button>
        </div>

        <div className="px-4 py-4 space-y-3">
          <p className="text-xs text-text-secondary leading-relaxed">
            直接读取 Zotero 数据目录（默认 ~/Zotero），导入条目元数据、PDF 附件、分类树和标签。
            已存在的文献（DOI 或文件内容相同）会自动跳过；Zotero 运行时也可导入。
          </p>

          <div className="flex items-center gap-2">
            <input
              value={path}
              onChange={(e) => setPath(e.target.value)}
              onBlur={() => path.trim() && scan(path.trim())}
              placeholder="Zotero 数据目录，如 /home/you/Zotero"
              className="flex-1 h-8 px-2.5 rounded-lg bg-background border border-surface-hover text-xs text-text-primary outline-none focus:border-primary/50"
            />
            <button
              onClick={handleBrowse}
              className="h-8 px-2.5 flex items-center gap-1 rounded-lg border border-surface-hover text-xs text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors shrink-0"
            >
              <FolderOpen size={13} />
              浏览
            </button>
          </div>

          {scanning && (
            <div className="flex items-center gap-2 text-xs text-text-secondary">
              <Loader2 size={13} className="animate-spin" /> 正在扫描 Zotero 数据库…
            </div>
          )}
          {error && <div className="text-xs text-red-400">{error}</div>}

          {preview && !scanning && (
            <div className="grid grid-cols-4 gap-2 rounded-lg border border-surface-hover bg-background/40 px-3 py-2.5 text-center">
              {[
                ['条目', preview.items],
                ['含 PDF', preview.withPdf],
                ['分类', preview.collections],
                ['标签', preview.tags],
              ].map(([label, n]) => (
                <div key={label as string}>
                  <div className="text-base font-semibold text-text-primary tabular-nums">{n}</div>
                  <div className="text-[10px] text-text-secondary">{label}</div>
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="flex justify-end gap-2 px-4 py-3 border-t border-surface-hover">
          <button
            onClick={onClose}
            className="px-3 py-1.5 rounded-lg text-xs text-text-secondary hover:text-text-primary border border-surface-hover transition-colors"
          >
            取消
          </button>
          <button
            onClick={handleImport}
            disabled={!preview || scanning || preview.items === 0}
            className="px-3 py-1.5 rounded-lg text-xs bg-primary/15 text-primary hover:bg-primary/25 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            开始导入{preview ? `（${preview.items} 条）` : ''}
          </button>
        </div>
      </div>
    </div>
  );
}
