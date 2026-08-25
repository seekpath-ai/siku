import { useCallback, useEffect, useRef } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { useChatStore } from '@/stores/chatStore';
import type { AgentStep, AgentStreamEvent, ToolCallInfo } from '@/lib/types';

export function useStreamingChat() {
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const activeSessionId = useChatStore((s) => s.activeSessionId);

  const finalizeStep = useCallback((stepIndex: number) => {
    const state = useChatStore.getState();
    const step = state.currentStreamingStep;
    if (!step || step.step_index !== stepIndex) return;

    const toolCalls = step.tool_calls;
    const agentStep: AgentStep = {
      id: `step_${step.step_index}_${Date.now()}`,
      session_id: activeSessionId || '',
      message_id: null,
      step_index: step.step_index,
      reasoning_content: step.reasoning_content.trim() || null,
      tool_calls: toolCalls.length > 0 ? JSON.stringify(toolCalls) : null,
      created_at: new Date().toISOString(),
    };
    state.addAgentStep(agentStep);
    state.finalizeStreamingStep(stepIndex);
    // Persist any assistant content produced during this step as its own message
    // so intermediate greetings/explanations are not lost after tool calls.
    /*
    const stepContent = state.streamContent.trim();
    if (stepContent) {
      const messageId = `assistant_step_${step.step_index}_${Date.now()}`;
      state.addMessage({
        id: messageId,
        session_id: activeSessionId || '',
        role: 'assistant',
        content: stepContent,
        reasoning_content: null,
        tool_calls: null,
        citations: null,
        model: null,
        tokens_used: null,
        created_at: new Date().toISOString(),
      });
      state.linkAgentSteps(messageId, activeSessionId || '');
      state.clearStreamContent();
    }
    */
  }, [activeSessionId]);

  useEffect(() => {
    let cancelled = false;

    const setup = async () => {
      const unlisten = await listen<AgentStreamEvent>('agent:event', (event) => {
        if (cancelled) return;
        const e = event.payload;

        // Ignore events for other sessions
        if (activeSessionId && e.session_id !== activeSessionId) return;

        const state = useChatStore.getState();

        switch (e.type) {
          case 'thinking':
            state.setStreaming(true);
            break;

          case 'delta':
            state.setStreaming(true);
            if (e.content) state.appendStreamContent(e.content);
            break;

          case 'reasoning': {
            state.setStreaming(true);
            if (!e.content || e.step_index === undefined) break;
            state.ensureStreamingStep(e.step_index);
            state.appendStreamingReasoning(e.step_index, e.content);
            break;
          }

          case 'tool_approval_required':
            state.setStreaming(true);
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
            state.setStreaming(true);
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
            state.setStreaming(false);
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
            state.setLoading(false);
            break;
          }

          case 'ask_user': {
            // The agent is waiting for structured answers (AskUserQuestion).
            try {
              const raw = e.content ? JSON.parse(e.content) : null;
              const parsed = Array.isArray(raw) ? raw : raw?.questions;
              if (Array.isArray(parsed) && parsed.length > 0) {
                state.setPendingQuestions(parsed);
              }
            } catch {
              /* ignore malformed questions */
            }
            break;
          }

          case 'cancelled': {
            // Generation stopped by the user; keep whatever was produced.
            state.setStreaming(false);
            state.setLoading(false);
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
            state.setStreaming(false);
            state.setLoading(false);
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
      });

      if (!cancelled) {
        unlistenRef.current = unlisten;
      } else {
        unlisten();
      }
    };

    setup();

    return () => {
      cancelled = true;
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
    };
  }, [activeSessionId, finalizeStep]);
}
