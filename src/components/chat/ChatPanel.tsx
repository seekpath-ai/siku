import { useEffect, useState } from 'react';
import { Bot, Loader2, ArrowUp } from 'lucide-react';
import { MessageList } from './MessageList';
import { MessageInput } from './MessageInput';
import { ChatHeader } from './ChatHeader';
import { AskUserDialog } from './AskUserDialog';
import { useChatStore } from '@/stores/chatStore';
import { useProjectStore } from '@/stores/projectStore';
import { usePetContextStore } from '@/stores/petContextStore';
import { useStreamingChat } from '@/hooks/useStreamingChat';
import { getChatMessages, getAgentSteps, agentRenameSession, agentSetSessionModel, agentIsRunning, agentCreateSession, agentSendMessage } from '@/lib/tauri';
import { DEFAULT_TOOLS } from '@/lib/agent-tools';

/** Empty-state hero input: typing here and hitting send creates a session
 * and immediately starts the first turn (Kimi-style "输入即开工"). The first
 * message is handed to the chat store as `pendingFirst` and fired from the
 * message-load effect below, once the (empty) history load has settled —
 * sending earlier would race that load and lose the optimistic bubble. */
function HeroInput() {
  const [text, setText] = useState('');
  const [creating, setCreating] = useState(false);
  const { sessions, setSessions, setActiveSession, setPendingFirst } = useChatStore();
  const { activeProjectId } = useProjectStore();

  const submit = async () => {
    const trimmed = text.trim();
    if (!trimmed || creating) return;
    setCreating(true);
    try {
      const firstLine = trimmed.split('\n')[0];
      const title = firstLine.length > 20 ? `${firstLine.slice(0, 20)}…` : firstLine;
      const session = await agentCreateSession({
        title,
        agentMode: 'chat',
        toolsEnabled: DEFAULT_TOOLS,
        projectId: activeProjectId ?? null,
      });
      setSessions([session, ...sessions]);
      setPendingFirst({ sessionId: session.id, text: trimmed });
      setActiveSession(session.id);
    } catch (err) {
      console.error('Failed to create session from hero input:', err);
      setCreating(false);
    }
  };

  return (
    <div className="w-full max-w-2xl px-6">
      <div className="rounded-2xl border border-codex-border bg-codex-surface shadow-lg focus-within:border-codex-accent transition-colors">
        <textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void submit();
            }
          }}
          autoFocus
          rows={3}
          placeholder="输入消息，开始新的对话…"
          className="w-full bg-transparent resize-none px-4 pt-3.5 pb-1 text-[14px] text-codex-primary placeholder:text-codex-muted focus:outline-none"
        />
        <div className="flex items-center justify-between px-3 pb-2.5">
          <span className="text-[11px] text-codex-muted">Enter 发送 · Shift+Enter 换行</span>
          <button
            onClick={() => void submit()}
            disabled={!text.trim() || creating}
            aria-label="发送"
            className="w-8 h-8 flex items-center justify-center rounded-full bg-codex-accent text-white disabled:opacity-30 hover:opacity-90 transition-opacity"
          >
            {creating ? <Loader2 size={15} className="animate-spin" /> : <ArrowUp size={15} />}
          </button>
        </div>
      </div>
    </div>
  );
}

/** Fire the hero input's queued first message (optimistic bubble + send). */
async function sendFirstMessage(sessionId: string, text: string) {
  const store = useChatStore.getState();
  store.addMessage({
    id: `user_${Date.now()}`,
    session_id: sessionId,
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
    attachments: null,
    user_tag: null,
    created_at: new Date().toISOString(),
  });
  store.setSessionLoading(sessionId, true);
  try {
    await agentSendMessage(sessionId, text);
  } catch (err) {
    const st = useChatStore.getState();
    st.setSessionLoading(sessionId, false);
    st.addMessage({
      id: `error_${Date.now()}`,
      session_id: sessionId,
      role: 'assistant',
      content: `❌ 发送失败: ${err}`,
      reasoning_content: null,
      tool_calls: null,
      citations: null,
      model: null,
      tokens_used: null,
      tokens_in: null,
      tokens_in_hit: null,
      tokens_out: null,
      attachments: null,
      user_tag: null,
      created_at: new Date().toISOString(),
    });
  }
}

