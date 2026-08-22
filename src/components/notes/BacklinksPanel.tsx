import { useMemo } from 'react';
import { Link2, Unlink, Plus } from 'lucide-react';
import type { Note } from '@/lib/types';

interface Backlink {
  id: string;
  title: string;
  context: string;
  created_at: string;
}

interface Props {
  activeNote: Note;
  notes: Note[];
  backlinks: Backlink[];
  onNavigate: (id: string) => void;
  onConvertMention?: (noteId: string) => void;
}

interface UnlinkedMention {
  id: string;
  title: string;
  snippet: string;
}

function buildSnippet(text: string, query: string, radius = 40): string {
  const lower = text.toLowerCase();
  const idx = lower.indexOf(query.toLowerCase());
  if (idx === -1) return text.slice(0, radius * 2);
  const start = Math.max(0, idx - radius);
  const end = Math.min(text.length, idx + query.length + radius);
  const snippet = text.slice(start, end);
  return `${start > 0 ? '…' : ''}${snippet}${end < text.length ? '…' : ''}`;
}

/** Backlinks view rendered inside the note tab (Obsidian "Show backlinks"). */
export function BacklinksPanel({ activeNote, notes, backlinks, onNavigate, onConvertMention }: Props) {
  const linkedIds = useMemo(() => new Set(backlinks.map((bl) => bl.id)), [backlinks]);

  const unlinked = useMemo<UnlinkedMention[]>(() => {
    const title = activeNote.title.trim();
    if (!title) return [];

    return notes
      .filter((n) => n.id !== activeNote.id && !linkedIds.has(n.id))
      .map((n) => {
        const text = n.content_plain || '';
        const idx = text.toLowerCase().indexOf(title.toLowerCase());
        if (idx === -1) return null;
        return {
          id: n.id,
          title: n.title,
          snippet: buildSnippet(text, title),
        };
      })
      .filter((m): m is UnlinkedMention => m !== null);
  }, [activeNote, notes, linkedIds]);

  return (
    <div className="h-full overflow-y-auto">
      <div className="max-w-3xl mx-auto p-8 md:px-16">
        <h2 className="flex items-center gap-1.5 text-sm font-semibold text-text-primary mb-3">
          <Link2 size={14} />
          <span>反向链接 ({backlinks.length})</span>
        </h2>
        {backlinks.length === 0 ? (
          <p className="text-xs text-text-secondary/60 mb-6">暂无反向链接</p>
        ) : (
          <div className="space-y-2 mb-6">
            {backlinks.map((bl) => (
              <button
                key={bl.id}
                onClick={() => onNavigate(bl.id)}
                className="w-full text-left p-2 rounded-lg bg-surface border border-surface-hover hover:border-primary/30 transition-colors"
              >
                <p className="text-xs font-medium text-text-primary truncate">{bl.title}</p>
                {bl.context && <p className="text-xs text-text-secondary/60 mt-0.5 truncate">{bl.context}</p>}
              </button>
            ))}
          </div>
        )}

        <h2 className="flex items-center gap-1.5 text-sm font-semibold text-text-primary mb-3">
          <Unlink size={14} />
          <span>未链接提及 ({unlinked.length})</span>
        </h2>
        {unlinked.length === 0 ? (
          <p className="text-xs text-text-secondary/60">暂无未链接提及</p>
        ) : (
          <div className="space-y-2">
            {unlinked.map((m) => (
              <div
                key={m.id}
                className="group p-2 rounded-lg bg-surface border border-surface-hover hover:border-primary/30 transition-colors"
              >
                <button onClick={() => onNavigate(m.id)} className="w-full text-left">
                  <p className="text-xs font-medium text-text-primary truncate">{m.title}</p>
                  {m.snippet && <p className="text-xs text-text-secondary/60 mt-0.5 line-clamp-2">{m.snippet}</p>}
                </button>
                {onConvertMention && (
                  <button
                    onClick={() => onConvertMention(m.id)}
                    className="mt-1.5 flex items-center gap-1 text-[10px] text-text-secondary hover:text-primary transition-colors"
                    title="在该笔记开头插入指向当前笔记的链接"
                  >
                    <Plus size={10} />
                    <span>转为链接</span>
                  </button>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
