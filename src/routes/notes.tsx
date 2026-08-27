import { useState, useEffect, useCallback } from 'react';
import { createRoute, useNavigate } from '@tanstack/react-router';
import { listen } from '@tauri-apps/api/event';
import { Route as RootRoute } from './__root';
import { NoteList } from '@/components/notes/NoteList';
import { NoteEditor } from '@/components/notes/NoteEditor';
import { FilePreview } from '@/components/files/FilePreview';
import { VaultSwitcher } from '@/components/notes/VaultSwitcher';
import { HelpDialog } from '@/components/notes/HelpDialog';
import { NotesSettingsModal } from '@/components/notes/NotesSettingsModal';
import { ListPanel } from '@/components/layout/ListPanel';
import { Folder } from 'lucide-react';
import { useShellStore } from '@/stores/shellStore';
import { useDialog } from '@/hooks/useDialog';
import { usePetContextStore } from '@/stores/petContextStore';
import { useTabStore } from '@/stores/tabStore';
import type { Note, Vault, FileItem } from '@/lib/types';
import {
  notesListAll,
  notesCreate,
  notesUpdate,
  notesDelete,
  notesMove,
  notesGetBacklinks,
  vaultList,
  vaultCurrent,
  vaultCreate,
  vaultRename,
  vaultDelete,
  vaultSetCurrent,
  vaultExport,
  vaultImport,
  filesList,
  filesImport,
  filesMove,
  filesRename,
  filesDelete,
  filesOpen,
} from '@/lib/tauri';
import { pickDirectory } from '@/lib/pickDirectory';

