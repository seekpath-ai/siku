import { create } from 'zustand';
import type { AgentSession, AgentStep, ChatMessage, StreamingStep, ToolCallInfo, AskQuestion } from '@/lib/types';

interface ChatState {
  sessions: AgentSession[];
  activeSessionId: string | null;
  messages: ChatMessage[];
  agentSteps: AgentStep[];
  isLoading: boolean;
  isStreaming: boolean;
  streamContent: string;
  streamingSteps: StreamingStep[];
  currentStreamingStep: StreamingStep | null;
  /** Questions the agent is waiting on the user to answer (AskUserQuestion). */
  pendingQuestions: AskQuestion[] | null;

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
  setPendingQuestions: (q: AskQuestion[] | null) => void;
}

export const useChatStore = create<ChatState>((set) => ({
  sessions: [],
  activeSessionId: null,
  messages: [],
  agentSteps: [],
  isLoading: false,
  isStreaming: false,
  streamContent: '',
  streamingSteps: [],
  currentStreamingStep: null,
  pendingQuestions: null,

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
        pendingQuestions: null,
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
  setLoading: (v) => set({ isLoading: v }),
  setStreaming: (v) => set({ isStreaming: v }),
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
    set((s) => ({
      sessions: s.sessions.filter((ses) => ses.id !== id),
      activeSessionId: s.activeSessionId === id ? null : s.activeSessionId,
      messages: s.activeSessionId === id ? [] : s.messages,
      agentSteps: s.activeSessionId === id ? [] : s.agentSteps,
    })),

  setPendingQuestions: (pendingQuestions) => set({ pendingQuestions }),
}));
