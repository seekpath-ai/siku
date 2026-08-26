import { useState, useEffect, useMemo } from 'react';
import { BookOpen, Eye, FileText, X, Search, Loader2 } from 'lucide-react';
import { notesListAll, vaultList, filesList, filesReadText } from '@/lib/tauri';
import { FilePreview } from '@/components/files/FilePreview';
import { contextKey, type ContextItem } from './contextItem';
import type { Note, FileItem } from '@/lib/types';

function isPdf(f: FileItem): boolean {
  return f.mime_type === 'application/pdf' || f.name.toLowerCase().endsWith('.pdf');
}

function isImage(f: FileItem): boolean {
  return (f.mime_type ?? '').startsWith('image/') || /\.(png|jpe?g|gif|svg|webp|bmp)$/i.test(f.name);
}

/** One-line excerpt around the first query hit, shown under a note title so
 *  content matches explain why the note appeared in the results. */
function contentExcerpt(content: string, q: string): string | null {
  const idx = content.toLowerCase().indexOf(q);
  if (idx < 0) return null;
  const start = Math.max(0, idx - 24);
  const end = Math.min(content.length, idx + q.length + 56);
  const text = content.slice(start, end).replace(/\s+/g, ' ').trim();
  return (start > 0 ? '…' : '') + text + (end < content.length ? '…' : '');
}

interface Props {
  /** Keys ("kind:id") already attached, so re-opening keeps them checked. */
  initialSelected?: Set<string>;
  onClose: () => void;
  onConfirm: (items: ContextItem[]) => void;
}

/** Multi-select picker for notes and vault files, attached to the chat
 * message as a `<context>` block (plain user-message context, unlike the
 * long-term memory which goes into the system prompt). */