function NotesPage() {
  const [notes, setNotes] = useState<Note[]>([]);
  const [files, setFiles] = useState<FileItem[]>([]);
  const [activeFileId, setActiveFileId] = useState<string | null>(null);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [activeNote, setActiveNote] = useState<Note | null>(null);
  const [backlinks, setBacklinks] = useState<{ id: string; title: string; context: string; created_at: string }[]>([]);
  const [vaults, setVaults] = useState<Vault[]>([]);
  const [currentVault, setCurrentVault] = useState<Vault | null>(null);
  const [vaultOpen, setVaultOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // Vault import progress (null = no import running).
  const [importProgress, setImportProgress] = useState<{ current: number; total: number; name: string } | null>(null);
  // Bumped after a version restore so NoteEditor remounts with fresh data.
  const [editorKey, setEditorKey] = useState(0);
  const { setSidePanelCollapsed } = useShellStore();
  const { alert, confirm } = useDialog();
  const navigate = useNavigate();
  const { note: noteParam } = Route.useSearch();

  useEffect(() => { loadNotes(); }, []);

  // Reload notes when a new note is created from outside the notes page (e.g. Ctrl+N).
  useEffect(() => {
    const handler = () => loadNotes();
    window.addEventListener('siku:note-created', handler);
    return () => window.removeEventListener('siku:note-created', handler);
  }, []);

  // Activate a note passed via URL (?note=<id>) — used by "open in new window".
  useEffect(() => {
    if (noteParam) setActiveId(noteParam);
  }, [noteParam]);

  const loadVaults = async () => {
    try {
      const [list, current] = await Promise.all([vaultList(), vaultCurrent()]);
      setVaults(list);
      setCurrentVault(current);
    } catch (err) {
      console.error('load vaults:', err);
    }
  };

  useEffect(() => { loadVaults(); }, []);

  const handleSwitchVault = async (id: string) => {
    try {
      const v = await vaultSetCurrent(id);
      setCurrentVault(v);
      setActiveId(null);
      setActiveFileId(null);
      setActiveNote(null);
      setBacklinks([]);
      await loadNotes();
    } catch (err) {
      console.error('switch vault:', err);
    }
  };

  const handleCreateVault = async (name: string) => {
    try {
      const v = await vaultCreate(name);
      await vaultSetCurrent(v.id);
      setCurrentVault(v);
      await Promise.all([loadNotes(), loadVaults()]);
      setActiveId(null);
      setActiveFileId(null);
      setActiveNote(null);
      setBacklinks([]);
    } catch (err) {
      console.error('create vault:', err);
    }
  };

  const handleRenameVault = async (id: string, name: string) => {
    try {
      const v = await vaultRename(id, name);
      if (currentVault?.id === id) setCurrentVault(v);
      await loadVaults();
    } catch (err) {
      console.error('rename vault:', err);
    }
  };

  const handleDeleteVault = async (id: string) => {
    try {
      await vaultDelete(id);
      if (currentVault?.id === id) {
        setActiveId(null);
        setActiveFileId(null);
        setActiveNote(null);
        setBacklinks([]);
        await Promise.all([loadNotes(), loadVaults()]);
      } else {
        await loadVaults();
      }
    } catch (err) {
      console.error('delete vault:', err);
    }
  };

  const handleExportVault = async () => {
    if (!currentVault) return;
    const dir = await pickDirectory();
    if (!dir) return;
    try {
      const n = await vaultExport(currentVault.id, dir);
      await alert(`已导出 ${n} 篇笔记到：\n${dir}`, '导出完成');
    } catch (err) {
      console.error('export vault:', err);
      await alert(`导出失败：${err}`, '导出失败');
    }
  };

  const handleImportVault = async () => {
    if (!currentVault) return;
    const dir = await pickDirectory();
    if (!dir) return;
    const ok = await confirm(`将把「${dir}」中的笔记和文件导入当前库「${currentVault.name}」，继续吗？`);
    if (!ok) return;
    setImportProgress({ current: 0, total: 0, name: '' });
    const unlisten = await listen<{ current: number; total: number; name: string }>(
      'vault:import_progress',
      (e) => setImportProgress(e.payload)
    );
    try {
      const r = await vaultImport(currentVault.id, dir);
      await Promise.all([loadNotes(), loadFiles()]);
      const parts = [`已导入 ${r.imported} 篇笔记`];
      if (r.files_imported > 0) parts.push(`${r.files_imported} 个文件`);
      if (r.unchanged > 0) parts.push(`${r.unchanged} 项内容未变化已跳过`);
      if (r.skipped > 0) parts.push(`${r.skipped} 个文件导入失败`);
      await alert(parts.join('，'), '导入完成');
    } catch (err) {
      console.error('import vault:', err);
      await alert(`导入失败：${err}`, '导入失败');
    } finally {
      unlisten();
      setImportProgress(null);
    }
  };

  const loadNotes = async () => {
    try {
      setNotes(await notesListAll());
    } catch (err) {
      console.error('load notes:', err);
    }
  };

  const loadFiles = useCallback(async () => {
    if (!currentVault) return;
    try {
      setFiles(await filesList(currentVault.id));
    } catch (err) {
      console.error('load files:', err);
    }
  }, [currentVault]);

  // Reload the vault's managed files when the current vault is known/switched.
  useEffect(() => { loadFiles(); }, [loadFiles]);

  // Reload note/file lists when sync applies remote changes. Debounced
  // because changesets and mailbox batches can arrive in bursts.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | null = null;
    const unlisten = listen('sync:remote_applied', () => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        loadNotes();
        loadFiles();
      }, 500);
    });
    return () => {
      if (timer) clearTimeout(timer);
      unlisten.then((fn) => fn());
    };
  }, [loadFiles]);

  // Reload the open note when the AI (note_write via the pet agent) edits it.
  // The list reload also refreshes activeNote through the [activeId, notes]
  // effect; NoteEditor decides whether to adopt the new content (it skips
  // while the user has unsaved edits).
  useEffect(() => {
    const unlisten = listen<{ id: string }>('note:changed', () => {
      loadNotes();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    if (activeId) {
      const n = notes.find((note) => note.id === activeId);
      setActiveNote(n || null);
      // Expose the focused note to the global pet.
      usePetContextStore.getState().setContext(
        n ? { page: 'notes', objectId: n.id, title: n.title || '未命名笔记' } : null
      );
      loadBacklinks(activeId);
    } else {
      setActiveNote(null);
      setBacklinks([]);
      usePetContextStore.getState().setContext(null);
    }
  }, [activeId, notes]);

  // Clear the pet context when leaving the notes page.
  useEffect(() => () => usePetContextStore.getState().setContext(null), []);

  const loadBacklinks = async (id: string) => {
    try {
      setBacklinks(await notesGetBacklinks(id));
    } catch {
      setBacklinks([]);
    }
  };

  const handleCreate = async (parentId?: string) => {
    try {
      const note = await notesCreate('新笔记', '', undefined, parentId);
      await loadNotes();
      setActiveId(note.id);
    } catch (err) {
      console.error('create note:', err);
    }
  };

  /** Create a folder (named "未命名文件夹") and return its id so the list can
   *  immediately enter rename mode — Obsidian-style. */
  const handleCreateFolder = async (): Promise<string> => {
    try {
      const note = await notesCreate('未命名文件夹', '', undefined, undefined, true);
      await loadNotes();
      return note.id;
    } catch (err) {
      console.error('create folder:', err);
      return '';
    }
  };

  const handleCreateSubNote = async (parentId: string) => {
    try {
      const note = await notesCreate('新子笔记', '', undefined, parentId);
      await loadNotes();
      setActiveId(note.id);
    } catch (err) {
      console.error('create sub-note:', err);
    }
  };

  const handleCreateSubFolder = async (parentId: string): Promise<string> => {
    try {
      const note = await notesCreate('未命名文件夹', '', undefined, parentId, true);
      await loadNotes();
      return note.id;
    } catch (err) {
      console.error('create sub-folder:', err);
      return '';
    }
  };

  const handleRename = async (id: string, title: string) => {
    try {
      await notesUpdate(id, title, undefined, undefined, undefined, undefined, false);
      await loadNotes();
    } catch (err) {
      console.error('rename note:', err);
    }
  };

  const handleUpdate = async (id: string, title: string, content: string) => {
    try {
      await notesUpdate(id, title, content, undefined);
      await loadNotes();
    } catch (err) {
      console.error('update note:', err);
      throw err;
    }
  };

  const handleUpdateAliases = async (id: string, aliases: string[]) => {
    try {
      await notesUpdate(id, undefined, undefined, undefined, JSON.stringify(aliases));
      await loadNotes();
    } catch (err) {
      console.error('update aliases:', err);
    }
  };

  /** Ids of a note/folder plus all its descendants (recursive delete target). */
  const collectSubtreeIds = (rootId: string): string[] => {
    const ids = [rootId];
    for (let i = 0; i < ids.length; i++) {
      for (const n of notes) {
        if (n.parent_id === ids[i]) ids.push(n.id);
      }
    }
    return ids;
  };

  /** `confirmed = true` when the caller (e.g. NoteEditor) already showed a
   *  confirmation dialog. The backend deletes the whole subtree recursively. */
  const handleDelete = async (id: string, confirmed?: boolean) => {
    const subtree = collectSubtreeIds(id);
    if (!confirmed) {
      const target = notes.find((n) => n.id === id);
      const childCount = subtree.length - 1;
      const ok = await confirm(
        childCount > 0
          ? `确定删除文件夹「${target?.title ?? ''}」吗？其中的 ${childCount} 个子项将一并删除，此操作不可撤销。`
          : `确定删除「${target?.title ?? ''}」吗？此操作不可撤销。`
      );
      if (!ok) return;
    }
    try {
      await notesDelete(id);
      if (activeId && subtree.includes(activeId)) {
        setActiveId(null);
        setActiveNote(null);
      }
      // Close any open tabs for the deleted note and its descendants.
      const tabStore = useTabStore.getState();
      for (const nid of subtree) tabStore.close(`note_${nid}`);
      // Deleting a folder also removes the managed files inside it.
      await Promise.all([loadNotes(), loadFiles()]);
    } catch (err) {
      console.error('delete note:', err);
    }
  };

  /** Bulk delete with a single confirmation covering the union of subtrees. */
  const handleBulkDelete = async (ids: string[]) => {
    const all = new Set<string>();
    for (const id of ids) {
      for (const nid of collectSubtreeIds(id)) all.add(nid);
    }
    const ok = await confirm(
      all.size > ids.length
        ? `确定删除选中的 ${ids.length} 项吗？连同子项共 ${all.size} 个对象将被删除，此操作不可撤销。`
        : `确定删除选中的 ${ids.length} 项吗？此操作不可撤销。`
    );
    if (!ok) return;
    try {
      for (const id of ids) await notesDelete(id);
      if (activeId && all.has(activeId)) {
        setActiveId(null);
        setActiveNote(null);
      }
      const tabStore = useTabStore.getState();
      for (const nid of all) tabStore.close(`note_${nid}`);
      await Promise.all([loadNotes(), loadFiles()]);
    } catch (err) {
      console.error('bulk delete notes:', err);
    }
  };

  const handleMoveToRoot = async (id: string) => {
    try {
      await notesMove(id, null);
      await loadNotes();
    } catch (err) {
      console.error('move to root:', err);
    }
  };

  const handleMoveToFolder = async (id: string, parentId: string | null) => {
    try {
      await notesMove(id, parentId);
      await loadNotes();
    } catch (err) {
      console.error('move note:', err);
    }
  };

  const handleToggleFavorite = async (id: string, fav: boolean) => {
    try {
      await notesUpdate(id, undefined, undefined, undefined, undefined, fav ? 1 : 0);
      await loadNotes();
    } catch (err) {
      console.error('toggle favorite:', err);
    }
  };

  const handleBulkCreateFolder = async (ids: string[]): Promise<string> => {
    try {
      // Find a common parent if all selected notes share one; otherwise root.
      const selected = notes.filter((n) => ids.includes(n.id));
      const parents = new Set(selected.map((n) => n.parent_id ?? null));
      const parentId = parents.size === 1 ? ([...parents][0] as string | null) : null;
      const folder = await notesCreate('未命名文件夹', '', undefined, parentId ?? undefined, true);
      for (const id of ids) {
        await notesMove(id, folder.id);
      }
      await loadNotes();
      return folder.id;
    } catch (err) {
      console.error('bulk create folder:', err);
      return '';
    }
  };

  const handleBulkMove = async (ids: string[], parentId: string | null) => {
    try {
      for (const id of ids) {
        await notesMove(id, parentId);
      }
      await loadNotes();
    } catch (err) {
      console.error('bulk move:', err);
    }
  };

  // Drop the inline preview when the previewed file disappears (deleted
  // directly or via a parent folder delete / vault switch).
  useEffect(() => {
    if (activeFileId && !files.some((f) => f.id === activeFileId)) {
      setActiveFileId(null);
    }
  }, [files, activeFileId]);

  // ── Vault-managed files ─────────────────────────────────────────────

  /** Import OS-dropped files into the current vault under a folder (null = root). */
  const handleFileImport = async (paths: string[], parentId: string | null) => {
    if (!currentVault) return;
    const failed: string[] = [];
    for (const p of paths) {
      try {
        await filesImport(currentVault.id, p, parentId);
      } catch (err) {
        console.error('import file:', err);
        failed.push(p);
      }
    }
    if (failed.length > 0) {
      await alert(`以下文件导入失败：\n${failed.join('\n')}`, '导入失败');
    }
    await loadFiles();
  };

  const handleFileMove = async (id: string, parentId: string | null) => {
    try {
      await filesMove(id, parentId);
      await loadFiles();
    } catch (err) {
      console.error('move file:', err);
    }
  };

  const handleFileRename = async (id: string, name: string) => {
    try {
      await filesRename(id, name);
      // Keep an open preview tab's title in sync.
      useTabStore.getState().updateTab(`file-${id}`, { title: name });
      await loadFiles();
    } catch (err) {
      console.error('rename file:', err);
    }
  };

  const handleFileDelete = async (id: string) => {
    const target = files.find((f) => f.id === id);
    const ok = await confirm(`确定删除「${target?.name ?? ''}」吗？此操作不可撤销。`);
    if (!ok) return;
    try {
      await filesDelete(id);
      if (activeFileId === id) setActiveFileId(null);
      useTabStore.getState().close(`file-${id}`);
      await loadFiles();
    } catch (err) {
      console.error('delete file:', err);
    }
  };

  const handleFileOpen = async (id: string) => {
    // Double-click: previewable files open in a dedicated tab (like papers in
    // the reader); known binary formats go straight to the system application.
    const f = files.find((x) => x.id === id);
    const name = f?.name.toLowerCase() ?? '';
    const isKnownBinary = /\.(docx?|xlsx?|pptx?|odt|ods|odp|zip|gz|7z|rar|tar|mp3|mp4|mov|avi|mkv|exe|dll|so|dylib|sqlite3?|db)$/.test(name);
    if (f && !isKnownBinary) {
      useTabStore.getState().open({
        id: `file-${id}`,
        title: f.name,
        icon: 'pdf',
        route: '/file/$fileId',
        params: { fileId: id },
      });
      navigate({ to: '/file/$fileId', params: { fileId: id } });
      return;
    }
    try {
      await filesOpen(id);
    } catch (err) {
      console.error('open file:', err);
      await alert(`打开文件失败：${err}`, '打开失败');
    }
  };

  /** Single-click a file: inline preview in the right pane (tree stays put). */
  const handleFileSelect = (id: string) => {
    setActiveFileId(id);
    setActiveId(null);
  };

  const handleCreateLink = async (title: string) => {
    try {
      const note = await notesCreate(title, '', undefined, undefined);
      await loadNotes();
      setActiveId(note.id);
    } catch (err) {
      console.error('create note from link:', err);
    }
  };

  const handleVersionRestored = async () => {
    await loadNotes();
    setEditorKey((k) => k + 1);
  };

  const handleConvertMention = async (noteId: string) => {
    if (!activeNote) return;
    const note = notes.find((n) => n.id === noteId);
    if (!note) return;
    const link = `[[${activeNote.title}]]`;
    const newContent = note.content.trimStart().startsWith(link)
      ? note.content
      : `${link}\n\n${note.content}`;
    try {
      await notesUpdate(noteId, undefined, newContent, undefined);
      await loadNotes();
      if (activeId) {
        loadBacklinks(activeId);
      }
    } catch (err) {
      console.error('convert mention:', err);
    }
  };

  return (
    <div className="flex h-full">
      <ListPanel width={260}>
        <NoteList
          notes={notes}
          files={files}
          activeNoteId={activeId}
          vaultId={currentVault?.id}
          onSelect={(id) => {
            setActiveId(id);
            setActiveFileId(null);
          }}
          onCreate={handleCreate}
          onCreateFolder={handleCreateFolder}
          onCreateSubNote={handleCreateSubNote}
          onCreateSubFolder={handleCreateSubFolder}
          onRename={handleRename}
          onDelete={handleDelete}
          onBulkDelete={handleBulkDelete}
          onToggleFavorite={handleToggleFavorite}
          onMoveToRoot={handleMoveToRoot}
          onMoveToFolder={handleMoveToFolder}
          onBulkCreateFolder={handleBulkCreateFolder}
          onBulkMove={handleBulkMove}
          onFileImport={handleFileImport}
          onFileSelect={handleFileSelect}
          onFileMove={handleFileMove}
          onFileRename={handleFileRename}
          onFileDelete={handleFileDelete}
          onFileOpen={handleFileOpen}
          onClose={() => setSidePanelCollapsed(true)}
          currentVaultName={currentVault?.name ?? 'cognitive-archive'}
          onOpenVault={() => setVaultOpen(true)}
          onOpenHelp={() => setHelpOpen(true)}
          onOpenSettings={() => setSettingsOpen(true)}
        />
      </ListPanel>
      <div className="flex-1 flex min-w-0 bg-background">
        {activeFileId ? (
          <FilePreview fileId={activeFileId} />
        ) : activeNote ? (
          <NoteEditor
            key={editorKey}
            note={activeNote}
            notes={notes}
            onUpdate={handleUpdate}
            onUpdateAliases={handleUpdateAliases}
            onNavigate={setActiveId}
            onCreateLink={handleCreateLink}
            onDelete={handleDelete}
            onToggleFavorite={handleToggleFavorite}
            backlinkCount={backlinks.length}
            backlinks={backlinks}
            onConvertMention={handleConvertMention}
            onVersionRestored={handleVersionRestored}
          />
        ) : (
          <div className="flex flex-col items-center justify-center h-full w-full text-text-secondary">
            <Folder size={48} className="mb-3 opacity-30" />
            <p className="text-sm">选择或新建一篇笔记</p>
          </div>
        )}
      </div>

      {vaultOpen && (
        <VaultSwitcher
          vaults={vaults}
          currentVaultId={currentVault?.id ?? null}
          onSwitch={handleSwitchVault}
          onCreate={handleCreateVault}
          onRename={handleRenameVault}
          onDelete={handleDeleteVault}
          onExport={handleExportVault}
          onImport={handleImportVault}
          onClose={() => setVaultOpen(false)}
        />
      )}
      {helpOpen && <HelpDialog onClose={() => setHelpOpen(false)} />}
      {settingsOpen && <NotesSettingsModal onClose={() => setSettingsOpen(false)} />}

      {/* Vault import progress */}
      {importProgress && (
        <div className="fixed inset-0 z-[200] flex items-center justify-center">
          <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" />
          <div className="relative w-[380px] bg-surface border border-surface-hover rounded-xl shadow-2xl p-5">
            <div className="text-sm font-medium text-text-primary mb-3">正在导入库…</div>
            <div className="h-1.5 rounded-full bg-surface-hover overflow-hidden">
              <div
                className="h-full bg-primary transition-all duration-150"
                style={{
                  width: `${importProgress.total > 0 ? Math.round((importProgress.current / importProgress.total) * 100) : 0}%`,
                }}
              />
            </div>
            <div className="flex items-center justify-between gap-2 mt-2 text-xs text-text-secondary">
              <span className="truncate flex-1">{importProgress.name || '扫描目录…'}</span>
              <span className="shrink-0 tabular-nums">
                {importProgress.current} / {importProgress.total || '…'}
              </span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/notes',
  validateSearch: (search: Record<string, unknown>) => ({
    note: typeof search.note === 'string' ? search.note : undefined,
  }),
  component: NotesPage,
});
