import { useState } from 'react';
import { Brain, ChevronDown, ChevronRight } from 'lucide-react';
import type { AgentPhase } from '@/lib/types';
import { AgentPhaseCard } from './AgentPhaseCard';

interface Props {
  phases: AgentPhase[];
  streaming?: boolean;
  /** Start collapsed even while streaming (used by compact surfaces). */
  defaultCollapsed?: boolean;
}

export function ReasoningProcessCard({ phases, streaming, defaultCollapsed }: Props) {
  const [expanded, setExpanded] = useState(defaultCollapsed ? false : Boolean(streaming));

  const roundCount = new Set(phases.map((p) => p.step_index)).size;
  const toolCount = phases.filter((p) => p.kind === 'tool_call').length;

  return (
    <div className="rounded-lg border border-codex-border/70 bg-codex-surface/30 overflow-hidden mb-2">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-2 w-full px-3 py-2 text-left hover:bg-codex-hover/40 transition-colors"
      >
        <Brain size={14} className="text-codex-accent" />
        <span className="text-[12px] font-medium text-codex-secondary">
          {streaming ? '推理过程' : `推理过程 · ${roundCount} 轮`}
          {toolCount > 0 && ` · ${toolCount} 个工具`}
        </span>
        {streaming && (
          <span className="ml-auto flex items-center gap-1.5">
            <span className="w-1.5 h-1.5 rounded-full bg-codex-accent animate-pulse" />
            <span className="text-[11px] text-codex-muted">进行中</span>
          </span>
        )}
        <span className="ml-auto flex items-center">
          {expanded ? (
            <ChevronDown size={14} className="text-codex-muted" />
          ) : (
            <ChevronRight size={14} className="text-codex-muted" />
          )}
        </span>
      </button>

      {expanded && (
        <div className="px-3 py-2 border-t border-codex-border/50 space-y-2">
          {phases.map((phase, idx) => (
            <AgentPhaseCard key={`${phase.kind}-${phase.step_index}-${idx}`} phase={phase} streaming={streaming} />
          ))}
        </div>
      )}
    </div>
  );
}
