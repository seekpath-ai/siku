import { useEffect, useRef } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { AgentStreamEvent } from '@/lib/types';

export interface AgentEventStreamHandlers {
  /** Visible-text fragment for `sessionId` (never empty). With
   *  `flushIntervalMs > 0` this fires once per flush window with the
   *  concatenated fragments instead of once per event. */
  onDelta?: (content: string, sessionId: string) => void;
  /** Reasoning fragment for one step (never empty), batched like onDelta. */
  onReasoning?: (stepIndex: number, content: string, sessionId: string) => void;
  /** Any other event addressed to `sessionId`. Pending delta/reasoning
   *  fragments are flushed first, so the handler observes the backend's
   *  event ordering. */
  onEvent?: (e: AgentStreamEvent) => void;
  /** Events addressed to any other session (delivered as-is, unbatched). */
  onForeignEvent?: (e: AgentStreamEvent) => void;
}

export interface AgentEventStreamOptions extends AgentEventStreamHandlers {
  /** The session this listener is attached to. Null treats every event as
   *  foreign. */
  sessionId: string | null;
  /** Merge window for delta/reasoning fragments (ms). 0 delivers each
   *  fragment as its own callback. Default 50. */
  flushIntervalMs?: number;
}

/** Shared `agent:event` listener: subscription lifecycle, per-session
 *  filtering and delta/reasoning batching. The per-store event semantics
 *  (chat panel vs pet panel) are injected via the handler callbacks. */
export function useAgentEventStream({
  sessionId,
  flushIntervalMs = 50,
  ...handlers
}: AgentEventStreamOptions) {
  // Handlers are read through a ref so a changing callback identity never
  // tears down the listener (dropping buffered fragments with it).
  const handlersRef = useRef(handlers);
  useEffect(() => {
    handlersRef.current = handlers;
  });

  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;
    let deltaBuf = '';
    const reasoningBuf = new Map<number, string>();
    let flushTimer: ReturnType<typeof setTimeout> | null = null;

    const stopTimer = () => {
      if (flushTimer) {
        clearTimeout(flushTimer);
        flushTimer = null;
      }
    };

    const flush = () => {
      stopTimer();
      const h = handlersRef.current;
      if (deltaBuf) {
        const text = deltaBuf;
        deltaBuf = '';
        h.onDelta?.(text, sessionId ?? '');
      }
      if (reasoningBuf.size > 0) {
        for (const [stepIndex, text] of reasoningBuf) {
          h.onReasoning?.(stepIndex, text, sessionId ?? '');
        }
        reasoningBuf.clear();
      }
    };

    const scheduleFlush = () => {
      if (flushTimer) return;
      flushTimer = setTimeout(flush, flushIntervalMs);
    };

    const handleEvent = (e: AgentStreamEvent) => {
      if (cancelled) return;
      const h = handlersRef.current;
      if (!sessionId || e.session_id !== sessionId) {
        h.onForeignEvent?.(e);
        return;
      }
      // Empty fragments carry no text and never occur from the engine (its
      // StreamBatcher only emits non-empty merges); drop them like the old
      // per-consumer guards did.
      if (e.type === 'delta') {
        if (e.content) {
          if (flushIntervalMs > 0) {
            deltaBuf += e.content;
            scheduleFlush();
          } else {
            h.onDelta?.(e.content, sessionId);
          }
        }
        return;
      }
      if (e.type === 'reasoning') {
        if (e.content && e.step_index !== undefined) {
          if (flushIntervalMs > 0) {
            reasoningBuf.set(e.step_index, (reasoningBuf.get(e.step_index) ?? '') + e.content);
            scheduleFlush();
          } else {
            h.onReasoning?.(e.step_index, e.content, sessionId);
          }
        }
        return;
      }
      // Preserve event ordering: any buffered text precedes this event.
      flush();
      h.onEvent?.(e);
    };

    void listen<AgentStreamEvent>('agent:event', (event) => handleEvent(event.payload)).then((fn) => {
      if (cancelled) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      cancelled = true;
      // Buffered text belongs to the session that was active while
      // listening; its stream area has already been reset by the session
      // switch, so flush nothing — the post-done DB reload restores the
      // full content.
      stopTimer();
      unlisten?.();
    };
  }, [sessionId, flushIntervalMs]);
}
