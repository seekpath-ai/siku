import { useState } from 'react';
import { Brain, ChevronDown, ChevronRight } from 'lucide-react';

interface Props {
  content: string;
  streaming?: boolean;
}

export function ReasoningBlock({ content, streaming }: Props) {
  const [expanded, setExpanded] = useState(streaming ? true : false);

  if (!content) return null;

  return (
    <div className="rounded-lg border border-codex-border/70 bg-codex-surface/40 overflow-hidden mb-2">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-2 w-full px-3 py-2 text-left hover:bg-codex-hover/50 transition-colors"
      >
        <Brain size={14} className="text-codex-accent" />
        <span className="text-[12px] font-medium text-codex-secondary">
          {streaming ? '正在思考' : '思考过程'}
        </span>
        {expanded ? (
          <ChevronDown size={14} className="ml-auto text-codex-muted" />
        ) : (
          <ChevronRight size={14} className="ml-auto text-codex-muted" />
        )}
      </button>

      {expanded && (
        <div className="px-3 py-2 border-t border-codex-border/50">
          <div className="prose prose-sm prose-invert max-w-none">
            <pre className="text-[12px] leading-relaxed whitespace-pre-wrap text-codex-secondary font-mono bg-transparent p-0 border-0">
              {content}
            </pre>
          </div>
        </div>
      )}
    </div>
  );
}
