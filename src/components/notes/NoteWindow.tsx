import { useEffect, useState } from 'react';
import { X } from 'lucide-react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { notesListAll, notesCreate, notesUpdate, notesDelete, notesGetBacklinks } from '@/lib/tauri';
import { NoteEditor } from '@/components/notes/NoteEditor';
import type { Note } from '@/lib/types';

interface Backlink {
  id: string;
  title: string;
  context: string;
  created_at: string;
}

/** Slim window that shows only the requested note (Obsidian-style "open in
 *  new window") — no sidebar, just a mini title bar + the editor. */
export function NoteWindow() {
  const [notes, setNotes] = useState<Note[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [activeNote, setActiveNote] = useState<Note | null>(null);
  const [backlinks, setBacklinks] = useState<Backlink[]>([]);

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const id = params.get('note');
    if (id) setActiveId(id);
    notesListAll()
      .then(setNotes)
      .catch((err) => console.error('note window load:', err));
  }, []);

  useEffect(() => {
    if (!activeId) {
      setActiveNote(null);
      setBacklinks([]);
      return;
    }
    const n = notes.find((x) => x.id === activeId);
    setActiveNote(n ?? null);
    if (n) {
      notesGetBacklinks(activeId)
        .then(setBacklinks)
        .catch(() => setBacklinks([]));
    } else {
      setBacklinks([]);
    }
  }, [activeId, notes]);

  const handleUpdate = async (id: string, title: string, content: string) => {
    await notesUpdate(id, title, content, undefined);
    setNotes(await notesListAll());
  };

  const handleDelete = async (id: string) => {
    await notesDelete(id);
    getCurrentWindow().close().catch(() => {});
  };

  return (
    <div className="h-screen w-screen rounded-xl overflow-hidden border border-surface-hover flex flex-col bg-background">
      {/* Mini title bar (drag + close) */}
      <div
        data-tauri-drag-region="deep"
        className="titlebar-drag flex items-center justify-end h-[36px] px-2 bg-surface border-b border-surface-hover shrink-0 select-none"
      >
        <button
          onClick={() => getCurrentWindow().close().catch(() => {})}
          className="w-8 h-8 flex items-center justify-center text-text-secondary hover:text-white hover:bg-red-500/80 transition-colors"
          title="关闭"
        >
          <X size={16} strokeWidth={1.5} />
        </button>
      </div>

      {activeNote ? (
        // The wrapper is load-bearing: NoteEditor's root carries BOTH flex-1
        // and h-full (100vh), so placed directly under the titlebar the window
        // content is 100vh+36px tall. Chromium scrolls overflow:hidden
        // ancestors on fragment navigation (clicking a [^n] footnote ref), and
        // that 36px of slack lets the scroll push the mini title bar — and its
        // close button / drag region — off the top of the window. flex-1 +
        // min-h-0 here makes the content exactly viewport-height again.
        <div className="flex-1 min-h-0 flex flex-col">
          <NoteEditor
            note={activeNote}
            notes={notes}
            onUpdate={handleUpdate}
            onNavigate={setActiveId}
            onCreateLink={async (title) => {
              const created = await notesCreate(title, '');
              setNotes(await notesListAll());
              setActiveId(created.id);
            }}
            onDelete={handleDelete}
            backlinkCount={backlinks.length}
            backlinks={backlinks}
          />
        </div>
      ) : (
        <div className="flex-1 flex items-center justify-center text-xs text-text-secondary">
          笔记不存在或已删除
        </div>
      )}
    </div>
  );
}
