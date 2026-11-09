import { AgentList } from '@/components/chat/AgentList';
import { ChatPanel } from '@/components/chat/ChatPanel';
import { ListPanel } from '@/components/layout/ListPanel';

export function ChatPage() {
  return (
    <div className="flex h-full bg-background">
      <ListPanel width={256}>
        <AgentList />
      </ListPanel>
      <div className="flex-1 min-w-0">
        <ChatPanel />
      </div>
    </div>
  );
}
