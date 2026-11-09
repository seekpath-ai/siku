import { useEffect, useState } from 'react';
import { FolderOpen, X, Loader2, FileText, Image as ImageIcon, Download, LibraryBig } from 'lucide-react';
import { save as saveDialog } from '@tauri-apps/plugin-dialog';
import { Modal } from '@/components/ui/Modal';
import {
  remoteChatFiles, remoteChatFileSave, remoteChatFileImport,
  type RemoteChatFile,
} from '@/lib/tauri';

interface Props {
  channel: string;
  chatId: string;
  onClose: () => void;
}

function fmtSize(bytes: number): string {
  if (!bytes || bytes <= 0) return '';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function fmtTime(ms: string): string {
  const t = Number(ms);
  if (!t) return '';
  const d = new Date(t);
  return `${d.getFullYear()}/${String(d.getMonth() + 1).padStart(2, '0')}/${String(d.getDate()).padStart(2, '0')} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

/** 「查看群文件」模态框：群内发过的文件/图片消息列表，可导入文献库或另存。 */
export function RemoteChatFilesDialog({ channel, chatId, onClose }: Props) {
  const [files, setFiles] = useState<RemoteChatFile[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Per-row busy marker: "save" | "import". */
  const [busy, setBusy] = useState<Record<string, string>>({});
  const [rowMsg, setRowMsg] = useState<Record<string, { ok: boolean; text: string }>>({});

  useEffect(() => {
    remoteChatFiles(channel, chatId).then(setFiles).catch((e) => setError(String(e)));
  }, [channel, chatId]);


  const rowKey = (f: RemoteChatFile) => `${f.message_id}:${f.file_key}`;
  const mark = (f: RemoteChatFile, ok: boolean, text: string) =>
    setRowMsg((m) => ({ ...m, [rowKey(f)]: { ok, text } }));

  const handleSave = async (f: RemoteChatFile) => {
    const path = await saveDialog({ defaultPath: f.name });
    if (!path) return;
    setBusy((b) => ({ ...b, [rowKey(f)]: 'save' }));
    try {
      await remoteChatFileSave(channel, f.message_id, f.file_key, f.kind, path);
      mark(f, true, '已保存');
    } catch (e) {
      mark(f, false, String(e));
    } finally {
      setBusy((b) => ({ ...b, [rowKey(f)]: '' }));
    }
  };

  const handleImport = async (f: RemoteChatFile) => {
    setBusy((b) => ({ ...b, [rowKey(f)]: 'import' }));
    try {
      const paper = await remoteChatFileImport(channel, f.message_id, f.file_key, f.kind, f.name);
      mark(f, true, `已导入：${paper.title || f.name}`);
    } catch (e) {
      mark(f, false, String(e));
    } finally {
      setBusy((b) => ({ ...b, [rowKey(f)]: '' }));
    }
  };

  return (
    <Modal
      onClose={onClose}
      backdropClassName="bg-black/50 backdrop-blur-sm"
    >
      <div className="relative w-[640px] max-w-[94vw] h-[520px] max-h-[86vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-2 px-4 py-2.5 border-b border-surface-hover shrink-0">
          <FolderOpen size={15} className="text-text-secondary" />
          <span className="text-sm font-medium text-text-primary">群文件</span>
          <span className="text-[11px] text-text-secondary/60">群里发过的文件与图片（新→旧）</span>
          <div className="flex-1" />
          <button
            onClick={onClose}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭"
          >
            <X size={14} />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-3 py-2">
          {!files && !error && (
            <div className="flex items-center justify-center py-10 text-text-secondary">
              <Loader2 size={18} className="animate-spin" />
            </div>
          )}
          {error && <div className="px-2 py-3 text-sm text-red-400 whitespace-pre-wrap">{error}</div>}
          {files && files.length === 0 && (
            <div className="px-2 py-10 text-center text-sm text-text-secondary">群里还没有文件消息</div>
          )}
          {files?.map((f) => {
            const key = rowKey(f);
            const state = busy[key];
            const msg = rowMsg[key];
            const isPdf = f.kind === 'file' && /\.pdf$/i.test(f.name);
            return (
              <div key={key} className="px-2 py-2 rounded-lg hover:bg-surface-hover">
                <div className="flex items-center gap-2.5">
                  {f.kind === 'image' ? (
                    <ImageIcon size={15} className="text-text-secondary shrink-0" />
                  ) : (
                    <FileText size={15} className="text-text-secondary shrink-0" />
                  )}
                  <div className="flex-1 min-w-0">
                    <div className="text-[13px] text-text-primary truncate">{f.name}</div>
                    <div className="text-[11px] text-text-secondary/60">
                      {[fmtSize(f.size_bytes), fmtTime(f.create_time_ms)].filter(Boolean).join(' · ')}
                    </div>
                  </div>
                  {isPdf && (
                    <button
                      disabled={!!state}
                      onClick={() => void handleImport(f)}
                      className="h-7 px-2 rounded-lg bg-primary/15 text-primary text-xs flex items-center gap-1 hover:bg-primary/25 disabled:opacity-50"
                    >
                      {state === 'import' ? <Loader2 size={11} className="animate-spin" /> : <LibraryBig size={11} />}
                      导入文献库
                    </button>
                  )}
                  <button
                    disabled={!!state}
                    onClick={() => void handleSave(f)}
                    className="h-7 px-2 rounded-lg border border-surface-hover text-text-secondary text-xs flex items-center gap-1 hover:text-text-primary disabled:opacity-50"
                  >
                    {state === 'save' ? <Loader2 size={11} className="animate-spin" /> : <Download size={11} />}
                    另存为
                  </button>
                </div>
                {msg && (
                  <div className={`mt-1 pl-7 text-[11px] ${msg.ok ? 'text-emerald-400' : 'text-red-400'}`}>
                    {msg.text}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    </Modal>
  );
}
