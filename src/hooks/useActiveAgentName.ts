import { useChatStore } from '@/stores/chatStore';

export function useActiveAgentName(): string {
  const sessions = useChatStore((s) => s.sessions);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const session = sessions.find((s) => s.id === activeSessionId);
  return session?.title || '思库';
}
