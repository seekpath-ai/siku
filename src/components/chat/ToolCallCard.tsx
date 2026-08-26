import { useState } from 'react';
import { Wrench, ChevronDown, ChevronRight, Loader2, CheckCircle2, XCircle, Clock } from 'lucide-react';
import type { ToolCallInfo } from '@/lib/types';
import { useActiveAgentName } from '@/hooks/useActiveAgentName';
import { ApprovalCard } from './ApprovalCard';
import { TerminalOutput } from './TerminalOutput';

interface Props {
  toolCall: ToolCallInfo;
}

const statusConfig: Record<string, { icon: React.ReactNode; label: string; color: string }> = {
  pending: { icon: <Clock size={14} />, label: '等待确认', color: 'text-codex-warning' },
  running: { icon: <Loader2 size={14} className="animate-spin" />, label: '执行中', color: 'text-codex-accent' },
  completed: { icon: <CheckCircle2 size={14} />, label: '已完成', color: 'text-codex-accent' },
  error: { icon: <XCircle size={14} />, label: '错误', color: 'text-codex-danger' },
  timeout: { icon: <Clock size={14} />, label: '超时', color: 'text-codex-warning' },
};

export function ToolCallCard({ toolCall }: Props) {
  const agentName = useActiveAgentName();
  const [expanded, setExpanded] = useState(false);
  const status = statusConfig[toolCall.status] ?? statusConfig.pending;

  if (toolCall.status === 'pending') {
    const command =
      typeof toolCall.arguments.command === 'string'
        ? toolCall.arguments.command
        : typeof toolCall.arguments.shell_command === 'string'
          ? toolCall.arguments.shell_command
          : JSON.stringify(toolCall.arguments);
    return (
      <div className="flex gap-4">
        <div className="w-8 h-8 rounded-full shrink-0 flex items-center justify-center text-[14px] font-semibold bg-gradient-to-br from-codex-accent to-emerald-700 text-black">
          C
        </div>
        <div className="flex-1 min-w-0 pt-1">
          <div className="flex items-center gap-2 mb-1">
            <span className="text-[13px] font-semibold text-codex-primary">{agentName}</span>
          </div>
          <ApprovalCard toolCallId={toolCall.id} toolName={toolCall.name} command={command} args={toolCall.arguments} />
        </div>
      </div>
    );
  }

  return (
    <div className="flex gap-4">
      <div className="w-8 h-8 rounded-full shrink-0 flex items-center justify-center text-[14px] font-semibold bg-gradient-to-br from-codex-accent to-emerald-700 text-black">
        C
      </div>
      <div className="flex-1 min-w-0 pt-1">
        <div className="flex items-center gap-2 mb-1">
          <span className="text-[13px] font-semibold text-codex-primary">{agentName}</span>
          <span className={`flex items-center gap-1 text-[11px] ${status.color}`}>
            {status.icon}
            <span>{status.label}</span>
          </span>
          {toolCall.duration_ms && (
            <span className="text-[11px] text-codex-muted">{toolCall.duration_ms}ms</span>
          )}
        </div>

        <div className="rounded-lg border border-codex-border bg-codex-surface overflow-hidden">
          <button
            onClick={() => setExpanded(!expanded)}
            className="flex items-center gap-2 px-3 py-2 w-full text-left hover:bg-codex-hover transition-colors"
          >
            <Wrench size={14} className="text-codex-secondary" />
            <span className="text-[13px] font-medium text-codex-primary">{toolCall.name}</span>
            {expanded ? <ChevronDown size={14} className="ml-auto text-codex-muted" /> : <ChevronRight size={14} className="ml-auto text-codex-muted" />}
          </button>

          {expanded && (
            <div className="px-3 py-2 border-t border-codex-border space-y-2">
              <div>
                <p className="text-[11px] text-codex-muted mb-1">参数:</p>
                <pre className="rounded-md bg-codex-code border border-codex-border p-2 text-[12px] text-codex-primary overflow-x-auto font-mono">
                  {JSON.stringify(toolCall.arguments, null, 2)}
                </pre>
              </div>
              {toolCall.result && (
                <TerminalOutput
                  output={toolCall.result}
                  status={toolCall.status}
                  command={toolCall.name}
                />
              )}
            </div>
          )}

          {!expanded && toolCall.result && (
            <TerminalOutput
              output={toolCall.result}
              status={toolCall.status}
              command={toolCall.name}
            />
          )}
        </div>
      </div>
    </div>
  );
}
