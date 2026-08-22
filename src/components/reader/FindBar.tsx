import { useEffect, useRef } from 'react';
import { Search, X, ChevronUp, ChevronDown } from 'lucide-react';

interface FindBarProps {
  query: string;
  onQueryChange: (q: string) => void;
  onNext: () => void;
  onPrev: () => void;
  onClose: () => void;
  matchIndex: number;   // 0-based; displays as +1. -1 when no matches or no query
  totalMatches: number;
  caseSensitive?: boolean;
  onCaseSensitiveChange?: (v: boolean) => void;
}

export function FindBar({
  query,
  onQueryChange,
  onNext,
  onPrev,
  onClose,
  matchIndex,
  totalMatches,
  caseSensitive = false,
  onCaseSensitiveChange,
}: FindBarProps) {
  const inputRef = useRef<HTMLInputElement>(null);

  // Auto-focus on mount
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      if (e.shiftKey) onPrev();
      else onNext();
    } else if (e.key === 'Escape') {
      onClose();
    }
  };

  const matchText = () => {
    if (!query.trim()) return null;
    if (totalMatches === 0) return (
      <span className="text-red-400 text-xs whitespace-nowrap">无结果</span>
    );
    return (
      <span className="text-text-secondary text-xs whitespace-nowrap tabular-nums">
        {matchIndex + 1} / {totalMatches}
      </span>
    );
  };

  return (
    <div className="absolute top-2 right-2 z-30 flex items-center gap-1.5 bg-surface border border-surface-hover rounded-lg shadow-xl px-3 py-1.5">
      <Search size={14} className="text-text-secondary shrink-0" />
      <input
        ref={inputRef}
        type="text"
        value={query}
        onChange={(e) => onQueryChange(e.target.value)}
        onKeyDown={handleKeyDown}
        placeholder="在 PDF 中搜索..."
        className="w-40 bg-transparent text-sm text-text-primary outline-none placeholder:text-text-secondary/40"
      />
      {matchText()}

      <button
        onClick={() => onCaseSensitiveChange?.(!caseSensitive)}
        title="区分大小写"
        className={`flex h-5 w-5 items-center justify-center rounded text-[10px] font-semibold transition-colors ${
          caseSensitive
            ? 'bg-primary/10 text-primary'
            : 'text-text-secondary hover:bg-surface-hover'
        }`}
      >
        Aa
      </button>

      <div className="flex items-center border-l border-surface-hover pl-1.5 gap-0.5">
        <button
          onClick={onPrev}
          disabled={totalMatches === 0}
          className="p-0.5 rounded hover:bg-surface-hover text-text-secondary disabled:opacity-30 disabled:cursor-not-allowed"
          title="上一个 (Shift+Enter)"
        >
          <ChevronUp size={14} />
        </button>
        <button
          onClick={onNext}
          disabled={totalMatches === 0}
          className="p-0.5 rounded hover:bg-surface-hover text-text-secondary disabled:opacity-30 disabled:cursor-not-allowed"
          title="下一个 (Enter)"
        >
          <ChevronDown size={14} />
        </button>
      </div>

      <button
        onClick={onClose}
        className="p-0.5 rounded hover:bg-surface-hover text-text-secondary ml-0.5"
        title="关闭 (Esc)"
      >
        <X size={14} />
      </button>
    </div>
  );
}