export function ChatPanel() {
  useStreamingChat();
  const { activeSessionId, sessions, isStreaming, setMessages, setAgentSteps, setSessions } =
    useChatStore();
  const { projects } = useProjectStore();
  const [loadingMessages, setLoadingMessages] = useState(false);

  const activeSession = sessions.find((s) => s.id === activeSessionId) ?? null;

  // Expose the focused conversation to the global pet.
  useEffect(() => {
    if (activeSession) {
      usePetContextStore.getState().setContext({
        page: 'chat',
        objectId: activeSession.id,
        title: activeSession.title || '当前对话',
      });
    } else {
      usePetContextStore.getState().setContext(null);
    }
    return () => usePetContextStore.getState().setContext(null);
  }, [activeSession]);
  // The project chip reflects the ACTIVE CONVERSATION's project, which may
  // differ from the sidebar's current filter project.
  const sessionProject =
    projects.find((p) => p.id === activeSession?.project_id) ?? null;

  useEffect(() => {
    if (!activeSessionId) return;

    let cancelled = false;
    setLoadingMessages(true);

    // Heal stale streaming/loading flags: the agent:event listener unmounts
    // with the chat route, so a turn that finished while the user was on
    // another module leaves streamingById stuck at true — with the cancel
    // token long gone, the stop button becomes a no-op. Ask the backend.
    agentIsRunning(activeSessionId)
      .then((running) => {
        if (cancelled || running) return;
        const st = useChatStore.getState();
        st.setSessionStreaming(activeSessionId, false);
        st.setSessionLoading(activeSessionId, false);
      })
      .catch(() => {});

    Promise.all([
      getChatMessages(activeSessionId),
      getAgentSteps(activeSessionId),
    ])
      .then(([messages, steps]) => {
        if (!cancelled) {
          setMessages(messages);
          setAgentSteps(steps);
        }
      })
      .catch((err) => {
        console.error('Failed to load chat data:', err);
        if (!cancelled) {
          setMessages([]);
          setAgentSteps([]);
        }
      })
      .finally(() => {
        if (cancelled) return;
        setLoadingMessages(false);
        // History is settled: fire the hero input's queued first message.
        const pending = useChatStore.getState().pendingFirst;
        if (pending && pending.sessionId === activeSessionId) {
          useChatStore.getState().setPendingFirst(null);
          void sendFirstMessage(pending.sessionId, pending.text);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [activeSessionId, setMessages, setAgentSteps]);

  const handleRename = async (title: string) => {
    if (!activeSession) return;
    try {
      await agentRenameSession(activeSession.id, title);
      setSessions(
        sessions.map((s) => (s.id === activeSession.id ? { ...s, title } : s))
      );
    } catch (err) {
      console.error('Failed to rename session:', err);
    }
  };

  // Header badge model switcher: pick a global provider → it becomes the
  // session's model from the next turn (any inline custom llm is replaced,
  // same semantics as the config dialog's 自定义 toggle).
  const handleModelChange = async (providerId: string) => {
    if (!activeSession) return;
    try {
      await agentSetSessionModel(activeSession.id, [providerId], []);
      setSessions(
        sessions.map((s) =>
          s.id === activeSession.id
            ? { ...s, llm_provider_ids: [providerId], llm_models: [] }
            : s
        )
      );
    } catch (err) {
      console.error('Failed to switch model:', err);
    }
  };

  if (!activeSession) {
    return (
      <div className="flex flex-col items-center justify-center h-full gap-8 text-text-secondary">
        <div className="flex flex-col items-center">
          <div className="w-12 h-12 rounded-xl bg-gradient-to-br from-primary to-emerald-700 flex items-center justify-center text-black text-2xl mb-4">
            <Bot size={24} />
          </div>
          <p className="text-lg font-medium text-text-primary">有什么可以帮你？</p>
          <p className="text-sm mt-2">输入消息直接开始，或从左侧选择对话</p>
        </div>
        <HeroInput />
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full bg-background">
      <ChatHeader
        session={activeSession}
        projectName={sessionProject?.name}
        projectPath={sessionProject?.path}
        onRename={handleRename}
        onModelChange={handleModelChange}
      />
      <div className="flex-1 overflow-hidden relative">
        <MessageList />
        {loadingMessages && (
          <div className="absolute inset-0 flex items-center justify-center bg-background/60 backdrop-blur-[1px]">
            <Loader2 size={20} className="animate-spin text-text-secondary" />
          </div>
        )}
      </div>
      <MessageInput disabled={isStreaming} />
      <AskUserDialog />
    </div>
  );
}
