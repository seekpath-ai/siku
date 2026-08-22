import { useState } from 'react';
import { Brain, ChevronDown, ChevronRight, Wrench } from 'lucide-react';
import type { AgentPhase } from '@/lib/types';
import { ToolCallCard } from './ToolCallCard';

interface Props {
  phase: AgentPhase;
  streaming?: boolean;
}

export function AgentPhaseCard({ phase, streaming }: Props) {
  const [expanded, setExpanded] = useState(streaming || false);

  if (phase.kind === 'reasoning') {
    return (
      <div className="rounded-lg border border-codex-border/70 bg-codex-surface/30 overflow-hidden mb-2">
        <button
          onClick={() => setExpanded(!expanded)}
          className="flex items-center gap-2 w-full px-3 py-2 text-left hover:bg-codex-hover/40 transition-colors"
        >
          <span className="flex items-center justify-center w-5 h-5 rounded-md bg-codex-surface border border-codex-border text-[11px] font-medium text-codex-secondary">
            {phase.step_index}
          </span>
          <Brain size={14} className="text-codex-accent" />
          <span className="text-[12px] font-medium text-codex-secondary">思考过程</span>
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
          <div className="px-3 py-2 border-t border-codex-border/50">
            <pre className="text-[12px] leading-relaxed whitespace-pre-wrap text-codex-secondary font-mono bg-codex-surface/50 rounded-md p-2 border border-codex-border/50">
              {phase.content}
            </pre>
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="rounded-lg border border-codex-border/70 bg-codex-surface/30 overflow-hidden mb-2">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-2 w-full px-3 py-2 text-left hover:bg-codex-hover/40 transition-colors"
      >
        <span className="flex items-center justify-center w-5 h-5 rounded-md bg-codex-surface border border-codex-border text-[11px] font-medium text-codex-secondary">
          {phase.step_index}
        </span>
        <Wrench size={14} className="text-codex-accent" />
        <span className="text-[12px] font-medium text-codex-secondary">
          调用工具: {phase.toolCall.name}
        </span>
        {streaming && phase.toolCall.status !== 'completed' && phase.toolCall.status !== 'error' && (
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
        <div className="px-3 py-2 border-t border-codex-border/50">
          <ToolCallCard toolCall={phase.toolCall} />
        </div>
      )}
    </div>
  );
}
