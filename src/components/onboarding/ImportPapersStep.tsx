import { useEffect, useState } from 'react';
import { FileText, Loader2, FolderOpen } from 'lucide-react';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import {
  importPapersBatch,
  zoteroDetect,
  zoteroImport,
  zoteroPreview,
  type ZoteroPreview,
} from '@/lib/tauri';
import { pickDirectory } from '@/lib/pickDirectory';

/** Onboarding step 3: first literature import, in-wizard. Two paths — pick
 *  PDFs directly, or pull the whole Zotero library (default dir auto-detected). */
export function ImportPapersStep() {
  const [pdfBusy, setPdfBusy] = useState(false);
  const [pdfResult, setPdfResult] = useState<string | null>(null);

  const [zoteroDir, setZoteroDir] = useState<string | null>(null);
  const [zoteroInfo, setZoteroInfo] = useState<ZoteroPreview | null>(null);
  const [zoteroBusy, setZoteroBusy] = useState(false);
  const [zoteroResult, setZoteroResult] = useState<string | null>(null);
  const [zoteroError, setZoteroError] = useState<string | null>(null);
  const [detected, setDetected] = useState<boolean | null>(null);

  // Detect the default Zotero data dir on mount and preview it immediately.
  useEffect(() => {
    (async () => {
      const dir = await zoteroDetect().catch(() => null);
      setDetected(dir !== null);
      if (!dir) return;
      setZoteroDir(dir);
      try {
        setZoteroInfo(await zoteroPreview(dir));
      } catch (e) {
        setZoteroError(e instanceof Error ? e.message : String(e));
      }
    })();
  }, []);

  const handlePickPdfs = async () => {
    if (pdfBusy) return;
    try {
      const selected = await openDialog({
        multiple: true,
        filters: [{ name: 'PDF', extensions: ['pdf'] }],
      });
      if (!selected || (Array.isArray(selected) && selected.length === 0)) return;
      const paths = Array.isArray(selected) ? selected : [selected];
      setPdfBusy(true);
      setPdfResult(null);
      const summary = await importPapersBatch(paths);
      setPdfResult(`导入 ${summary.imported} 篇，跳过 ${summary.skipped}，失败 ${summary.failed}`);
    } catch (e) {
      setPdfResult(`导入失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setPdfBusy(false);
    }
  };

  const handlePickZoteroDir = async () => {
    const dir = await pickDirectory();
    if (!dir) return;
    setZoteroDir(dir);
    setZoteroInfo(null);
    setZoteroError(null);
    setZoteroResult(null);
    try {
      setZoteroInfo(await zoteroPreview(dir));
    } catch (e) {
      setZoteroError(e instanceof Error ? e.message : String(e));
    }
  };

  const handleZoteroImport = async () => {
    if (zoteroBusy || !zoteroDir) return;
    setZoteroBusy(true);
    setZoteroResult(null);
    setZoteroError(null);
    try {
      const summary = await zoteroImport(zoteroDir);
      setZoteroResult(`导入 ${summary.imported} 条，跳过 ${summary.skipped}，失败 ${summary.failed}`);
    } catch (e) {
      setZoteroError(e instanceof Error ? e.message : String(e));
    } finally {
      setZoteroBusy(false);
    }
  };

  return (
    <div className="flex flex-col gap-3 text-left">
      {/* Direct PDF import */}
      <div className="px-4 py-3 rounded-xl bg-background border border-surface-hover">
        <div className="flex items-center gap-2 text-sm text-text-primary mb-1">
          <FileText size={14} className="text-primary" />
          导入 PDF 文献
        </div>
        <p className="text-[11px] text-text-secondary mb-2.5">
          支持多选，自动提取标题、作者等元数据。也可以稍后直接拖拽 PDF 到窗口。
        </p>
        <div className="flex items-center gap-2">
          <button
            onClick={handlePickPdfs}
            disabled={pdfBusy}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-white text-xs font-medium hover:opacity-90 disabled:opacity-40"
          >
            {pdfBusy ? <Loader2 size={12} className="animate-spin" /> : <FolderOpen size={12} />}
            {pdfBusy ? '导入中…' : '选择 PDF 文件'}
          </button>
          {pdfResult && <span className="text-[11px] text-text-secondary">{pdfResult}</span>}
        </div>
      </div>

      {/* Zotero import */}
      <div className="px-4 py-3 rounded-xl bg-background border border-surface-hover">
        <div className="flex items-center gap-2 text-sm text-text-primary mb-1">
          <img src="/brand-logos/zotero.svg" alt="Zotero" className="w-3.5 h-3.5" />
          从 Zotero 导入
        </div>
        {detected === null ? (
          <div className="flex items-center gap-1.5 text-[11px] text-text-secondary">
            <Loader2 size={11} className="animate-spin" />正在检测 Zotero 数据目录…
          </div>
        ) : (
          <>
            <p className="text-[11px] text-text-secondary mb-2.5">
              {zoteroInfo
                ? `${zoteroInfo.dataDir} — ${zoteroInfo.items} 个条目（${zoteroInfo.withPdf} 含 PDF），${zoteroInfo.collections} 个分类`
                : detected
                  ? '读取 Zotero 库失败，可手动选择数据目录'
                  : '未检测到默认 Zotero 目录（~/Zotero），可手动选择'}
            </p>
            <div className="flex items-center gap-2">
              {zoteroInfo && (
                <button
                  onClick={handleZoteroImport}
                  disabled={zoteroBusy}
                  className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-white text-xs font-medium hover:opacity-90 disabled:opacity-40"
                >
                  {zoteroBusy && <Loader2 size={12} className="animate-spin" />}
                  {zoteroBusy ? '导入中…' : '开始导入'}
                </button>
              )}
              <button
                onClick={handlePickZoteroDir}
                disabled={zoteroBusy}
                className="px-3 py-1.5 rounded-lg border border-surface-hover text-xs text-text-secondary hover:bg-surface-hover disabled:opacity-40"
              >
                手动选择目录
              </button>
              {zoteroResult && <span className="text-[11px] text-text-secondary">{zoteroResult}</span>}
            </div>
            {zoteroError && <div className="mt-1.5 text-[11px] text-red-400 break-words">{zoteroError}</div>}
          </>
        )}
      </div>
    </div>
  );
}
