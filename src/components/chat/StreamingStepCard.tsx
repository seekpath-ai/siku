import { useState } from 'react';
import { Brain, ChevronDown, ChevronRight, Wrench } from 'lucide-react';
import type { StreamingStep } from '@/lib/types';
import { ToolCallCard } from './ToolCallCard';

interface Props {
  step: StreamingStep;
}

export function StreamingStepCard({ step }: Props) {
  const isCompleted = step.status === 'completed';
  const [expanded, setExpanded] = useState(!isCompleted);
  const toolCalls = step.tool_calls;
  const hasReasoning = step.reasoning_content.trim().length > 0;

  return (
    <div className="rounded-lg border border-codex-border/70 bg-codex-surface/30 overflow-hidden mb-2">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-2 w-full px-3 py-2 text-left hover:bg-codex-hover/40 transition-colors"
      >
        <span className="flex items-center justify-center w-5 h-5 rounded-md bg-codex-surface border border-codex-border text-[11px] font-medium text-codex-secondary">
          {step.step_index}
        </span>
        {toolCalls.length > 0 ? (
          <Wrench size={14} className="text-codex-accent" />
        ) : (
          <Brain size={14} className="text-codex-accent" />
        )}
        <span className="text-[12px] font-medium text-codex-secondary">
          {toolCalls.length > 0
            ? `调用 ${toolCalls.length} 个工具`
            : hasReasoning
              ? '思考过程'
              : '推理步骤'}
        </span>
        <span className="ml-auto flex items-center gap-1.5">
          {!isCompleted && (
            <>
              <span className="w-1.5 h-1.5 rounded-full bg-codex-accent animate-pulse" />
              <span className="text-[11px] text-codex-muted">进行中</span>
            </>
          )}
          {expanded ? (
            <ChevronDown size={14} className="text-codex-muted" />
          ) : (
            <ChevronRight size={14} className="text-codex-muted" />
          )}
        </span>
      </button>

      {expanded && (
        <div className="px-3 py-2 border-t border-codex-border/50 space-y-3">
          {hasReasoning && (
            <div>
              <p className="text-[11px] text-codex-muted mb-1">思考：</p>
              <pre className="text-[12px] leading-relaxed whitespace-pre-wrap text-codex-secondary font-mono bg-codex-surface/50 rounded-md p-2 border border-codex-border/50">
                {step.reasoning_content}
              </pre>
            </div>
          )}

          {toolCalls.length > 0 && (
            <div className="space-y-2">
              <p className="text-[11px] text-codex-muted">工具调用：</p>
              {toolCalls.map((tc) => (
                <ToolCallCard key={tc.id} toolCall={tc} />
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
