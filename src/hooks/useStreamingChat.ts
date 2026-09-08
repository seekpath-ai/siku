import { useCallback } from 'react';
import { useChatStore } from '@/stores/chatStore';
import { getChatMessages, getAgentSteps } from '@/lib/tauri';
import { useAgentEventStream } from '@/hooks/useAgentEventStream';
import type { AgentStep, AgentStreamEvent, ChatMessage, ToolCallInfo } from '@/lib/types';

/** Event types that mark their session as actively generating. */
const STREAMING_START_TYPES = new Set<AgentStreamEvent['type']>([
  'thinking',
  'delta',
  'reasoning',
  'tool_call',
  'tool_approval_required',
]);

export function useStreamingChat() {
  const activeSessionId = useChatStore((s) => s.activeSessionId);

  const finalizeStep = useCallback((stepIndex: number) => {
    const state = useChatStore.getState();
    const step = state.currentStreamingStep;
    if (!step || step.step_index !== stepIndex) return;

    const toolCalls = step.tool_calls;
    const agentStep: AgentStep = {
      id: `step_${step.step_index}_${Date.now()}`,
      session_id: state.activeSessionId || '',
      message_id: null,
      step_index: step.step_index,
      reasoning_content: step.reasoning_content.trim() || null,
      tool_calls: toolCalls.length > 0 ? JSON.stringify(toolCalls) : null,
      created_at: new Date().toISOString(),
    };
    state.addAgentStep(agentStep);
    state.finalizeStreamingStep(stepIndex);
  }, []);

  // Re-read the persisted history after a turn completes so the local view
  // matches the DB (same post-run alignment the pet panel already does).
  const reloadSessionHistory = useCallback((sessionId: string) => {
    const reload = () => Promise.all([getChatMessages(sessionId), getAgentSteps(sessionId)]);
    const apply = ([msgs, steps]: [ChatMessage[], AgentStep[]]) => {
      const state = useChatStore.getState();
      // The user switched away meanwhile; that session's history is loaded
      // by ChatPanel when it becomes active again.
      if (state.activeSessionId !== sessionId) return;
      const lastAssistant = [...msgs].reverse().find((m) => m.role === 'assistant');
      const linked = lastAssistant
        ? steps.map((s) => (s.message_id === null ? { ...s, message_id: lastAssistant.id } : s))
        : steps;
      state.setMessages(msgs);
      state.setAgentSteps(linked);
    };
    (async () => {
      try {
        apply(await reload());
      } catch (err) {
        console.error('reload messages after done:', err);
        try {
          await new Promise((r) => setTimeout(r, 800));
          apply(await reload());
        } catch (err2) {
          // Keep the locally synthesized message as a fallback.
          console.error('reload messages retry failed:', err2);
        }
      }
    })();
  }, []);

  const onDelta = useCallback((content: string, sessionId: string) => {
    const state = useChatStore.getState();
    state.setSessionStreaming(sessionId, true);
    state.appendStreamContent(content);
  }, []);

  const onReasoning = useCallback((stepIndex: number, content: string, sessionId: string) => {
    const state = useChatStore.getState();
    state.setSessionStreaming(sessionId, true);
    state.ensureStreamingStep(stepIndex);
    state.appendStreamingReasoning(stepIndex, content);
  }, []);

  // Events for sessions other than the active one (pet/cron runs, or a chat
  // session the user switched away from). Content is dropped; only the
  // per-session streaming/loading flags are tracked so switching back
  // restores the correct input state.
  const onForeignEvent = useCallback((e: AgentStreamEvent) => {
    const state = useChatStore.getState();
    if (e.type === 'done' || e.type === 'cancelled' || e.type === 'error') {
      // Terminal events must clear the OWNING session's streaming/loading
      // flags even when it is not the active one — otherwise the input
      // locks up permanently once that session is switched back to.
      state.setSessionStreaming(e.session_id, false);
      state.setSessionLoading(e.session_id, false);
    } else if (STREAMING_START_TYPES.has(e.type)) {
      state.setSessionStreaming(e.session_id, true);
    }
  }, []);

  // Non-stream events for the active session. The hook has already flushed
  // any buffered delta/reasoning text, so the state read below observes the
  // just-flushed streamContent / currentStreamingStep.
  const onEvent = useCallback((e: AgentStreamEvent) => {
    const state = useChatStore.getState();

    switch (e.type) {
      case 'thinking':
        state.setSessionStreaming(e.session_id, true);
        break;

      case 'tool_approval_required':
        state.setSessionStreaming(e.session_id, true);
        if (e.tool_call_id && e.tool_name && e.step_index !== undefined) {
          state.ensureStreamingStep(e.step_index);
          // Update an existing tool call to pending; if it does not exist yet, add it.
          state.updateStreamingToolCall(e.step_index, e.tool_call_id, {
            status: 'pending',
          });
          state.addStreamingToolCall(e.step_index, {
            id: e.tool_call_id,
            name: e.tool_name,
            arguments: (e.tool_args as Record<string, unknown>) || {},
            status: 'pending',
          });
        }
        break;

      case 'tool_call':
        state.setSessionStreaming(e.session_id, true);
        if (e.tool_call_id && e.tool_name && e.step_index !== undefined) {
          state.ensureStreamingStep(e.step_index);
          state.addStreamingToolCall(e.step_index, {
            id: e.tool_call_id,
            name: e.tool_name,
            arguments: (e.tool_args as Record<string, unknown>) || {},
            status: 'running',
          });
        }
        break;

      case 'tool_result':
        if (e.tool_call_id && e.step_index !== undefined) {
          state.ensureStreamingStep(e.step_index);
          state.updateStreamingToolCall(e.step_index, e.tool_call_id, {
            result: e.tool_result,
            status: (e.status as ToolCallInfo['status']) || 'completed',
            duration_ms: e.duration_ms,
          });
        }
        break;

      case 'step_complete':
        if (e.step_index !== undefined) {
          finalizeStep(e.step_index);
        }
        break;

      case 'done': {
        state.setSessionStreaming(e.session_id, false);
        const currentStep = state.currentStreamingStep;
        if (currentStep) {
          finalizeStep(currentStep.step_index);
        }
        const finalContent = e.content || state.streamContent;
        const messageId = `assistant_${Date.now()}`;
        if (finalContent) {
          state.addMessage({
            id: messageId,
            session_id: e.session_id,
            role: 'assistant',
            content: finalContent,
            reasoning_content: null,
            tool_calls: null,
            citations: null,
            model: null,
            tokens_used: e.tokens_used ?? null,
            tokens_in: e.tokens_in ?? null,
            tokens_in_hit: e.tokens_in_hit ?? null,
            tokens_out: e.tokens_out ?? null,
            attachments: null,
            created_at: new Date().toISOString(),
          });
          state.linkAgentSteps(messageId, e.session_id);
        }
        state.clearStreamContent();
        state.clearStreamingSteps();
        state.setSessionLoading(e.session_id, false);
        // Align with the DB so the reply gets its persisted id and any
        // lost stream events are healed from history.
        reloadSessionHistory(e.session_id);
        break;
      }

      case 'ask_user': {
        // The agent is waiting for structured answers (AskUserQuestion).
        try {
          const raw = e.content ? JSON.parse(e.content) : null;
          const parsed = Array.isArray(raw) ? raw : raw?.questions;
          if (Array.isArray(parsed) && parsed.length > 0) {
            state.setPendingQuestions(parsed, e.session_id);
          }
        } catch {
          /* ignore malformed questions */
        }
        break;
      }

      case 'cancelled': {
        // Generation stopped by the user; keep whatever was produced.
        state.setSessionStreaming(e.session_id, false);
        state.setSessionLoading(e.session_id, false);
        const currentStep = state.currentStreamingStep;
        if (currentStep) {
          finalizeStep(currentStep.step_index);
        }
        const finalContent = e.content || state.streamContent;
        const messageId = `assistant_${Date.now()}`;
        if (finalContent) {
          state.addMessage({
            id: messageId,
            session_id: e.session_id,
            role: 'assistant',
            content: `${finalContent}\n\n> ⏹ 已停止生成`,
            reasoning_content: null,
            tool_calls: null,
            citations: null,
            model: null,
            tokens_used: e.tokens_used ?? null,
            tokens_in: e.tokens_in ?? null,
            tokens_in_hit: e.tokens_in_hit ?? null,
            tokens_out: e.tokens_out ?? null,
            attachments: null,
            created_at: new Date().toISOString(),
          });
          state.linkAgentSteps(messageId, e.session_id);
        }
        state.clearStreamContent();
        state.clearStreamingSteps();
        break;
      }

      case 'error':
        state.setSessionStreaming(e.session_id, false);
        state.setSessionLoading(e.session_id, false);
        {
          const currentStep = state.currentStreamingStep;
          if (currentStep) {
            finalizeStep(currentStep.step_index);
          }
        }
        state.addMessage({
          id: `error_${Date.now()}`,
          session_id: e.session_id,
          role: 'assistant',
          content: `❌ ${e.content || 'Unknown error'}`,
          reasoning_content: null,
          tool_calls: null,
          citations: null,
          model: null,
          tokens_used: null,
          tokens_in: null,
          tokens_in_hit: null,
          tokens_out: null,
          attachments: null,
          created_at: new Date().toISOString(),
        });
        state.clearStreamContent();
        state.clearStreamingSteps();
        break;
    }
  }, [finalizeStep, reloadSessionHistory]);

  useAgentEventStream({
    sessionId: activeSessionId,
    onDelta,
    onReasoning,
    onEvent,
    onForeignEvent,
  });
}
