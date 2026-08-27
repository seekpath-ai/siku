import { useState } from 'react';
import { Terminal, Loader2, ChevronDown } from 'lucide-react';
import { useChatStore } from '@/stores/chatStore';
import { agentApproveTool, type ApprovalDecision } from '@/lib/tauri';

interface ApprovalCardProps {
  toolCallId: string;
  toolName: string;
  /** Display string of the call (command or JSON fallback). */
  command: string;
  /** Raw tool arguments, edited by the "修改参数" panel. */
  args: Record<string, unknown>;
  /** Session the approval belongs to; defaults to the main chat's active
   * session. The pet panel passes its own session id here. */
  sessionId?: string;
  /** Optimistic status update for the caller's streaming tool-call card;
   * defaults to the main chat store. */
  onDecision?: (decision: ApprovalDecision, localResult?: string) => void;
  /** Revert the optimistic update when the submit fails. */
  onSubmitFailed?: (decision: ApprovalDecision) => void;
  /** Compact spacing for narrow containers (pet panel). */
  compact?: boolean;
  /** Called after a successful submit (e.g. clear the caller's pending state). */
  onSubmitted?: () => void;
}

type Panel = 'none' | 'modify' | 'guide';

/** Long command displays collapse to a preview so the action buttons stay
 * visible without scrolling past a wall of JSON (e.g. note_write content). */
const COMMAND_COLLAPSE_CHARS = 500;
const COMMAND_PREVIEW_CHARS = 300;

/** Tool approval card: approve (optionally with edited arguments), decline &
 * continue, decline with guidance for the agent, or decline & end the turn. */
