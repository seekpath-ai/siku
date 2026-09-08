import { create } from 'zustand';
import type { AgentSession, AgentStep, ChatMessage, StreamingStep, ToolCallInfo, AskQuestion } from '@/lib/types';

interface ChatState {
  sessions: AgentSession[];
  activeSessionId: string | null;
  messages: ChatMessage[];
  agentSteps: AgentStep[];
  isLoading: boolean;
  isStreaming: boolean;
  /** Streaming flag per session — the single source of truth; `isStreaming`
   * mirrors the active session's entry and is restored on session switch. */
  streamingById: Record<string, boolean>;
  /** Loading (awaiting first token) flag per session, same mirroring rule. */
  loadingById: Record<string, boolean>;
  streamContent: string;
  streamingSteps: StreamingStep[];
  currentStreamingStep: StreamingStep | null;
  /** Questions the agent is waiting on the user to answer (AskUserQuestion). */
  pendingQuestions: AskQuestion[] | null;
  /** Session the pending questions belong to; the dialog only renders while
   * that session is active and re-appears when switching back to it. */
  pendingQuestionsSessionId: string | null;

  setSessions: (sessions: AgentSession[]) => void;
  setActiveSession: (id: string | null) => void;
  setMessages: (messages: ChatMessage[]) => void;
  addMessage: (msg: ChatMessage) => void;
  setAgentSteps: (steps: AgentStep[]) => void;
  addAgentStep: (step: AgentStep) => void;
  updateAgentStep: (stepIndex: number, updates: Partial<AgentStep>) => void;
  linkAgentSteps: (messageId: string, sessionId: string) => void;
  setLoading: (v: boolean) => void;
  setStreaming: (v: boolean) => void;
  setSessionLoading: (sessionId: string, v: boolean) => void;
  setSessionStreaming: (sessionId: string, v: boolean) => void;
  appendStreamContent: (text: string) => void;
  clearStreamContent: () => void;
  ensureStreamingStep: (stepIndex: number) => void;
  appendStreamingReasoning: (stepIndex: number, text: string) => void;
  addStreamingToolCall: (stepIndex: number, tc: ToolCallInfo) => void;
  updateStreamingToolCall: (stepIndex: number, id: string, updates: Partial<ToolCallInfo>) => void;
  updateStreamingToolCallById: (id: string, updates: Partial<ToolCallInfo>) => void;
  finalizeStreamingStep: (stepIndex: number) => void;
  clearStreamingSteps: () => void;
  removeSession: (id: string) => void;
  setPendingQuestions: (q: AskQuestion[] | null, sessionId?: string | null) => void;
}

