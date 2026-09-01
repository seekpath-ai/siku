import { useTabStore } from '@/stores/tabStore';

/** Minimal structural type compatible with TanStack Router's navigate. */
type Navigate = (opts: { to: string; search?: Record<string, unknown> }) => unknown;

/** Open a note in its own tab (one tab per note, deduped by id) and navigate
 *  to it. All "open note" entry points go through this so multi-note tabs
 *  behave consistently. */
export function openNoteTab(navigate: Navigate, note: { id: string; title?: string }) {
  useTabStore.getState().open({
    id: `note_${note.id}`,
    title: note.title || '未命名笔记',
    icon: 'note',
    route: '/notes',
    search: { note: note.id },
  });
  navigate({ to: '/notes', search: { note: note.id } });
}
