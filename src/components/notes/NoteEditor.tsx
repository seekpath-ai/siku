import { useEffect, useMemo, useRef, useState, useCallback } from 'react';
import { useDialog } from '@/hooks/useDialog';
import {
  Check,
  Loader2,
  CircleAlert,
  BookOpen,
  Pen,
  MoreVertical,
  Link2,
  Columns2,
  Rows2,
  ExternalLink,
  Star,
  FileDown,
  Trash2,
  FolderOpen,
  History,
  Wand2,
  Code,
} from 'lucide-react';
import { WikiMarkdown } from './WikiMarkdown';
import { BacklinksPanel } from './BacklinksPanel';
import { VersionHistoryDialog } from './VersionHistoryDialog';
import { saveTextFile, fileBrowserRevealInSystem, noteVersionRestore, vaultAttachmentsDir } from '@/lib/tauri';
import { EditorView } from '@codemirror/view';
import { MarkdownEditor } from '@/components/editor/MarkdownEditor';
import type { Note, NoteVersion } from '@/lib/types';
import { parseNoteTags, parseNoteAliases } from '@/lib/types';

interface Props {
  note: Note;
  notes: Note[];
  onUpdate: (id: string, title: string, content: string) => Promise<void>;
  onUpdateAliases?: (id: string, aliases: string[]) => void;
  onNavigate: (id: string) => void;
  onCreateLink?: (title: string) => void;
  backlinkCount?: number;
  backlinks: { id: string; title: string; context: string; created_at: string }[];
  onDelete?: (id: string, confirmed?: boolean) => void;
  onToggleFavorite?: (id: string, fav: boolean) => void;
  onConvertMention?: (noteId: string) => void;
  /** Called after a version restore so the parent can refresh the note. */
  onVersionRestored?: () => void;
}

type SaveStatus = 'saved' | 'saving' | 'unsaved';
type ViewMode = 'edit' | 'source' | 'reading' | 'split-h' | 'split-v' | 'backlinks';

const SAVE_DEBOUNCE_MS = 800;

// ── Component ──────────────────────────────────────────────────────────

