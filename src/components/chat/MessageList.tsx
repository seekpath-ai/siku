import { useEffect, useMemo, useRef } from 'react';
import { useChatStore } from '@/stores/chatStore';
import type { AgentPhase, StreamingStep } from '@/lib/types';
import { MessageBubble } from './MessageBubble';
import { StreamingContent } from './StreamingContent';
import { ReasoningProcessCard } from './ReasoningProcessCard';
import { useActiveAgentName } from '@/hooks/useActiveAgentName';
import { Brain } from 'lucide-react';

const NEAR_BOTTOM_THRESHOLD = 120; // px

function streamingToPhases(steps: StreamingStep[], current: StreamingStep | null): AgentPhase[] {
  const phases: AgentPhase[] = [];
  for (const step of steps) {
    if (step.reasoning_content.trim()) {
      phases.push({ kind: 'reasoning', step_index: step.step_index, content: step.reasoning_content });
    }
    for (const tc of step.tool_calls) {
      phases.push({ kind: 'tool_call', step_index: step.step_index, toolCall: tc });
    }
  }
  if (current) {
    if (current.reasoning_content.trim()) {
      phases.push({ kind: 'reasoning', step_index: current.step_index, content: current.reasoning_content });
    }
    for (const tc of current.tool_calls) {
      phases.push({ kind: 'tool_call', step_index: current.step_index, toolCall: tc });
    }
  }
  return phases;
}

export function MessageList() {
  const agentName = useActiveAgentName();
  const messages = useChatStore((s) => s.messages);
  const agentSteps = useChatStore((s) => s.agentSteps);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const streamContent = useChatStore((s) => s.streamContent);
  const streamingSteps = useChatStore((s) => s.streamingSteps);
  const currentStreamingStep = useChatStore((s) => s.currentStreamingStep);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const containerRef = useRef<HTMLDivElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const prevSessionIdRef = useRef<string | null>(null);
  const lastMessageIdRef = useRef<string | null>(null);
  const justSwitchedRef = useRef(false);
  const isNearBottomRef = useRef(true);

  const stepsByMessageId = useMemo(() => {
    const map = new Map<string, typeof agentSteps>();
    for (const step of agentSteps) {
      if (!step.message_id) continue;
      const list = map.get(step.message_id) || [];
      list.push(step);
      map.set(step.message_id, list);
    }
    for (const list of map.values()) {
      list.sort((a, b) => a.step_index - b.step_index);
    }
    return map;
  }, [agentSteps]);

  const scrollToBottom = (behavior: ScrollBehavior) => {
    bottomRef.current?.scrollIntoView({ behavior });
  };

  const updateNearBottom = () => {
    const container = containerRef.current;
    if (!container) return;
    const distance = container.scrollHeight - container.scrollTop - container.clientHeight;
    isNearBottomRef.current = distance < NEAR_BOTTOM_THRESHOLD;
  };

  useEffect(() => {
    const sessionChanged = prevSessionIdRef.current !== activeSessionId;
    const lastMessage = messages[messages.length - 1];
    const lastId = lastMessage?.id || null;

    if (sessionChanged) {
      prevSessionIdRef.current = activeSessionId;
      justSwitchedRef.current = true;
      lastMessageIdRef.current = lastId;
      // On session switch, jump to bottom instantly (no top-to-bottom animation).
      if (messages.length > 0) {
        requestAnimationFrame(() => scrollToBottom('auto'));
      }
      return;
    }

    const hasNewMessage = lastId !== lastMessageIdRef.current;
    if (hasNewMessage) {
      lastMessageIdRef.current = lastId;
      const behavior = justSwitchedRef.current ? 'auto' : 'smooth';
      justSwitchedRef.current = false;
      if (isNearBottomRef.current) {
        requestAnimationFrame(() => scrollToBottom(behavior));
      }
      return;
    }

    justSwitchedRef.current = false;

    // Streaming updates only scroll if user is already near bottom.
    if (isNearBottomRef.current && (streamContent || streamingSteps.length > 0 || currentStreamingStep)) {
      requestAnimationFrame(() => scrollToBottom('smooth'));
    }
  }, [messages, streamContent, streamingSteps, currentStreamingStep, activeSessionId]);

  const assistantAvatar = (
    <div className="w-8 h-8 rounded-full shrink-0 flex items-center justify-center text-[14px] font-semibold bg-gradient-to-br from-codex-accent to-emerald-700 text-black">
      S
    </div>
  );

  return (
    <div
      ref={containerRef}
      onScroll={updateNearBottom}
      className="h-full overflow-y-auto px-6 py-8"
    >
      <div className="max-w-[800px] mx-auto space-y-7">
        {messages.map((msg) => (
          <MessageBubble
            key={msg.id}
            message={msg}
            agentSteps={msg.role === 'assistant' ? stepsByMessageId.get(msg.id) : undefined}
          />
        ))}

        {isStreaming && (
          <div className="flex gap-3 flex-row">
            {assistantAvatar}
            <div className="flex-1 min-w-0 pt-1">
              <div className="flex items-center gap-2 mb-1">
                <span className="text-[13px] font-semibold text-codex-primary">{agentName}</span>
              </div>

              {streamingToPhases(streamingSteps, currentStreamingStep).length > 0 && (
                <ReasoningProcessCard
                  phases={streamingToPhases(streamingSteps, currentStreamingStep)}
                  streaming
                />
              )}

              {streamContent && (
                <div className="mt-2 inline-block text-left max-w-[85%] rounded-2xl px-4 py-2.5 bg-codex-surface/60 border border-codex-border/60 text-[14px] leading-relaxed text-codex-primary">
                  <StreamingContent content={streamContent} />
                </div>
              )}

              {!streamContent && streamingSteps.length === 0 && !currentStreamingStep && (
                <div className="inline-flex items-center gap-2.5 rounded-xl px-3.5 py-2 bg-codex-surface border border-codex-border text-[13px] text-codex-secondary">
                  <Brain size={14} className="text-codex-accent animate-pulse" />
                  <span>正在思考…</span>
                  <span className="flex gap-1 ml-1">
                    <span className="w-1.5 h-1.5 rounded-full bg-codex-accent animate-bounce" />
                    <span className="w-1.5 h-1.5 rounded-full bg-codex-accent animate-bounce" style={{ animationDelay: '0.15s' }} />
                    <span className="w-1.5 h-1.5 rounded-full bg-codex-accent animate-bounce" style={{ animationDelay: '0.3s' }} />
                  </span>
                </div>
              )}
            </div>
          </div>
        )}

        <div ref={bottomRef} />
      </div>
    </div>
  );
}