export function ContextPickerModal({ initialSelected, onClose, onConfirm }: Props) {
  const [notes, setNotes] = useState<Note[] | null>(null);
  const [files, setFiles] = useState<FileItem[] | null>(null);
  /** Folder notes (id → note) and vault names, used to build file path labels. */
  const [folders, setFolders] = useState<Map<string, Note>>(new Map());
  const [vaultNames, setVaultNames] = useState<Map<string, string>>(new Map());
  const [query, setQuery] = useState('');
  const [selected, setSelected] = useState<Set<string>>(new Set(initialSelected ?? []));
  const [resolving, setResolving] = useState(false);
  /** File shown in the stacked preview overlay (PDF/image only). */
  const [previewId, setPreviewId] = useState<string | null>(null);

  useEffect(() => {
    notesListAll()
      .then((ns) => {
        setNotes(ns.filter((n) => !n.is_folder));
        setFolders(new Map(ns.filter((n) => n.is_folder).map((n) => [n.id, n])));
      })
      .catch(() => setNotes([]));
    vaultList()
      .then(async (vs) => {
        setVaultNames(new Map(vs.map((v) => [v.id, v.name])));
        const all: FileItem[] = [];
        for (const v of vs) {
          all.push(...(await filesList(v.id).catch(() => [] as FileItem[])));
        }
        setFiles(all);
      })
      .catch(() => setFiles([]));
  }, []);

  useEffect(() => {
    const onDown = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      // Escape closes the stacked preview first, then the picker itself.
      if (previewId) setPreviewId(null);
      else onClose();
    };
    window.addEventListener('keydown', onDown);
    return () => window.removeEventListener('keydown', onDown);
  }, [onClose, previewId]);

  const q = query.trim().toLowerCase();
  const filteredNotes = useMemo(
    () =>
      (notes ?? []).filter(
        (n) =>
          !q ||
          (n.title || '').toLowerCase().includes(q) ||
          (n.content_plain || n.content || '').toLowerCase().includes(q)
      ),
    [notes, q]
  );
  const filteredFiles = useMemo(
    () => (files ?? []).filter((f) => !q || f.name.toLowerCase().includes(q)),
    [files, q]
  );

  /** Display path per item (notes and files): vault name + folder chain
   *  (parents are notes-tree folders), shown above the title/name. */
  const paths = useMemo(() => {
    const chain = (parentId: string | null): string[] => {
      const parts: string[] = [];
      let pid = parentId;
      let guard = 0; // cycle protection
      while (pid && guard++ < 32) {
        const folder = folders.get(pid);
        if (!folder) break;
        parts.unshift(folder.title || '未命名');
        pid = folder.parent_id;
      }
      return parts;
    };
    const map = new Map<string, string>();
    for (const n of notes ?? []) {
      map.set(n.id, [vaultNames.get(n.vault_id), ...chain(n.parent_id)].filter(Boolean).join(' / '));
    }
    for (const f of files ?? []) {
      map.set(f.id, [vaultNames.get(f.vault_id), ...chain(f.parent_id)].filter(Boolean).join(' / '));
    }
    return map;
  }, [notes, files, folders, vaultNames]);

  const toggle = (key: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const handleConfirm = async () => {
    setResolving(true);
    const items: ContextItem[] = [];
    for (const key of selected) {
      const [kind, id] = key.split(':') as ['note' | 'file', string];
      if (kind === 'note') {
        const n = (notes ?? []).find((x) => x.id === id);
        if (n) items.push({ kind, id, name: n.title || '未命名笔记', content: n.content });
      } else {
        const f = (files ?? []).find((x) => x.id === id);
        if (!f) continue;
        // Text files carry their content; binaries get a placeholder.
        const content = await filesReadText(id)
          .then((t) => t.content)
          .catch(() => null);
        items.push({ kind, id, name: f.name, content });
      }
    }
    setResolving(false);
    onConfirm(items);
  };

  const loading = notes === null || files === null;

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div className="relative w-[560px] max-w-[92vw] h-[520px] max-h-[84vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-2 px-4 py-2.5 border-b border-surface-hover shrink-0">
          <BookOpen size={15} className="text-text-secondary" />
          <span className="text-sm font-medium text-text-primary">选择上下文</span>
          <div className="flex-1" />
          <button
            onClick={onClose}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭"
          >
            <X size={14} />
          </button>
        </div>

        <div className="px-3 py-2 border-b border-surface-hover shrink-0">
          <div className="flex items-center gap-2 px-2.5 py-1.5 rounded-lg bg-background border border-surface-hover">
            <Search size={13} className="text-text-secondary/50 shrink-0" />
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="搜索笔记标题、正文或文件名…"
              className="flex-1 bg-transparent border-0 outline-0 text-[13px] text-text-primary placeholder:text-text-secondary/50"
              autoFocus
            />
          </div>
        </div>

        <div className="flex-1 overflow-y-auto px-3 py-2">
          {loading ? (
            <div className="flex items-center justify-center h-full text-sm text-text-secondary/60">
              <Loader2 size={15} className="animate-spin mr-2" />加载中…
            </div>
          ) : (
            <>
              {filteredNotes.length > 0 && (
                <>
                  <div className="px-1.5 py-1 text-[11px] text-text-secondary/50">笔记</div>
                  {filteredNotes.map((n) => {
                    const key = contextKey('note', n.id);
                    const path = paths.get(n.id) || '';
                    const snippet = q
                      ? contentExcerpt(n.content_plain || n.content || '', q)
                      : null;
                    return (
                      <label
                        key={key}
                        className="flex items-center gap-2.5 px-2.5 py-1.5 rounded-lg hover:bg-surface-hover cursor-pointer"
                      >
                        <input
                          type="checkbox"
                          checked={selected.has(key)}
                          onChange={() => toggle(key)}
                          className="accent-primary shrink-0"
                        />
                        <FileText size={13} className="text-text-secondary/60 shrink-0" />
                        <span className="flex-1 min-w-0">
                          {path && (
                            <span
                              className="block truncate text-[11px] text-text-secondary/60"
                              title={path}
                            >
                              {path}
                            </span>
                          )}
                          <span className="block truncate text-[13px] text-text-primary">
                            {n.title || '未命名笔记'}
                          </span>
                          {snippet && (
                            <span className="block truncate text-[11px] text-text-secondary/60">
                              {snippet}
                            </span>
                          )}
                        </span>
                      </label>
                    );
                  })}
                </>
              )}
              {filteredFiles.length > 0 && (
                <>
                  <div className="px-1.5 py-1 mt-1 text-[11px] text-text-secondary/50">文件</div>
                  {filteredFiles.map((f) => {
                    const key = contextKey('file', f.id);
                    const path = paths.get(f.id) || '';
                    return (
                      <label
                        key={key}
                        className="group flex items-center gap-2.5 px-2.5 py-1.5 rounded-lg hover:bg-surface-hover cursor-pointer"
                      >
                        <input
                          type="checkbox"
                          checked={selected.has(key)}
                          onChange={() => toggle(key)}
                          className="accent-primary shrink-0"
                        />
                        <FileText size={13} className="text-text-secondary/60 shrink-0" />
                        <span className="flex-1 min-w-0">
                          {path && (
                            <span
                              className="block truncate text-[11px] text-text-secondary/60"
                              title={path}
                            >
                              {path}
                            </span>
                          )}
                          <span className="block truncate text-[13px] text-text-primary">{f.name}</span>
                        </span>
                        {(isPdf(f) || isImage(f)) && (
                          <button
                            type="button"
                            onClick={(e) => {
                              // Keep the label from toggling the checkbox.
                              e.preventDefault();
                              e.stopPropagation();
                              setPreviewId(f.id);
                            }}
                            className="opacity-0 group-hover:opacity-100 p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-background transition-opacity shrink-0"
                            title="预览"
                          >
                            <Eye size={13} />
                          </button>
                        )}
                      </label>
                    );
                  })}
                </>
              )}
              {filteredNotes.length === 0 && filteredFiles.length === 0 && (
                <div className="flex items-center justify-center h-32 text-sm text-text-secondary/50">
                  没有匹配的笔记或文件
                </div>
              )}
            </>
          )}
        </div>

        <div className="flex items-center justify-between px-4 py-2.5 border-t border-surface-hover shrink-0">
          <span className="text-[11px] text-text-secondary/60">已选 {selected.size} 项</span>
          <div className="flex items-center gap-2">
            <button
              onClick={onClose}
              className="px-3 py-1.5 rounded-lg text-[12px] text-text-secondary hover:bg-surface-hover transition-colors"
            >
              取消
            </button>
            <button
              onClick={handleConfirm}
              disabled={resolving}
              className="px-3 py-1.5 rounded-lg text-[12px] bg-primary text-white hover:opacity-90 disabled:opacity-40 transition-opacity"
            >
              {resolving ? '读取中…' : '附加到消息'}
            </button>
          </div>
        </div>
      </div>

      {/* Stacked preview overlay for PDF/image files; sits above the picker so
          the selection state underneath is preserved. */}
      {previewId && (
        <div className="fixed inset-0 z-[300] flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/60 backdrop-blur-sm"
            onClick={() => setPreviewId(null)}
          />
          <div className="relative w-[860px] max-w-[94vw] h-[82vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
            <FilePreview fileId={previewId} onBack={() => setPreviewId(null)} />
          </div>
        </div>
      )}
    </div>
  );
}
