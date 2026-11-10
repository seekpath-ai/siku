import { create } from 'zustand';
import type { AgentSession, AgentStep, ChatMessage, StreamingStep, ToolCallInfo, AskQuestion } from '@/lib/types';

/** Messages per history page (initial load and each scroll-up fetch). */
export const MESSAGE_PAGE_SIZE = 50;

interface ChatState {
  sessions: AgentSession[];
  activeSessionId: string | null;
  messages: ChatMessage[];
  /** Older history exists beyond messages[0] (scroll-up pagination). */
  hasMoreMessages: boolean;
  agentSteps: AgentStep[];
  isLoading: boolean;
  isStreaming: boolean;
  /** Streaming flag per session — the single source of truth; `isStreaming`
   * mirrors the active session's entry and is restored on session switch. */
  streamingById: Record<string, boolean>;
  /** Loading (awaiting first token) flag per session, same mirroring rule. */
  loadingById: Record<string, boolean>;
  streamContent: string;
  /** streamContent 所属的轮次（step_index）：工具调用事件到达时，该轮
   * 文本作为过程叙述（narration）迁出回复气泡、归入推理卡。 */
  streamTextStep: number | null;
  streamingSteps: StreamingStep[];
  currentStreamingStep: StreamingStep | null;
  /** Questions the agent is waiting on the user to answer (AskUserQuestion). */
  pendingQuestions: AskQuestion[] | null;
  /** Session the pending questions belong to; the dialog only renders while
   * that session is active and re-appears when switching back to it. */
  pendingQuestionsSessionId: string | null;
  /** First message queued by the empty-state hero input: the session was just
   * created and the message fires once its (empty) history has loaded —
   * sending earlier would race the load and wipe the optimistic bubble. */
  pendingFirst: { sessionId: string; text: string } | null;
  /** Search-result jump: scroll this message into view and flash it. The
   * nonce lets repeat jumps to the same message retrigger. */
  jumpTarget: { messageId: string; nonce: number } | null;
  /** 会话级持久的静默替换映射（DB 消息 id → 沿用的临时 id）。增量对齐的
   * 游标会越过旧轮次，每轮现算的映射会丢旧映射、推理步骤因此挂回真实
   * id 而找不到气泡 —— 所以跨 reload 累积。会话切换时随消息一起清空。 */
  twinTempByDbId: Record<string, string>;

  setSessions: (sessions: AgentSession[]) => void;
  setActiveSession: (id: string | null) => void;
  setMessages: (messages: ChatMessage[]) => void;
  /** Prepend an older page (scroll-up pagination). */
  prependMessages: (messages: ChatMessage[]) => void;
  setHasMoreMessages: (v: boolean) => void;
  addMessage: (msg: ChatMessage) => void;
  /** Replace one message in place (e.g. after tagging it). */
  patchMessage: (msg: ChatMessage) => void;
  setAgentSteps: (steps: AgentStep[]) => void;
  addAgentStep: (step: AgentStep) => void;
  updateAgentStep: (stepIndex: number, updates: Partial<AgentStep>) => void;
  linkAgentSteps: (messageId: string, sessionId: string) => void;
  setLoading: (v: boolean) => void;
  setStreaming: (v: boolean) => void;
  setSessionLoading: (sessionId: string, v: boolean) => void;
  setSessionStreaming: (sessionId: string, v: boolean) => void;
  appendStreamContent: (stepIndex: number, text: string) => void;
  clearStreamContent: () => void;
  /** 工具调用到达：把该轮已流出的文本从回复气泡迁移为步骤的 narration。 */
  migrateStreamToNarration: (stepIndex: number) => void;
  ensureStreamingStep: (stepIndex: number) => void;
  appendStreamingReasoning: (stepIndex: number, text: string) => void;
  addStreamingToolCall: (stepIndex: number, tc: ToolCallInfo) => void;
  updateStreamingToolCall: (stepIndex: number, id: string, updates: Partial<ToolCallInfo>) => void;
  updateStreamingToolCallById: (id: string, updates: Partial<ToolCallInfo>) => void;
  finalizeStreamingStep: (stepIndex: number) => void;
  clearStreamingSteps: () => void;
  removeSession: (id: string) => void;
  /** 累积静默替换映射（见 twinTempByDbId 字段说明）。 */
  mergeTwinTempIds: (pairs: Record<string, string>) => void;
  setPendingQuestions: (q: AskQuestion[] | null, sessionId?: string | null) => void;
  setPendingFirst: (p: { sessionId: string; text: string } | null) => void;
  setJumpTarget: (messageId: string | null) => void;
}

