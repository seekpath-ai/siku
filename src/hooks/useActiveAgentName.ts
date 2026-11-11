import { useChatStore } from '@/stores/chatStore';

/** Identity of the active session-as-agent: sidebar list, chat header and
 * message bubbles all render the avatar from this, so they stay consistent. */
export function useActiveAgentIdentity(): { name: string; color: string | null } {
  const sessions = useChatStore((s) => s.sessions);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const session = sessions.find((s) => s.id === activeSessionId);
  return { name: session?.title || '思库', color: session?.color ?? null };
}

export function useActiveAgentName(): string {
  return useActiveAgentIdentity().name;
}