export function ApprovalCard({ toolCallId, toolName, command, args, sessionId, compact, onDecision, onSubmitFailed, onSubmitted }: ApprovalCardProps) {
  const { activeSessionId, updateStreamingToolCallById } = useChatStore();
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [declineOpen, setDeclineOpen] = useState(false);
  const [panel, setPanel] = useState<Panel>('none');
  const [argsText, setArgsText] = useState(() => JSON.stringify(args, null, 2));
  const [argsError, setArgsError] = useState<string | null>(null);
  const [guidance, setGuidance] = useState('');
  const [cmdExpanded, setCmdExpanded] = useState(false);
  // Code-point-safe preview (surrogate pairs must not be split).
  const cmdChars = [...command];
  const cmdLong = cmdChars.length > COMMAND_COLLAPSE_CHARS;
  const cmdPreview = cmdLong ? `${cmdChars.slice(0, COMMAND_PREVIEW_CHARS).join('')}…` : command;

  const submit = async (
    decision: ApprovalDecision,
    opts?: { guidance?: string; modifiedArgs?: Record<string, unknown> },
    localResult?: string
  ) => {
    const sid = sessionId ?? activeSessionId;
    if (!sid || isSubmitting) return;
    setIsSubmitting(true);
    if (onDecision) {
      onDecision(decision, localResult);
    } else if (decision === 'approve') {
      updateStreamingToolCallById(toolCallId, { status: 'running' });
    } else {
      updateStreamingToolCallById(toolCallId, {
        status: 'error',
        result: localResult ?? '用户拒绝了该操作',
      });
    }
    try {
      await agentApproveTool(sid, toolCallId, decision, opts);
      onSubmitted?.();
    } catch (err) {
      setIsSubmitting(false);
      // Only the approve path optimistically flipped the status; revert it.
      if (onSubmitFailed) {
        onSubmitFailed(decision);
      } else if (decision === 'approve') {
        updateStreamingToolCallById(toolCallId, { status: 'pending' });
      }
      console.error('approval submit failed:', err);
    }
  };

  const handleApproveModified = () => {
    try {
      const parsed = JSON.parse(argsText) as Record<string, unknown>;
      setArgsError(null);
      submit('approve', { modifiedArgs: parsed });
    } catch {
      setArgsError('JSON 格式错误，请检查后再试');
    }
  };

  const pickDecline = (mode: 'decline' | 'decline_guide' | 'decline_stop') => {
    setDeclineOpen(false);
    if (mode === 'decline_guide') {
      setPanel('guide');
      return;
    }
    if (mode === 'decline_stop') {
      submit('decline_stop', undefined, '用户拒绝并结束了本轮对话');
    } else {
      submit('decline');
    }
  };

  return (
    <div className={`rounded-lg border border-codex-border bg-codex-surface ${compact ? 'my-2 p-2.5' : 'my-3.5 p-3.5'}`}>
      <div className="flex items-center gap-2 text-[13px] font-semibold text-codex-primary mb-2">
        <Terminal size={14} className="text-codex-accent" />
        请求执行 · {toolName}
      </div>
      <div className={`rounded-md bg-codex-code border border-codex-border p-2.5 font-mono text-[12px] text-codex-secondary break-all whitespace-pre-wrap max-h-40 overflow-y-auto ${cmdLong ? 'mb-1' : 'mb-3'}`}>
        {cmdExpanded ? command : cmdPreview}
      </div>
      {cmdLong && (
        <button
          type="button"
          onClick={() => setCmdExpanded((v) => !v)}
          className="mb-2 text-[11px] text-codex-accent hover:underline"
        >
          {cmdExpanded ? '收起' : `展开全部(共 ${cmdChars.length} 字符)`}
        </button>
      )}

      {panel === 'modify' && (
        <div className="mb-3">
          <textarea
            value={argsText}
            onChange={(e) => setArgsText(e.target.value)}
            rows={Math.min(12, argsText.split('\n').length + 1)}
            spellCheck={false}
            className="w-full rounded-md bg-codex-code border border-codex-border p-2.5 font-mono text-[12px] text-codex-primary outline-none focus:border-codex-accent resize-y"
          />
          {argsError && <p className="mt-1 text-[11px] text-codex-danger">{argsError}</p>}
        </div>
      )}
      {panel === 'guide' && (
        <div className="mb-3">
          <textarea
            value={guidance}
            onChange={(e) => setGuidance(e.target.value)}
            rows={3}
            autoFocus
            placeholder="告诉智能体接下来应该怎么做…"
            className="w-full rounded-md bg-codex-code border border-codex-border p-2.5 text-[12px] text-codex-primary outline-none focus:border-codex-accent resize-y placeholder:text-codex-secondary/50"
          />
        </div>
      )}

      <div className="flex items-center gap-2.5 flex-wrap">
        {panel === 'modify' ? (
          <>
            <button
              onClick={handleApproveModified}
              disabled={isSubmitting}
              className="flex items-center gap-1.5 px-3.5 py-1.5 rounded-md bg-codex-accent text-black text-[13px] font-medium hover:bg-codex-accent-hover transition-colors disabled:opacity-60"
            >
              {isSubmitting && <Loader2 size={12} className="animate-spin" />}
              以修改后的参数执行
            </button>
            <button
              onClick={() => setPanel('none')}
              disabled={isSubmitting}
              className="px-3.5 py-1.5 rounded-md bg-transparent border border-codex-border text-codex-primary text-[13px] hover:bg-codex-hover transition-colors disabled:opacity-60"
            >
              返回
            </button>
          </>
        ) : panel === 'guide' ? (
          <>
            <button
              onClick={() =>
                guidance.trim() &&
                submit('decline_guide', { guidance: guidance.trim() }, `用户拒绝并给出指引:${guidance.trim()}`)
              }
              disabled={isSubmitting || !guidance.trim()}
              className="flex items-center gap-1.5 px-3.5 py-1.5 rounded-md bg-codex-accent text-black text-[13px] font-medium hover:bg-codex-accent-hover transition-colors disabled:opacity-60"
            >
              {isSubmitting && <Loader2 size={12} className="animate-spin" />}
              拒绝并发送指引
            </button>
            <button
              onClick={() => setPanel('none')}
              disabled={isSubmitting}
              className="px-3.5 py-1.5 rounded-md bg-transparent border border-codex-border text-codex-primary text-[13px] hover:bg-codex-hover transition-colors disabled:opacity-60"
            >
              返回
            </button>
          </>
        ) : (
          <>
            <button
              onClick={() => submit('approve')}
              disabled={isSubmitting}
              className="flex items-center gap-1.5 px-3.5 py-1.5 rounded-md bg-codex-accent text-black text-[13px] font-medium hover:bg-codex-accent-hover transition-colors disabled:opacity-60"
            >
              {isSubmitting && <Loader2 size={12} className="animate-spin" />}
              允许执行
            </button>
            <button
              onClick={() => setPanel('modify')}
              disabled={isSubmitting}
              className="px-3.5 py-1.5 rounded-md bg-transparent border border-codex-border text-codex-primary text-[13px] hover:bg-codex-hover transition-colors disabled:opacity-60"
              title="编辑工具参数后再执行"
            >
              修改参数
            </button>
            <div className="relative">
              <button
                onClick={() => setDeclineOpen((o) => !o)}
                disabled={isSubmitting}
                className="flex items-center gap-1 px-3.5 py-1.5 rounded-md bg-transparent border border-codex-border text-codex-primary text-[13px] hover:bg-codex-hover transition-colors disabled:opacity-60"
              >
                拒绝
                <ChevronDown size={13} />
              </button>
              {declineOpen && (
                <div className="absolute left-0 bottom-full z-50 mb-1 w-40 rounded-lg border border-codex-border bg-codex-surface shadow-xl py-1">
                  <button
                    onClick={() => pickDecline('decline')}
                    className="w-full text-left px-3 py-1.5 text-[12px] text-codex-primary hover:bg-codex-hover"
                  >
                    拒绝并继续
                  </button>
                  <button
                    onClick={() => pickDecline('decline_guide')}
                    className="w-full text-left px-3 py-1.5 text-[12px] text-codex-primary hover:bg-codex-hover"
                  >
                    拒绝并指引
                  </button>
                  <button
                    onClick={() => pickDecline('decline_stop')}
                    className="w-full text-left px-3 py-1.5 text-[12px] text-codex-danger hover:bg-codex-hover"
                  >
                    拒绝并结束
                  </button>
                </div>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