export const useChatStore = create<ChatState>((set) => ({
  sessions: [],
  activeSessionId: null,
  messages: [],
  hasMoreMessages: false,
  agentSteps: [],
  isLoading: false,
  isStreaming: false,
  streamingById: {},
  loadingById: {},
  streamContent: '',
  streamTextStep: null,
  streamingSteps: [],
  currentStreamingStep: null,
  pendingQuestions: null,
  pendingQuestionsSessionId: null,
  pendingFirst: null,
  jumpTarget: null,
  twinTempByDbId: {},

  setSessions: (sessions) => set({ sessions }),
  setActiveSession: (id) =>
    set((s) => {
      // Clicking the already-active session must not clear its messages.
      if (s.activeSessionId === id) return s;
      return {
        activeSessionId: id,
        messages: [],
        hasMoreMessages: false,
        agentSteps: [],
        streamContent: '',
        streamTextStep: null,
        streamingSteps: [],
        currentStreamingStep: null,
        twinTempByDbId: {},
        // Restore the target session's streaming/loading flags so the input
        // is only disabled while THAT session is actually generating.
        isStreaming: id ? !!s.streamingById[id] : false,
        isLoading: id ? !!s.loadingById[id] : false,
        // pendingQuestions intentionally kept: they belong to their origin
        // session (pendingQuestionsSessionId) and re-appear on switch-back.
      };
    }),
  setMessages: (messages) => set({ messages }),
  prependMessages: (older) => set((s) => ({ messages: [...older, ...s.messages] })),
  setHasMoreMessages: (v) => set({ hasMoreMessages: v }),
  addMessage: (msg) => set((s) => ({ messages: [...s.messages, msg] })),
  patchMessage: (msg) =>
    set((s) => ({ messages: s.messages.map((m) => (m.id === msg.id ? msg : m)) })),
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
  appendStreamContent: (stepIndex, text) =>
    set((s) => {
      // 换轮时若旧轮文本残留（理论上有 tool_call 先迁移，此处是兜底），
      // 先归入旧轮的 narration，避免两轮文本混接。
      if (s.streamTextStep !== null && s.streamTextStep !== stepIndex && s.streamContent) {
        const withNarration = (st: StreamingStep): StreamingStep =>
          st.step_index === s.streamTextStep ? { ...st, narration: st.narration ?? s.streamContent } : st;
        return {
          streamContent: text,
          streamTextStep: stepIndex,
          streamingSteps: s.streamingSteps.map(withNarration),
          currentStreamingStep: s.currentStreamingStep ? withNarration(s.currentStreamingStep) : null,
        };
      }
      return { streamContent: s.streamContent + text, streamTextStep: stepIndex };
    }),
  clearStreamContent: () => set({ streamContent: '', streamTextStep: null }),
  migrateStreamToNarration: (stepIndex) =>
    set((s) => {
      if (!s.streamContent || s.streamTextStep !== stepIndex) return { streamContent: '', streamTextStep: null };
      const withNarration = (st: StreamingStep): StreamingStep =>
        st.step_index === stepIndex ? { ...st, narration: st.narration ?? s.streamContent } : st;
      return {
        streamContent: '',
        streamTextStep: null,
        streamingSteps: s.streamingSteps.map(withNarration),
        currentStreamingStep: s.currentStreamingStep ? withNarration(s.currentStreamingStep) : null,
      };
    }),

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
        twinTempByDbId: wasActive ? {} : s.twinTempByDbId,
      };
    }),

  setPendingQuestions: (q, sessionId) =>
    set((s) =>
      q
        ? { pendingQuestions: q, pendingQuestionsSessionId: sessionId ?? s.activeSessionId }
        : { pendingQuestions: null, pendingQuestionsSessionId: null }
    ),

  setPendingFirst: (p) => set({ pendingFirst: p }),
  setJumpTarget: (messageId) =>
    set((s) => ({
      jumpTarget: messageId ? { messageId, nonce: (s.jumpTarget?.nonce ?? 0) + 1 } : null,
    })),
  mergeTwinTempIds: (pairs) =>
    set((s) => ({ twinTempByDbId: { ...s.twinTempByDbId, ...pairs } })),
}));