export const useChatStore = create<ChatState>((set) => ({
  sessions: [],
  activeSessionId: null,
  messages: [],
  agentSteps: [],
  isLoading: false,
  isStreaming: false,
  streamingById: {},
  loadingById: {},
  streamContent: '',
  streamingSteps: [],
  currentStreamingStep: null,
  pendingQuestions: null,
  pendingQuestionsSessionId: null,

  setSessions: (sessions) => set({ sessions }),
  setActiveSession: (id) =>
    set((s) => {
      // Clicking the already-active session must not clear its messages.
      if (s.activeSessionId === id) return s;
      return {
        activeSessionId: id,
        messages: [],
        agentSteps: [],
        streamContent: '',
        streamingSteps: [],
        currentStreamingStep: null,
        // Restore the target session's streaming/loading flags so the input
        // is only disabled while THAT session is actually generating.
        isStreaming: id ? !!s.streamingById[id] : false,
        isLoading: id ? !!s.loadingById[id] : false,
        // pendingQuestions intentionally kept: they belong to their origin
        // session (pendingQuestionsSessionId) and re-appear on switch-back.
      };
    }),
  setMessages: (messages) => set({ messages }),
  addMessage: (msg) => set((s) => ({ messages: [...s.messages, msg] })),
  setAgentSteps: (agentSteps) => set({ agentSteps }),
  addAgentStep: (step) => set((s) => ({ agentSteps: [...s.agentSteps, step] })),
  updateAgentStep: (stepIndex, updates) =>
    set((s) => ({
      agentSteps: s.agentSteps.map((st) => (st.step_index === stepIndex ? { ...st, ...updates } : st)),
    })),
  linkAgentSteps: (messageId, sessionId) =>
    set((s) => ({
      agentSteps: s.agentSteps.map((st) =>
        st.message_id === null && st.session_id === sessionId ? { ...st, message_id: messageId } : st
      ),
    })),
  setLoading: (v) =>
    set((s) =>
      s.activeSessionId
        ? { loadingById: { ...s.loadingById, [s.activeSessionId]: v }, isLoading: v }
        : { isLoading: v }
    ),
  setStreaming: (v) =>
    set((s) =>
      s.activeSessionId
        ? { streamingById: { ...s.streamingById, [s.activeSessionId]: v }, isStreaming: v }
        : { isStreaming: v }
    ),
  setSessionLoading: (sessionId, v) =>
    set((s) => ({
      loadingById: { ...s.loadingById, [sessionId]: v },
      ...(sessionId === s.activeSessionId ? { isLoading: v } : {}),
    })),
  setSessionStreaming: (sessionId, v) =>
    set((s) => ({
      streamingById: { ...s.streamingById, [sessionId]: v },
      ...(sessionId === s.activeSessionId ? { isStreaming: v } : {}),
    })),
  appendStreamContent: (text) => set((s) => ({ streamContent: s.streamContent + text })),
  clearStreamContent: () => set({ streamContent: '' }),

  ensureStreamingStep: (stepIndex) =>
    set((s) => {
      if (s.currentStreamingStep && s.currentStreamingStep.step_index === stepIndex) return s;
      const nextStreamingSteps = s.currentStreamingStep
        ? [...s.streamingSteps, { ...s.currentStreamingStep, status: 'completed' as const }]
        : s.streamingSteps;
      return {
        streamingSteps: nextStreamingSteps,
        currentStreamingStep: { step_index: stepIndex, reasoning_content: '', tool_calls: [], status: 'streaming' },
      };
    }),

  appendStreamingReasoning: (stepIndex, text) =>
    set((s) => {
      if (!s.currentStreamingStep || s.currentStreamingStep.step_index !== stepIndex) return s;
      return {
        currentStreamingStep: {
          ...s.currentStreamingStep,
          reasoning_content: s.currentStreamingStep.reasoning_content + text,
        },
      };
    }),

  addStreamingToolCall: (stepIndex, tc) =>
    set((s) => {
      if (!s.currentStreamingStep || s.currentStreamingStep.step_index !== stepIndex) return s;
      if (s.currentStreamingStep.tool_calls.find((t) => t.id === tc.id)) return s;
      return {
        currentStreamingStep: {
          ...s.currentStreamingStep,
          tool_calls: [...s.currentStreamingStep.tool_calls, tc],
        },
      };
    }),

  updateStreamingToolCall: (stepIndex, id, updates) =>
    set((s) => {
      if (!s.currentStreamingStep || s.currentStreamingStep.step_index !== stepIndex) return s;
      return {
        currentStreamingStep: {
          ...s.currentStreamingStep,
          tool_calls: s.currentStreamingStep.tool_calls.map((tc) =>
            tc.id === id ? { ...tc, ...updates } : tc
          ),
        },
      };
    }),

  updateStreamingToolCallById: (id, updates) =>
    set((s) => {
      if (!s.currentStreamingStep) return s;
      return {
        currentStreamingStep: {
          ...s.currentStreamingStep,
          tool_calls: s.currentStreamingStep.tool_calls.map((tc) =>
            tc.id === id ? { ...tc, ...updates } : tc
          ),
        },
      };
    }),

  finalizeStreamingStep: (stepIndex) =>
    set((s) => {
      if (!s.currentStreamingStep || s.currentStreamingStep.step_index !== stepIndex) return s;
      return {
        streamingSteps: [...s.streamingSteps, { ...s.currentStreamingStep, status: 'completed' as const }],
        currentStreamingStep: null,
      };
    }),

  clearStreamingSteps: () => set({ streamingSteps: [], currentStreamingStep: null }),

  removeSession: (id) =>
    set((s) => {
      const streamingById = { ...s.streamingById };
      const loadingById = { ...s.loadingById };
      delete streamingById[id];
      delete loadingById[id];
      const wasActive = s.activeSessionId === id;
      return {
        sessions: s.sessions.filter((ses) => ses.id !== id),
        activeSessionId: wasActive ? null : s.activeSessionId,
        messages: wasActive ? [] : s.messages,
        agentSteps: wasActive ? [] : s.agentSteps,
        streamingById,
        loadingById,
        isStreaming: wasActive ? false : s.isStreaming,
        isLoading: wasActive ? false : s.isLoading,
        pendingQuestions: s.pendingQuestionsSessionId === id ? null : s.pendingQuestions,
        pendingQuestionsSessionId: s.pendingQuestionsSessionId === id ? null : s.pendingQuestionsSessionId,
      };
    }),

  setPendingQuestions: (q, sessionId) =>
    set((s) =>
      q
        ? { pendingQuestions: q, pendingQuestionsSessionId: sessionId ?? s.activeSessionId }
        : { pendingQuestions: null, pendingQuestionsSessionId: null }
    ),
}));
