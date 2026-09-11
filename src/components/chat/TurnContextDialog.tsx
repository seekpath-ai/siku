import { useEffect, useMemo, useState } from 'react';
import { X } from 'lucide-react';
import { agentGetTurnContext, type TurnContext } from '@/lib/tauri';
import { useChatStore } from '@/stores/chatStore';
import type { ChatMessage } from '@/lib/types';

interface Props {
  /** The user message whose turn is being inspected. */
  message: ChatMessage;
  onClose: () => void;
}

/** Read-only "what the model saw" viewer for one turn: the system-prompt
 *  snapshot taken at send time (turn_contexts, device-local), the history
 *  leading up to the user message, and the user message itself. */
export function TurnContextDialog({ message, onClose }: Props) {
  const [ctx, setCtx] = useState<TurnContext | null>(null);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    agentGetTurnContext(message.id)
      .then(setCtx)
      .catch(() => setCtx(null))
      .finally(() => setLoaded(true));
  }, [message.id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  // History as the store currently holds it: every message before this user
  // message (the LLM saw the memory-file version, which mirrors these).
  const history = useMemo(() => {
    const all = useChatStore.getState().messages;
    const idx = all.findIndex((m) => m.id === message.id);
    return (idx >= 0 ? all.slice(0, idx) : all).filter(
      (m) => m.role === 'user' || m.role === 'assistant'
    );
  }, [message.id]);

  return (
    <div className="fixed inset-0 z-[6000] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div className="relative w-full max-w-2xl max-h-[80vh] mx-4 flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-2 px-4 py-3 border-b border-surface-hover shrink-0">
          <span className="text-sm font-medium text-text-primary">本轮上下文</span>
          <span className="text-[11px] text-text-secondary/60">发送该消息时模型实际看到的内容</span>
          <button
            onClick={onClose}
            className="ml-auto p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
          >
            <X size={14} />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-4 py-3 space-y-4">
          <section>
            <h3 className="text-xs font-semibold text-text-secondary mb-1.5">系统提示词</h3>
            {!loaded ? (
              <p className="text-xs text-text-secondary/60">加载中…</p>
            ) : ctx ? (
              <pre className="max-h-64 overflow-y-auto whitespace-pre-wrap [overflow-wrap:anywhere] rounded-lg bg-black/30 px-3 py-2 text-xs leading-relaxed text-text-primary/90 font-mono">
                {ctx.system_prompt}
              </pre>
            ) : (
              <p className="text-xs text-text-secondary/60">
                该轮没有上下文快照（仅支持快照功能上线后发送的消息）。
              </p>
            )}
          </section>

          <section>
            <h3 className="text-xs font-semibold text-text-secondary mb-1.5">
              历史记忆（{history.length} 条）
            </h3>
            {history.length === 0 ? (
              <p className="text-xs text-text-secondary/60">无更早的历史消息。</p>
            ) : (
              <div className="space-y-1.5">
                {history.map((m) => (
                  <div key={m.id} className="rounded-lg bg-black/20 px-3 py-1.5 text-xs">
                    <span className={m.role === 'user' ? 'text-primary' : 'text-emerald-400'}>
                      {m.role === 'user' ? '用户' : '助手'}
                    </span>
                    <p className="mt-0.5 whitespace-pre-wrap [overflow-wrap:anywhere] text-text-primary/80 line-clamp-3">
                      {m.content}
                    </p>
                  </div>
                ))}
              </div>
            )}
          </section>

          <section>
            <h3 className="text-xs font-semibold text-text-secondary mb-1.5">用户消息</h3>
            <div className="rounded-lg bg-black/20 px-3 py-2 text-xs whitespace-pre-wrap [overflow-wrap:anywhere] text-text-primary/90">
              {message.content}
            </div>
          </section>
        </div>
      </div>
    </div>
  );
}
