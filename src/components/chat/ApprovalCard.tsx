import { useState } from 'react';
import { Terminal, Loader2 } from 'lucide-react';
import { useChatStore } from '@/stores/chatStore';
import { agentApproveTool } from '@/lib/tauri';

interface ApprovalCardProps {
  toolCallId: string;
  command: string;
}

export function ApprovalCard({ toolCallId, command }: ApprovalCardProps) {
  const { activeSessionId, updateStreamingToolCallById } = useChatStore();
  const [isSubmitting, setIsSubmitting] = useState(false);

  const handleApprove = async () => {
    if (!activeSessionId || isSubmitting) return;
    setIsSubmitting(true);
    updateStreamingToolCallById(toolCallId, { status: 'running' });
    try {
      await agentApproveTool(activeSessionId, toolCallId, true);
    } catch (err) {
      setIsSubmitting(false);
      updateStreamingToolCallById(toolCallId, { status: 'pending' });
      console.error('Failed to approve tool:', err);
    }
  };

  const handleDecline = async () => {
    if (!activeSessionId || isSubmitting) return;
    setIsSubmitting(true);
    updateStreamingToolCallById(toolCallId, { status: 'error', result: '用户拒绝了该操作' });
    try {
      await agentApproveTool(activeSessionId, toolCallId, false);
    } catch (err) {
      console.error('Failed to decline tool:', err);
    }
  };

  return (
    <div className="my-3.5 rounded-lg border border-codex-border bg-codex-surface p-3.5">
      <div className="flex items-center gap-2 text-[13px] font-semibold text-codex-primary mb-2">
        <Terminal size={14} className="text-codex-accent" />
        请求执行命令
      </div>
      <div className="rounded-md bg-codex-code border border-codex-border p-2.5 font-mono text-[12px] text-codex-secondary break-all mb-3">
        {command}
      </div>
      <div className="flex items-center gap-2.5">
        <button
          onClick={handleApprove}
          disabled={isSubmitting}
          className="flex items-center gap-1.5 px-3.5 py-1.5 rounded-md bg-codex-accent text-black text-[13px] font-medium hover:bg-codex-accent-hover transition-colors disabled:opacity-60"
        >
          {isSubmitting && <Loader2 size={12} className="animate-spin" />}
          允许执行
        </button>
        <button
          onClick={handleDecline}
          disabled={isSubmitting}
          className="px-3.5 py-1.5 rounded-md bg-transparent border border-codex-border text-codex-primary text-[13px] hover:bg-codex-hover transition-colors disabled:opacity-60"
        >
          拒绝
        </button>
      </div>
    </div>
  );
}
