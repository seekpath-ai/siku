import { useCallback, useEffect, useState } from 'react';
import { listen, type UnlistenFn } from '@/lib/events';
import { Bot, Loader2, ArrowUp, Laptop } from 'lucide-react';
import { MessageList } from './MessageList';
import { MessageInput } from './MessageInput';
import { ChatHeader } from './ChatHeader';
import { ChatSearchBar } from './ChatSearchBar';
import { ArtifactSidebar } from './ArtifactSidebar';
import { AskUserDialog } from './AskUserDialog';
import { useChatStore, MESSAGE_PAGE_SIZE } from '@/stores/chatStore';
import { mergeMessages } from '@/lib/chatMerge';
import { useProjectStore } from '@/stores/projectStore';
import { usePetContextStore } from '@/stores/petContextStore';
import { useStreamingChat } from '@/hooks/useStreamingChat';
import { getChatMessages, getAgentSteps, agentRenameSession, agentSetSessionModel, agentIsRunning, agentCreateSession, agentSendMessage, getDeviceId, agentSetSessionWorkingDir, agentSessionWorkspace } from '@/lib/tauri';
import { pickDirectory } from '@/lib/pickDirectory';
import { useDialogStore } from '@/stores/dialogStore';
import { useSyncTick } from '@/stores/syncTickStore';

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
        toolsEnabled: null,
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
  const tempId = `user_${Date.now()}`;
  store.addMessage({
    id: tempId,
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
    const userMessageId = await agentSendMessage(sessionId, text);
    useChatStore.getState().swapMessageId(tempId, userMessageId);
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
  const messages = useChatStore((s) => s.messages);
  const { projects } = useProjectStore();
  const [loadingMessages, setLoadingMessages] = useState(false);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const [artifactsOpen, setArtifactsOpen] = useState(false);
  const [autoWorkspace, setAutoWorkspace] = useState<string | null>(null);
  // Current machine id, used to spot a conversation continued elsewhere.
  const [deviceId, setDeviceId] = useState('');

  useEffect(() => {
    getDeviceId().then(setDeviceId).catch(() => {});
  }, []);

  // Remote changes (typically the other device continuing this conversation)
  // should show up without closing/reopening the session. The event carries no
  // table info, so re-fetch the newest page and merge it; older pages stay
  // loaded. Skip while streaming to avoid fighting the live turn.
  useEffect(() => {
    if (!activeSessionId) return;
    let unlisten: UnlistenFn | undefined;
    listen('sync:remote_applied', () => {
      if (useChatStore.getState().isStreaming) return;
      void Promise.all([
        getChatMessages(activeSessionId, { limit: MESSAGE_PAGE_SIZE }),
        getAgentSteps(activeSessionId).catch(() => []),
      ])
        .then(([page, steps]) => {
          const st = useChatStore.getState();
          if (st.activeSessionId !== activeSessionId) return;
          // Rows can be UPDATED on the peer (artifact links appended to an
          // assistant message, message origin attached after the insert), and
          // mergeMessages keeps existing ids verbatim. Refresh known rows from
          // the page first so edits are not masked by the local copy.
          const pageById = new Map(page.messages.map((m) => [m.id, m]));
          const refreshed = st.messages.map((m) => pageById.get(m.id) ?? m);
          st.setMessages(mergeMessages(refreshed, page.messages));
          st.setAgentSteps(steps);
          useSyncTick.getState().bump();
        })
        .catch((err) => console.error('Failed to refresh chat after sync:', err));
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      unlisten?.();
    };
  }, [activeSessionId]);

  // Ctrl+F opens the in-conversation search bar.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'f' && activeSessionId) {
        e.preventDefault();
        setSearchOpen(true);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [activeSessionId]);

  const activeSession = sessions.find((s) => s.id === activeSessionId) ?? null;

  // The latest message's origin tells us whether this conversation is being
  // continued here for the first time (messages are synced, so a fresh device
  // sees the last turn produced by the other machine).
  const lastMessage = messages.length > 0 ? messages[messages.length - 1] : null;
  const foreignOrigin =
    !!lastMessage?.origin_device_id &&
    !!deviceId &&
    lastMessage.origin_device_id !== deviceId
      ? {
          name: lastMessage.origin_device_name || '另一台设备',
          dir: lastMessage.origin_working_dir || '',
        }
      : null;

  const bindProjectPath = async () => {
    if (!sessionProject) return;
    const dir = await pickDirectory(sessionProject.path ?? '');
    if (!dir) return;
    try {
      await useProjectStore.getState().bindPath(sessionProject.id, dir);
    } catch (err) {
      useDialogStore.getState().alert(`绑定项目目录失败：${err}`, { kind: 'error' });
    }
  };

  const bindLocalWorkingDir = async () => {
    if (!activeSession) return;
    const dir = await pickDirectory(activeSession.working_dir ?? '');
    if (!dir) return;
    try {
      await agentSetSessionWorkingDir(activeSession.id, dir);
      setSessions(
        useChatStore
          .getState()
          .sessions.map((s) => (s.id === activeSession.id ? { ...s, working_dir: dir } : s)),
      );
    } catch (err) {
      useDialogStore.getState().alert(`绑定工作目录失败：${err}`, { kind: 'error' });
    }
  };

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
  // The effective local folder can come from the session override, the
  // project bound on this device, or the app-owned session workspace. The
  // latter is a perfectly valid local binding, so it must not trigger the
  //「绑定本机工作目录」prompt just because the session has no explicit path.
  const explicitLocalDir = activeSession?.working_dir?.trim() || '';
  const projectLocalDir = sessionProject?.path?.trim() || '';
  const localBoundDir = explicitLocalDir || projectLocalDir || autoWorkspace || '';
  const localDirSource = explicitLocalDir
    ? '会话目录'
    : projectLocalDir
      ? '项目目录'
      : autoWorkspace
        ? '自动工作区'
        : '';

  useEffect(() => {
    if (!activeSessionId) {
      setAutoWorkspace(null);
      return;
    }
    if (explicitLocalDir || projectLocalDir) {
      setAutoWorkspace(null);
      return;
    }
    let cancelled = false;
    agentSessionWorkspace(activeSessionId)
      .then((path) => {
        if (!cancelled) setAutoWorkspace(path);
      })
      .catch(() => {
        if (!cancelled) setAutoWorkspace(null);
      });
    return () => {
      cancelled = true;
    };
  }, [activeSessionId, explicitLocalDir, projectLocalDir]);

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
      getChatMessages(activeSessionId, { limit: MESSAGE_PAGE_SIZE }),
      // 步骤查询失败只丢推理过程卡，不能拖垮消息本体（曾因此整页历史空白）。
      getAgentSteps(activeSessionId).catch((err) => {
        console.error('Failed to load agent steps:', err);
        return [];
      }),
    ])
      .then(([page, steps]) => {
        if (!cancelled) {
          setMessages(page.messages);
          useChatStore.getState().setHasMoreMessages(page.hasMore);
          setAgentSteps(steps);
        }
      })
      .catch((err) => {
        console.error('Failed to load chat data:', err);
        if (!cancelled) {
          setMessages([]);
          useChatStore.getState().setHasMoreMessages(false);
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

  // Outbound files settle AFTER the turn's `done` event: the router uploads
  // them once the reply row is persisted, then rewrites the attachments. The
  // history reload above therefore still shows 「发送中」 — patch the row in
  // place when the backend reports the final status instead of making the
  // user switch tabs to re-read it.
  useEffect(() => {
    if (!activeSessionId) return;
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;
    void listen<{ session_id: string; message_id: string; attachments: string }>(
      'chat:message_updated',
      (ev) => {
        const p = ev.payload;
        if (p.session_id !== activeSessionId) return;
        const st = useChatStore.getState();
        const msg = st.messages.find((m) => m.id === p.message_id);
        if (msg) {
          st.patchMessage({ ...msg, attachments: p.attachments });
          return;
        }
        // The history reload is still in flight; re-read the session once so
        // the settled row lands instead of the stale `pending` copy. Merge —
        // never wholesale-replace: the page is capped at MESSAGE_PAGE_SIZE,
        // so replacing would drop older loaded messages and temp bubbles.
        void getChatMessages(p.session_id, { limit: MESSAGE_PAGE_SIZE })
          .then((page) => {
            if (cancelled) return;
            const cur = useChatStore.getState();
            if (cur.activeSessionId !== p.session_id) return;
            cur.setMessages(mergeMessages(cur.messages, page.messages));
          })
          .catch(() => {});
      },
    ).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [activeSessionId]);

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

  // Scroll-up pagination: fetch the page older than the oldest loaded row.
  // MessageList anchors the scroll position across the prepend.
  const loadOlderMessages = async () => {
    const { activeSessionId: sid, messages, hasMoreMessages } = useChatStore.getState();
    if (!sid || !hasMoreMessages || messages.length === 0) return;
    setLoadingOlder(true);
    try {
      const oldest = messages[0];
      const page = await getChatMessages(sid, {
        before: { createdAt: oldest.created_at, id: oldest.id },
        limit: MESSAGE_PAGE_SIZE,
      });
      const st = useChatStore.getState();
      st.prependMessages(page.messages);
      st.setHasMoreMessages(page.hasMore);
    } catch (err) {
      console.error('Failed to load older messages:', err);
    } finally {
      setLoadingOlder(false);
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

  const reloadAfterArtifactDelete = useCallback(async () => {
    if (!activeSessionId) return;
    try {
      const page = await getChatMessages(activeSessionId, { limit: MESSAGE_PAGE_SIZE });
      const st = useChatStore.getState();
      if (st.activeSessionId !== activeSessionId) return;
      const pageById = new Map(page.messages.map((m) => [m.id, m]));
      const refreshed = st.messages.map((m) => pageById.get(m.id) ?? m);
      st.setMessages(mergeMessages(refreshed, page.messages));
    } catch (err) {
      console.error('Failed to refresh messages after artifact delete:', err);
    }
  }, [activeSessionId]);

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
        onToggleSearch={() => setSearchOpen((v) => !v)}
        onToggleArtifacts={() => setArtifactsOpen((v) => !v)}
        artifactsOpen={artifactsOpen}
        onBindProject={() => void bindProjectPath()}
        hasLocalWorkingDir={!!activeSession.working_dir?.trim()}
      />
      {activeSession && foreignOrigin && (
        <div className="shrink-0 flex items-center gap-2 px-5 py-1.5 border-b border-surface-hover bg-warning/10 text-[11px] text-text-secondary">
          <Laptop size={13} className="shrink-0 text-warning" />
          <span className="truncate">
            此会话最近在「{foreignOrigin.name}」上继续
            {foreignOrigin.dir ? `（当时工作目录 ${foreignOrigin.dir}）` : ''}
            ，历史中的路径/环境可能不属于本机。
          </span>
          {localBoundDir ? (
            <span className="ml-auto shrink-0 text-text-secondary/60 truncate max-w-[220px]" title={localBoundDir}>
              本机目录：{localBoundDir}
              {localDirSource ? `（${localDirSource}）` : ''}
            </span>
          ) : (
            <button
              onClick={() => void bindLocalWorkingDir()}
              className="ml-auto shrink-0 px-2 py-0.5 rounded bg-warning/20 text-text-primary hover:bg-warning/30 transition-colors"
            >
              绑定本机工作目录
            </button>
          )}
        </div>
      )}
      {searchOpen && <ChatSearchBar onClose={() => setSearchOpen(false)} />}
      <div className="flex-1 min-h-0 overflow-hidden flex">
        <div className="flex-1 min-w-0 relative">
          <MessageList onLoadOlder={loadOlderMessages} loadingOlder={loadingOlder} />
          {loadingMessages && (
            <div className="absolute inset-0 flex items-center justify-center bg-background/60 backdrop-blur-[1px]">
              <Loader2 size={20} className="animate-spin text-text-secondary" />
            </div>
          )}
        </div>
        {artifactsOpen && (
          <ArtifactSidebar
            sessionId={activeSession.id}
            refreshToken={`${messages.length}:${isStreaming}`}
            onClose={() => setArtifactsOpen(false)}
            onChanged={() => void reloadAfterArtifactDelete()}
          />
        )}
      </div>
      <MessageInput disabled={isStreaming} />
      <AskUserDialog />
    </div>
  );
}
