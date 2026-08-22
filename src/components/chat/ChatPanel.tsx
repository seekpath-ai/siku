import { useEffect, useState } from 'react';
import { Bot, Loader2 } from 'lucide-react';
import { MessageList } from './MessageList';
import { MessageInput } from './MessageInput';
import { ChatHeader } from './ChatHeader';
import { AskUserDialog } from './AskUserDialog';
import { useChatStore } from '@/stores/chatStore';
import { useProjectStore } from '@/stores/projectStore';
import { usePetContextStore } from '@/stores/petContextStore';
import { useStreamingChat } from '@/hooks/useStreamingChat';
import { getChatMessages, getAgentSteps, agentRenameSession } from '@/lib/tauri';

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
        if (!cancelled) setLoadingMessages(false);
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

  if (!activeSession) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-text-secondary">
        <div className="w-12 h-12 rounded-xl bg-gradient-to-br from-primary to-emerald-700 flex items-center justify-center text-black text-2xl mb-4">
          <Bot size={24} />
        </div>
        <p className="text-lg font-medium text-text-primary">新建或选择一个对话</p>
        <p className="text-sm mt-2">开始与 AI 智能体对话</p>
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
