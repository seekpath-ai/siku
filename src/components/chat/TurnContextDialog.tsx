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

function fmtTime(iso: string): string {
  try {
    return new Date(iso).toLocaleString('zh-CN', {
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
    });
  } catch {
    return '';
  }
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

  // Prefer the snapshot's history (the exact records the turn was sent —
  // max_memory_rounds already applied). Snapshots predating the history
  // column fall back to the store's message list (an approximation: it shows
  // ALL earlier messages, not what was actually sent).
  const snapshotHistory = useMemo(() => {
    if (!ctx?.history) return null;
    try {
      const v = JSON.parse(ctx.history);
      return Array.isArray(v)
        ? (v as { role: string; content: string; time?: string | null }[]).filter(
            (m) => m.role === 'user' || m.role === 'assistant'
          )
        : null;
    } catch {
      return null;
    }
  }, [ctx]);

  const history = useMemo(() => {
    if (snapshotHistory) {
      return snapshotHistory.map((m) => ({
        role: m.role,
        content: m.content,
        time: m.time ?? '',
      }));
    }
    const all = useChatStore.getState().messages;
    const idx = all.findIndex((m) => m.id === message.id);
    return (idx >= 0 ? all.slice(0, idx) : all)
      .filter((m) => m.role === 'user' || m.role === 'assistant')
      .map((m) => ({ role: m.role, content: m.content, time: m.created_at }));
  }, [snapshotHistory, message.id]);

  // Group flat history into rounds: a user message opens a round, assistant
  // replies attach to it. A leading assistant message (memory truncated
  // mid-round) forms a user-less first round.
  const roundGroups = useMemo(() => {
    const groups: { items: typeof history }[] = [];
    for (const m of history) {
      if (m.role === 'user' || groups.length === 0) groups.push({ items: [] });
      groups[groups.length - 1].items.push(m);
    }
    return groups;
  }, [history]);

  // 轮数按用户消息计（一轮 = 一次用户提问 + 助手应答），比消息总数直观。
  const rounds = history.filter((m) => m.role === 'user').length;

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
            <h3 className="text-xs font-semibold text-text-secondary mb-1.5">
              系统提示词{ctx && <span className="ml-1.5 font-normal text-text-secondary/50">{fmtTime(ctx.created_at)} 快照</span>}
            </h3>
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
              历史记忆（{rounds} 轮 · {history.length} 条）
              {!snapshotHistory && loaded && (
                <span className="ml-1.5 font-normal text-text-secondary/50">为当前全部消息，非当轮实际发送</span>
              )}
            </h3>
            {history.length === 0 ? (
              <p className="text-xs text-text-secondary/60">无更早的历史消息。</p>
            ) : (
              <div className="space-y-2">
                {roundGroups.map((g, gi) => (
                  <div
                    key={gi}
                    className="rounded-lg border border-surface-hover/60 overflow-hidden"
                  >
                    <div className="px-3 py-1 bg-surface-hover/40 text-[10px] font-medium text-text-secondary/70">
                      第 {gi + 1} 轮
                      {g.items[0]?.time && (
                        <span className="ml-1.5 font-normal text-text-secondary/50">
                          {fmtTime(g.items[0].time)}
                        </span>
                      )}
                    </div>
                    <div className="space-y-1.5 p-1.5">
                      {g.items.map((m, i) => (
                        <div key={i} className="rounded-lg bg-black/20 px-3 py-1.5 text-xs">
                          <span className={m.role === 'user' ? 'text-primary' : 'text-emerald-400'}>
                            {m.role === 'user' ? '用户' : '助手'}
                          </span>
                          {m.time && (
                            <span className="ml-1.5 text-text-secondary/50">{fmtTime(m.time)}</span>
                          )}
                          <p className="mt-0.5 whitespace-pre-wrap [overflow-wrap:anywhere] text-text-primary/80 line-clamp-3">
                            {m.content}
                          </p>
                        </div>
                      ))}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </section>

          <section>
            <h3 className="text-xs font-semibold text-text-secondary mb-1.5">
              用户消息<span className="ml-1.5 font-normal text-text-secondary/50">{fmtTime(message.created_at)}</span>
            </h3>
            <div className="rounded-lg bg-black/20 px-3 py-2 text-xs whitespace-pre-wrap [overflow-wrap:anywhere] text-text-primary/90">
              {message.content}
            </div>
          </section>
        </div>
      </div>
    </div>
  );
}