export function NoteEditor({ note, notes, onUpdate, onUpdateAliases, onNavigate, onCreateLink, backlinkCount = 0, backlinks, onDelete, onToggleFavorite, onConvertMention, onVersionRestored }: Props) {
  const [content, setContent] = useState(note.content);
  const [saveStatus, setSaveStatus] = useState<SaveStatus>('saved');
  const [cursorPos, setCursorPos] = useState(0);
  const [aliasInput, setAliasInput] = useState('');
  const [editingTitle, setEditingTitle] = useState(false);
  const [titleInput, setTitleInput] = useState(note.title);
  const [mode, setMode] = useState<ViewMode>('edit');
  const [attachmentsDir, setAttachmentsDir] = useState<string | undefined>(undefined);
  const { confirm } = useDialog();
  const [menuOpen, setMenuOpen] = useState(false);
  const [versionOpen, setVersionOpen] = useState(false);
  // Split-view synchronized scrolling (editor <-> preview).
  const viewRef = useRef<EditorView | null>(null);
  const previewScrollRef = useRef<HTMLDivElement | null>(null);
  const syncLockRef = useRef(false);
  const syncScroll = useCallback((source: 'editor' | 'preview') => {
    if (syncLockRef.current) return;
    const view = viewRef.current;
    const preview = previewScrollRef.current;
    if (!view || !preview) return;
    syncLockRef.current = true;
    try {
      const es = view.scrollDOM;
      const eh = Math.max(es.scrollHeight - es.clientHeight, 1);
      const ph = Math.max(preview.scrollHeight - preview.clientHeight, 1);
      if (source === 'editor') {
        preview.scrollTop = (es.scrollTop / eh) * ph;
      } else {
        es.scrollTop = (preview.scrollTop / ph) * eh;
      }
    } finally {
      setTimeout(() => {
        syncLockRef.current = false;
      }, 0);
    }
  }, []);
  const menuRef = useRef<HTMLDivElement>(null);
  const noteTags = useMemo(() => parseNoteTags(note), [note]);
  const aliases = useMemo(() => parseNoteAliases(note), [note]);

  // Breadcrumb path relative to the notes root (parent chain), like Obsidian.
  const notePath = useMemo(() => {
    const byId = new Map(notes.map((n) => [n.id, n]));
    const parts: { id: string; title: string }[] = [
      { id: note.id, title: note.title || '未命名笔记' },
    ];
    let cur = note.parent_id;
    let hops = 0;
    while (cur && byId.has(cur) && hops < 50) {
      const parent = byId.get(cur)!;
      parts.unshift({ id: parent.id, title: parent.title || '未命名笔记' });
      cur = parent.parent_id;
      hops += 1;
    }
    return parts;
  }, [notes, note]);

  const saveVersionRef = useRef(0);
  const savedVersionRef = useRef(0);
  const cursorPosRef = useRef(0);
  const onUpdateRef = useRef(onUpdate);

  useEffect(() => {
    onUpdateRef.current = onUpdate;
  }, [onUpdate]);

  const handleContentChange = useCallback((value: string) => {
    setContent(value);
    saveVersionRef.current += 1;
  }, []);

  // Reset local state when the active note changes
  useEffect(() => {
    setContent(note.content);
    setTitleInput(note.title);
    setEditingTitle(false);
    setSaveStatus('saved');
    saveVersionRef.current = 0;
    savedVersionRef.current = 0;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [note.id]);

  // Resolve the vault attachments directory for image rendering and paste/drop.
  useEffect(() => {
    let active = true;
    vaultAttachmentsDir(note.vault_id)
      .then((dir) => {
        if (active) setAttachmentsDir(dir);
      })
      .catch((err) => {
        console.error('attachments dir:', err);
        if (active) setAttachmentsDir(undefined);
      });
    return () => {
      active = false;
    };
  }, [note.vault_id]);

  // Commit a rename from the title row (single click to edit, Obsidian-style).
  const commitRename = useCallback(() => {
    setEditingTitle(false);
    const t = titleInput.trim();
    if (t && t !== note.title) {
      onUpdate(note.id, t, content).catch((err) => console.error('笔记重命名失败:', err));
    } else {
      setTitleInput(note.title);
    }
  }, [titleInput, note.title, note.id, content, onUpdate]);

  // Auto-save with debounce (the title row acts as the file name).
  useEffect(() => {
    if (saveVersionRef.current === savedVersionRef.current) return;

    setSaveStatus('unsaved');
    const timer = setTimeout(async () => {
      setSaveStatus('saving');
      const version = saveVersionRef.current;
      try {
        await onUpdateRef.current(note.id, note.title, content);
        savedVersionRef.current = version;
        setSaveStatus(saveVersionRef.current === version ? 'saved' : 'unsaved');
      } catch (err) {
        console.error('autosave:', err);
        setSaveStatus('unsaved');
      }
    }, SAVE_DEBOUNCE_MS);

    return () => clearTimeout(timer);
  }, [content, note.id, note.title]);

  const addAlias = useCallback(() => {
    const v = aliasInput.trim();
    if (!v || aliases.includes(v)) return;
    onUpdateAliases?.(note.id, [...aliases, v]);
    setAliasInput('');
  }, [aliasInput, aliases, onUpdateAliases, note.id]);

  const removeAlias = useCallback(
    (a: string) => onUpdateAliases?.(note.id, aliases.filter((x) => x !== a)),
    [aliases, onUpdateAliases, note.id]
  );

  // ── View mode & overflow menu ────────────────────────────────
  const toggleMode = useCallback((next: ViewMode) => setMode((m) => (m === next ? 'edit' : next)), []);

  // "导出为 PDF" — currently exports the note as Markdown (.md).
  // Restore a note from a version snapshot, then let the parent refresh.
  const handleVersionRestore = useCallback(
    async (version: NoteVersion) => {
      try {
        await noteVersionRestore(version.id);
        onVersionRestored?.();
      } catch (err) {
        console.error('restore version:', err);
      }
    },
    [onVersionRestored]
  );

  const handleExport = useCallback(async () => {    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const target = await save({
        defaultPath: `${note.title || '未命名笔记'}.md`,
        filters: [{ name: 'Markdown', extensions: ['md'] }],
      });
      if (!target) return;
      await saveTextFile(target, content);
    } catch (err) {
      console.error('export note:', err);
    }
  }, [note.title, content]);

  // Close the overflow menu when clicking outside it.
  useEffect(() => {
    if (!menuOpen) return;
    const onDown = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenuOpen(false);
      }
    };
    window.addEventListener('mousedown', onDown);
    return () => window.removeEventListener('mousedown', onDown);
  }, [menuOpen]);

  // Open the note in a separate Tauri window. The slim note window reads the
  // note id from its URL (?note=<id>) and renders only that note.
  const handleOpenInNewWindow = useCallback(async () => {
    try {
      const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
      const label = `note-${note.id}-${Date.now()}`;
      new WebviewWindow(label, {
        url: `index.html?note=${encodeURIComponent(note.id)}`,
        title: `${note.title || '未命名笔记'} - 思库`,
        width: 1000,
        height: 760,
        minWidth: 640,
        minHeight: 480,
        center: true,
        decorations: false,
        transparent: true,
        shadow: false,
      });
    } catch (err) {
      console.error('open note in new window:', err);
    }
  }, [note.id, note.title]);

  // Notes live in the database, so "reveal in system file manager" exports the
  // note to a .md file (user picks the location) and reveals that file.
  const handleReveal = useCallback(async () => {
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const target = await save({
        defaultPath: `${note.title || '未命名笔记'}.md`,
        filters: [{ name: 'Markdown', extensions: ['md'] }],
      });
      if (!target) return;
      await saveTextFile(target, content);
      await fileBrowserRevealInSystem(target);
    } catch (err) {
      console.error('reveal note:', err);
    }
  }, [note.title, content]);

  // Cursor position for the status bar.
  const cursorListener = useMemo(
    () =>
      EditorView.updateListener.of((u) => {
        if (u.selectionSet) {
          const head = u.state.selection.main.head;
          if (head !== cursorPosRef.current) {
            cursorPosRef.current = head;
            setCursorPos(head);
          }
        }
      }),
    []
  );

  const words = useMemo(() => {
    return content.trim() === '' ? 0 : content.trim().split(/\s+/).length;
  }, [content]);
  const chars = content.length;
  const { line, col } = useMemo(() => {
    const before = content.slice(0, cursorPos);
    const lines = before.split('\n');
    return { line: lines.length, col: lines[lines.length - 1].length + 1 };
  }, [content, cursorPos]);

  const saveIndicator = {
    saved: { icon: Check, text: '已保存', class: 'text-accent' },
    saving: { icon: Loader2, text: '保存中', class: 'text-primary animate-spin' },
    unsaved: { icon: CircleAlert, text: '未保存', class: 'text-yellow-400' },
  }[saveStatus];

  const editorEl = (
    <MarkdownEditor
      value={content}
      onChange={(value) => handleContentChange(value)}
      notes={notes}
      currentNoteId={note.id}
      onNavigate={onNavigate}
      onCreateLink={onCreateLink}
      // Source mode (Obsidian-style): raw markdown, no live-preview rendering.
      livePreview={mode !== 'source'}
      editorRef={(view) => {
        viewRef.current = view;
      }}
      onEditorScroll={() => syncScroll('editor')}
      extensions={[cursorListener]}
      vaultId={note.vault_id}
      attachmentsDir={attachmentsDir}
    />
  );
  const previewEl = (
    <div
      ref={previewScrollRef}
      onScroll={() => syncScroll('preview')}
      className="h-full overflow-y-auto p-8 md:px-16 prose prose-base prose-invert max-w-none"
    >
      <WikiMarkdown
        content={content || ' '}
        notes={notes}
        onNavigate={onNavigate}
        onCreateLink={onCreateLink}
        attachmentsDir={attachmentsDir}
      />
    </div>
  );

  return (
    <div className="flex-1 min-w-0 flex flex-col h-full bg-background">
      {/* Obsidian-style tab bar: centered breadcrumb path + view toggle + overflow menu */}
      <div className="relative flex items-center px-6 pt-2.5 pb-0.5 shrink-0">
        {note.agent_edit_count > 0 &&
          (note.agent_edited_at && note.updated_at > note.agent_edited_at ? (
            // AI edited, but manually modified afterwards — weakened badge.
            <span
              className="flex items-center gap-1 text-[10px] px-1.5 py-0.5 rounded bg-surface-hover text-text-secondary/70 shrink-0"
              title={`由 AI 整理于 ${note.agent_edited_at}，其后有人工修改（最后修改 ${note.updated_at}）`}
            >
              <Wand2 size={10} /> AI 已整理 · 已人工修改
            </span>
          ) : (
            <span
              className="flex items-center gap-1 text-[10px] px-1.5 py-0.5 rounded bg-primary/10 text-primary shrink-0"
              title={note.agent_edited_at ? `最近由 AI 整理：${note.agent_edited_at}` : '已由 AI 整理'}
            >
              <Wand2 size={10} /> AI 已整理
            </span>
          ))}
        <div
          className="absolute left-1/2 -translate-x-1/2 max-w-[55%] truncate text-[11px] text-text-secondary/60 flex items-center"
          title={notePath.map((p) => p.title).join(' / ')}
        >
          {notePath.map((part, idx) => (
            <span key={part.id} className="flex items-center">
              {idx > 0 && <span className="mx-1 text-text-secondary/30">/</span>}
              <button
                onClick={() => onNavigate(part.id)}
                className={`hover:text-text-primary hover:underline ${
                  idx === notePath.length - 1 ? 'text-text-secondary/80 font-medium' : ''
                }`}
              >
                {part.title}
              </button>
            </span>
          ))}
        </div>
        <div className="flex items-center gap-0.5 ml-auto shrink-0">
          <button
            onClick={() => toggleMode('reading')}
            title={
              mode === 'reading'
                ? '该标签页处于阅读视图中，点击此处切换至编辑视图'
                : mode === 'backlinks'
                  ? '该标签页处于反向链接视图中，点击此处切换至阅读视图'
                  : '该标签页处于编辑视图中，点击此处切换至阅读视图'
            }
            className="p-1.5 rounded hover:bg-surface-hover text-text-secondary hover:text-text-primary"
          >
            {mode === 'reading' ? <Pen size={13} /> : <BookOpen size={13} />}
          </button>
          <div className="relative" ref={menuRef}>
            <button
              onClick={() => setMenuOpen((o) => !o)}
              title="更多操作"
              className="p-1.5 rounded hover:bg-surface-hover text-text-secondary hover:text-text-primary"
            >
              <MoreVertical size={13} />
            </button>
            {menuOpen && (
              <div className="absolute right-0 top-full z-50 mt-1 w-52 bg-surface border border-surface-hover rounded-lg shadow-xl py-1">
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => {
                    setMenuOpen(false);
                    toggleMode('source');
                  }}
                >
                  <Code size={13} />
                  <span className="flex-1">源码模式</span>
                  {mode === 'source' && <Check size={13} className="text-primary" />}
                </button>
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => {
                    setMenuOpen(false);
                    toggleMode('backlinks');
                  }}
                >
                  <Link2 size={13} />
                  <span className="flex-1">在标签页中显示反向链接</span>
                  {mode === 'backlinks' && <Check size={13} className="text-primary" />}
                </button>
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => {
                    setMenuOpen(false);
                    toggleMode('split-h');
                  }}
                >
                  <Columns2 size={13} />
                  <span className="flex-1">左右分屏</span>
                  {mode === 'split-h' && <Check size={13} className="text-primary" />}
                </button>
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => {
                    setMenuOpen(false);
                    toggleMode('split-v');
                  }}
                >
                  <Rows2 size={13} />
                  <span className="flex-1">上下分屏</span>
                  {mode === 'split-v' && <Check size={13} className="text-primary" />}
                </button>
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => {
                    setMenuOpen(false);
                    handleOpenInNewWindow();
                  }}
                >
                  <ExternalLink size={13} /> 在新窗口中打开
                </button>
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => {
                    setMenuOpen(false);
                    onToggleFavorite?.(note.id, !note.is_favorite);
                  }}
                >
                  <Star size={13} className={note.is_favorite ? 'text-yellow-400' : ''} />
                  {note.is_favorite ? '取消收藏' : '收藏'}
                </button>
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => {
                    setMenuOpen(false);
                    handleExport();
                  }}
                >
                  <FileDown size={13} /> 导出为PDF
                </button>
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => {
                    setMenuOpen(false);
                    setVersionOpen(true);
                  }}
                >
                  <History size={13} /> 版本历史
                </button>
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-red-400 hover:bg-red-500/10 w-full text-left"
                  onClick={async () => {
                    setMenuOpen(false);
                    // Folder delete is recursive — count descendants so the
                    // confirmation can say they go too.
                    const ids = [note.id];
                    for (let i = 0; i < ids.length; i++) {
                      for (const n of notes) {
                        if (n.parent_id === ids[i]) ids.push(n.id);
                      }
                    }
                    const childCount = ids.length - 1;
                    const ok = await confirm(
                      childCount > 0
                        ? `确定删除文件夹「${note.title}」吗？其中的 ${childCount} 个子项将一并删除，此操作不可撤销。`
                        : `确定删除笔记「${note.title}」吗？此操作不可撤销。`
                    );
                    if (ok) onDelete?.(note.id, true);
                  }}
                >
                  <Trash2 size={13} /> 删除
                </button>
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => {
                    setMenuOpen(false);
                    handleReveal();
                  }}
                >
                  <FolderOpen size={13} /> 在系统资源管理器中显示
                </button>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Note title — single click to rename (Obsidian-style) */}
      <div className="px-6 pb-1 shrink-0">
        {editingTitle ? (
          <input
            autoFocus
            value={titleInput}
            onChange={(e) => setTitleInput(e.target.value)}
            onBlur={commitRename}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                commitRename();
              }
              if (e.key === 'Escape') {
                setEditingTitle(false);
                setTitleInput(note.title);
              }
            }}
            className="w-full bg-transparent text-center text-2xl font-bold text-text-primary outline-none border-b border-primary/30 pb-0.5"
          />
        ) : (
          <h1
            className="text-center text-2xl font-bold text-text-primary truncate cursor-text hover:text-accent transition-colors"
            title="单击重命名"
            onClick={() => {
              setTitleInput(note.title);
              setEditingTitle(true);
            }}
          >
            {note.title || '未命名笔记'}
          </h1>
        )}
      </div>

      {/* Tags & aliases */}
      {(noteTags.length > 0 || aliases.length > 0 || onUpdateAliases) && (
        <div className="flex flex-wrap items-center gap-1 px-6 pb-1 shrink-0">
          {noteTags.map((t) => (
            <span key={t} className="px-1.5 py-0.5 rounded text-[10px] bg-primary/10 text-primary">
              #{t}
            </span>
          ))}
          {aliases.map((a) => (
            <span
              key={a}
              className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] bg-surface-hover text-text-secondary"
            >
              {a}
              {onUpdateAliases && (
                <button onClick={() => removeAlias(a)} className="hover:text-red-400" aria-label="删除别名">
                  ×
                </button>
              )}
            </span>
          ))}
          {onUpdateAliases && (
            <input
              value={aliasInput}
              onChange={(e) => setAliasInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  addAlias();
                }
              }}
              onBlur={addAlias}
              placeholder="+ 别名"
              className="w-20 bg-transparent text-[11px] text-text-secondary placeholder:text-text-secondary/40 outline-none"
            />
          )}
        </div>
      )}

      {/* Editor / Reading / Split / Backlinks views */}
      <div className="flex-1 min-h-0 relative overflow-hidden">
        {mode === 'backlinks' ? (
          <BacklinksPanel
            activeNote={note}
            notes={notes}
            backlinks={backlinks}
            onNavigate={onNavigate}
            onConvertMention={onConvertMention}
          />
        ) : mode === 'reading' ? (
          previewEl
        ) : mode === 'split-h' || mode === 'split-v' ? (
          <div className={`flex h-full ${mode === 'split-v' ? 'flex-col' : ''}`}>
            <div className={`min-h-0 min-w-0 ${mode === 'split-h' ? 'w-1/2' : 'h-1/2'}`}>{editorEl}</div>
            <div className={mode === 'split-h' ? 'w-px bg-surface-hover' : 'h-px bg-surface-hover'} />
            <div className={`min-h-0 min-w-0 ${mode === 'split-h' ? 'w-1/2' : 'h-1/2'}`}>{previewEl}</div>
          </div>
        ) : (
          editorEl
        )}
      </div>

      {/* Status bar */}
      <div className="flex items-center justify-between px-4 py-1.5 border-t border-surface-hover text-[11px] text-text-secondary/70 gap-4 shrink-0">
        <div className={`flex items-center gap-1 ${saveIndicator.class}`}>
          <saveIndicator.icon size={12} />
          <span>{saveIndicator.text}</span>
        </div>
        <div className="flex items-center gap-4">
          <div className="flex items-center gap-1.5">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M4 11a9 9 0 0 1 9 9" />
              <path d="M4 4a16 16 0 0 1 16 16" />
              <circle cx="5" cy="19" r="1" />
            </svg>
            <span>{backlinkCount} 条反向链接</span>
          </div>
          <div className="flex items-center gap-1.5">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z" />
              <polyline points="14 2 14 8 20 8" />
            </svg>
            <span>{words} 个词</span>
          </div>
          <span>{chars} 个字符</span>
          <span>行 {line}, 列 {col}</span>
        </div>
      </div>

      {versionOpen && (
        <VersionHistoryDialog
          current={note}
          onRestore={handleVersionRestore}
          onClose={() => setVersionOpen(false)}
        />
      )}
    </div>
  );
}
