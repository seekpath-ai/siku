import { create } from 'zustand';
import type { AgentSession, AgentStep, ChatAttachment, ChatMessage, StreamingStep, ToolCallInfo } from '@/lib/types';
import { petCreateSession, getChatMessages, getAgentSteps, agentGetSession, agentSendMessage } from '@/lib/tauri';
import type { PetContext } from './petContextStore';

/** Map a page type to its built-in pet domain agent. */
export const PAGE_DOMAIN: Record<string, string> = {
  notes: 'note_organizer',
  library: 'literature_analyzer',
  reader: 'literature_analyzer',
  research: 'research_tracker',
  knowledge: 'knowledge_curator',
  chat: 'chat_summarizer',
};

export interface PetApproval {
  toolCallId: string;
  toolName: string;
  args: string;
  stepIndex?: number;
}

interface PetState {
  open: boolean;
  session: AgentSession | null;
  messages: ChatMessage[];
  /** Persisted agent steps (tool calls / reasoning) for history cards. */
  agentSteps: AgentStep[];
  streamContent: string;
  loading: boolean;
  streaming: boolean;
  pendingApproval: PetApproval | null;
  error: string | null;
  /** Completed streaming steps (for the reasoning/tool-call process card). */
  streamingSteps: StreamingStep[];
  /** The step currently being streamed. */
  currentStreamingStep: StreamingStep | null;
  /** Bumped when a run's persisted messages have been reloaded after
   *  completion — the panel scrolls to the final reply in response. */
  completionNonce: number;
  setOpen: (o: boolean) => void;
  /** Create a fresh domain session for the current context and open the panel. */
  start: (ctx: PetContext) => Promise<void>;
  /** Attach to an EXISTING session (detached chat window) and load history. */
  attach: (sessionId: string) => Promise<void>;
  send: (text: string, attachments?: ChatAttachment[]) => Promise<void>;
  appendDelta: (t: string) => void;
  setStreaming: (s: boolean) => void;
  setPendingApproval: (a: PetApproval | null) => void;
  setSession: (s: AgentSession | null) => void;
  setError: (e: string | null) => void;
  // ── Streaming step machinery (mirrors chatStore) ──
  ensureStreamingStep: (stepIndex: number) => void;
  appendStreamingReasoning: (stepIndex: number, text: string) => void;
  addStreamingToolCall: (stepIndex: number, tc: ToolCallInfo) => void;
  updateStreamingToolCall: (stepIndex: number, id: string, updates: Partial<ToolCallInfo>) => void;
  finalizeStreamingStep: (stepIndex: number) => void;
  clearStreamingSteps: () => void;
  reset: () => void;
}

const initial = {
  open: false,
  session: null,
  messages: [],
  agentSteps: [],
  streamContent: '',
  loading: false,
  streaming: false,
  pendingApproval: null,
  error: null,
  streamingSteps: [],
  currentStreamingStep: null,
  completionNonce: 0,
};

export const usePetStore = create<PetState>((set, get) => ({
  ...initial,

  setOpen: (open) => set({ open }),

  start: async (ctx) => {
    set({
      loading: true, open: true, session: null, messages: [], agentSteps: [], streamContent: '',
      streaming: false, pendingApproval: null, error: null,
      streamingSteps: [], currentStreamingStep: null,
    });
    const domain = PAGE_DOMAIN[ctx.page] || 'note_organizer';
    try {
      const session = await petCreateSession(domain, {
        page: ctx.page,
        objectId: ctx.objectId,
        title: ctx.title,
      });
      const [messages, agentSteps] = await Promise.all([
        getChatMessages(session.id),
        getAgentSteps(session.id),
      ]);
      set({ session, messages, agentSteps, loading: false });
    } catch (err) {
      console.error('pet start:', err);
      set({ loading: false, session: null, error: err instanceof Error ? err.message : String(err) });
    }
  },

  attach: async (sessionId) => {
    set({
      loading: true, open: true, session: null, messages: [], agentSteps: [], streamContent: '',
      streaming: false, pendingApproval: null, error: null,
      streamingSteps: [], currentStreamingStep: null,
    });
    try {
      const session = await agentGetSession(sessionId);
      const [messages, agentSteps] = await Promise.all([
        getChatMessages(sessionId),
        getAgentSteps(sessionId),
      ]);
      set({ session, messages, agentSteps, loading: false });
    } catch (err) {
      console.error('pet attach:', err);
      set({ loading: false, session: null, error: err instanceof Error ? err.message : String(err) });
    }
  },

  send: async (text, attachments) => {
    const { session } = get();
    if (!session || get().streaming) return;
    const userMsg: ChatMessage = {
      id: `pet_u_${Date.now()}`,
      session_id: session.id,
      role: 'user',
      content: text,
      reasoning_content: null,
      tool_calls: null,
      citations: null,
      model: null,
      tokens_used: null,
      tokens_in: null,
      tokens_in_hit: null,
      tokens_out: null,
      attachments: attachments?.length ? JSON.stringify(attachments) : null,
      created_at: new Date().toISOString(),
    };
    set((s) => ({
      messages: [...s.messages, userMsg], streamContent: '', streaming: true,
      pendingApproval: null, streamingSteps: [], currentStreamingStep: null, error: null,
    }));
    try {
      await agentSendMessage(session.id, text, attachments);
    } catch (err) {
      console.error('pet send:', err);
      set({ streaming: false, error: err instanceof Error ? err.message : String(err) });
    }
  },

  appendDelta: (t) => set((s) => ({ streamContent: s.streamContent + t })),
  setStreaming: (streaming) => set({ streaming }),
  setPendingApproval: (pendingApproval) => set({ pendingApproval }),
  setSession: (session) => set({ session }),
  setError: (error) => set({ error }),

  // ── Streaming step machinery ──
  ensureStreamingStep: (stepIndex) =>
    set((s) => {
      if (s.currentStreamingStep && s.currentStreamingStep.step_index === stepIndex) return s;
      const nextStreamingSteps = s.currentStreamingStep
        ? [...s.streamingSteps, { ...s.currentStreamingStep, status: 'completed' as const }]
        : s.streamingSteps;
      return {
        streamingSteps: nextStreamingSteps,
        currentStreamingStep: { step_index: stepIndex, reasoning_content: '', tool_calls: [], status: 'streaming' as const },
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

  finalizeStreamingStep: (stepIndex) =>
    set((s) => {
      if (!s.currentStreamingStep || s.currentStreamingStep.step_index !== stepIndex) return s;
      return {
        streamingSteps: [...s.streamingSteps, { ...s.currentStreamingStep, status: 'completed' as const }],
        currentStreamingStep: null,
      };
    }),

  clearStreamingSteps: () => set({ streamingSteps: [], currentStreamingStep: null }),

  reset: () => set({ ...initial }),
}));
