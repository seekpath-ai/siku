import { useEffect, useRef, useState } from 'react';
import { ChevronDown, ChevronUp, Search, X } from 'lucide-react';
import { useChatStore } from '@/stores/chatStore';
import { chatSearchMessages } from '@/lib/tauri';
import type { ChatMessageHit } from '@/lib/tauri';

/** In-conversation search bar (Ctrl+F): match count + prev/next navigation.
 *  Jumps go through the store's jumpTarget; MessageList scrolls and flashes. */
export function ChatSearchBar({ onClose }: { onClose: () => void }) {
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const [query, setQuery] = useState('');
  const [matches, setMatches] = useState<ChatMessageHit[]>([]);
  const [index, setIndex] = useState(-1);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Debounced in-session search; start from the newest match (chat reads
  // bottom-up).
  useEffect(() => {
    const q = query.trim();
    if (!q || !activeSessionId) {
      setMatches([]);
      setIndex(-1);
      return;
    }
    const t = setTimeout(() => {
      chatSearchMessages(q, activeSessionId, 50)
        .then((hits) => {
          const asc = [...hits].reverse(); // backend returns newest-first
          setMatches(asc);
          const start = asc.length - 1;
          setIndex(start);
          if (start >= 0) useChatStore.getState().setJumpTarget(asc[start].message_id);
        })
        .catch(() => {
          setMatches([]);
          setIndex(-1);
        });
    }, 300);
    return () => clearTimeout(t);
  }, [query, activeSessionId]);

  const step = (delta: number) => {
    if (matches.length === 0) return;
    const next = (index + delta + matches.length) % matches.length;
    setIndex(next);
    useChatStore.getState().setJumpTarget(matches[next].message_id);
  };

  return (
    <div className="shrink-0 flex items-center gap-2 px-5 py-1.5 border-b border-surface-hover bg-background">
      <Search size={13} className="shrink-0 text-text-secondary" />
      <input
        ref={inputRef}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            step(e.shiftKey ? -1 : 1);
          }
          if (e.key === 'Escape') {
            e.preventDefault();
            onClose();
          }
        }}
        placeholder="在当前对话中搜索"
        className="w-64 bg-surface border border-surface-hover rounded-lg px-2.5 py-1 text-[12px] text-text-primary placeholder:text-text-secondary/50 outline-none focus:border-primary/50"
      />
      <span className="text-[11px] text-text-secondary tabular-nums w-12 text-center">
        {matches.length > 0 ? `${index + 1}/${matches.length}` : '0/0'}
      </span>
      <button
        onClick={() => step(-1)}
        disabled={matches.length === 0}
        title="上一条（Shift+Enter）"
        className="p-1 rounded hover:bg-surface-hover text-text-secondary hover:text-text-primary disabled:opacity-40"
      >
        <ChevronUp size={14} />
      </button>
      <button
        onClick={() => step(1)}
        disabled={matches.length === 0}
        title="下一条（Enter）"
        className="p-1 rounded hover:bg-surface-hover text-text-secondary hover:text-text-primary disabled:opacity-40"
      >
        <ChevronDown size={14} />
      </button>
      <div className="flex-1" />
      <button
        onClick={onClose}
        title="关闭（Esc）"
        className="p-1 rounded hover:bg-surface-hover text-text-secondary hover:text-text-primary"
      >
        <X size={14} />
      </button>
    </div>
  );
}
