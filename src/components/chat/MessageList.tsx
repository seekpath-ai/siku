import { memo, useEffect, useMemo, useRef, useState } from 'react';
import { useChatStore } from '@/stores/chatStore';
import type { AgentStep, ChatMessage } from '@/lib/types';
import { streamingToPhases } from '@/lib/agentPhases';
import { MessageBubble } from './MessageBubble';
import { StreamingContent } from './StreamingContent';
import { ReasoningProcessCard } from './ReasoningProcessCard';
import { useActiveAgentName } from '@/hooks/useActiveAgentName';
import { Brain } from 'lucide-react';

const NEAR_BOTTOM_THRESHOLD = 120; // px

const assistantAvatar = (
  <div className="w-8 h-8 rounded-full shrink-0 flex items-center justify-center text-[14px] font-semibold bg-gradient-to-br from-codex-accent to-emerald-700 text-black">
    S
  </div>
);

/** Persisted history. Memoized so streaming updates (which re-render the
 * parent for auto-scroll) never re-render — and re-parse — old bubbles. */
const HistoryMessages = memo(function HistoryMessages({
  messages,
  stepsByMessageId,
  highlightId,
}: {
  messages: ChatMessage[];
  stepsByMessageId: Map<string, AgentStep[]>;
  highlightId?: string | null;
}) {
  // Each assistant message gets the nearest preceding user message — the
  // bubble's turn export includes it for context.
  const prevUserById = useMemo(() => {
    const map = new Map<string, ChatMessage>();
    let lastUser: ChatMessage | undefined;
    for (const m of messages) {
      if (m.role === 'user') lastUser = m;
      else if (m.role === 'assistant' && lastUser) map.set(m.id, lastUser);
    }
    return map;
  }, [messages]);

  return (
    <>
      {messages.map((msg) => (
        <MessageBubble
          key={msg.id}
          message={msg}
          agentSteps={msg.role === 'assistant' ? stepsByMessageId.get(msg.id) : undefined}
          prevUserMessage={msg.role === 'assistant' ? prevUserById.get(msg.id) : undefined}
          highlighted={msg.id === highlightId}
        />
      ))}
    </>
  );
});

/** The live turn: subscribes to the streaming slice on its own, so the
 * history list above is fully isolated from per-flush updates. */
function StreamingBubble() {
  const agentName = useActiveAgentName();
  const isStreaming = useChatStore((s) => s.isStreaming);
  const streamContent = useChatStore((s) => s.streamContent);
  const streamingSteps = useChatStore((s) => s.streamingSteps);
  const currentStreamingStep = useChatStore((s) => s.currentStreamingStep);

  if (!isStreaming) return null;

  return (
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
  );
}

export function MessageList({
  onLoadOlder,
  loadingOlder,
}: {
  /** Scroll-up pagination: fired when the user scrolls near the top and
   * older history exists. */
  onLoadOlder?: () => void;
  loadingOlder?: boolean;
}) {
  const messages = useChatStore((s) => s.messages);
  const hasMoreMessages = useChatStore((s) => s.hasMoreMessages);
  const agentSteps = useChatStore((s) => s.agentSteps);
  const streamContent = useChatStore((s) => s.streamContent);
  const streamingSteps = useChatStore((s) => s.streamingSteps);
  const currentStreamingStep = useChatStore((s) => s.currentStreamingStep);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const jumpTarget = useChatStore((s) => s.jumpTarget);
  const containerRef = useRef<HTMLDivElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const prevSessionIdRef = useRef<string | null>(null);
  const lastMessageIdRef = useRef<string | null>(null);
  const justSwitchedRef = useRef(false);
  const isNearBottomRef = useRef(true);
  /** Search-jump bookkeeping: the nonce already completed, the nonce last
   * seen (resets the page-pull counter), and how many older pages the
   * current jump has pulled while hunting the message. */
  const jumpDoneNonceRef = useRef(0);
  const jumpPrevNonceRef = useRef(0);
  const jumpAttemptsRef = useRef(0);
  const [highlightId, setHighlightId] = useState<string | null>(null);
  /** scrollHeight captured when an older-page fetch starts; the prepend
   * shifts content down, and restoring scrollTop = newHeight - oldHeight
   * keeps the viewport pinned to the same message. */
  const prependAnchorRef = useRef<number | null>(null);

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

  // Scroll-top trigger for older history (capture the anchor BEFORE the
  // store prepend happens — the fetch is async, so the anchor is restored
  // in the layout effect below once the page lands).
  const handleScroll = () => {
    updateNearBottom();
    const container = containerRef.current;
    if (!container || !onLoadOlder || !hasMoreMessages || loadingOlder) return;
    if (container.scrollTop < 80) {
      prependAnchorRef.current = container.scrollHeight;
      onLoadOlder();
    }
  };

  // Restore the scroll anchor after an older page is prepended.
  useEffect(() => {
    const container = containerRef.current;
    if (container && prependAnchorRef.current != null) {
      container.scrollTop = container.scrollHeight - prependAnchorRef.current;
      prependAnchorRef.current = null;
    }
  }, [messages]);

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

  // Search-jump: scroll the target message into view and flash it. When the
  // message predates the loaded window, pull older pages until it appears
  // (bounded — a deleted message must not loop forever).
  useEffect(() => {
    if (!jumpTarget) return;
    if (jumpDoneNonceRef.current === jumpTarget.nonce) return;
    if (jumpPrevNonceRef.current !== jumpTarget.nonce) {
      jumpPrevNonceRef.current = jumpTarget.nonce;
      jumpAttemptsRef.current = 0;
    }
    const el = containerRef.current?.querySelector(`[data-msg-id="${jumpTarget.messageId}"]`);
    if (el) {
      jumpDoneNonceRef.current = jumpTarget.nonce;
      jumpAttemptsRef.current = 0;
      requestAnimationFrame(() => el.scrollIntoView({ block: 'center' }));
      setHighlightId(jumpTarget.messageId);
      // Consume the jump: leaving it in the store would re-trigger the scroll
      // + flash every time this list remounts (e.g. switching back to the
      // chat tab). The 2.4s flash runs off local highlightId, unaffected.
      useChatStore.getState().setJumpTarget(null);
      return;
    }
    if (hasMoreMessages && onLoadOlder && !loadingOlder && jumpAttemptsRef.current < 50) {
      jumpAttemptsRef.current += 1;
      onLoadOlder();
    }
  }, [jumpTarget, messages, hasMoreMessages, loadingOlder, onLoadOlder]);

  // The flash fades on its own.
  useEffect(() => {
    if (!highlightId) return;
    const t = setTimeout(() => setHighlightId(null), 2400);
    return () => clearTimeout(t);
  }, [highlightId]);

  return (
    <div
      ref={containerRef}
      onScroll={handleScroll}
      className="h-full overflow-y-auto px-6 py-8"
    >
      <div className="max-w-[800px] mx-auto space-y-7">
        {hasMoreMessages && (
          <div className="flex justify-center">
            <span className="text-[11px] text-codex-muted">
              {loadingOlder ? '加载更早的消息…' : '向上滚动加载更早的消息'}
            </span>
          </div>
        )}
        <HistoryMessages messages={messages} stepsByMessageId={stepsByMessageId} highlightId={highlightId} />
        <StreamingBubble />
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
