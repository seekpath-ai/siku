import { useEffect, useRef, useState } from 'react';
import { Bot, Check, ChevronDown } from 'lucide-react';
import type { AgentSession, LlmProvider } from '@/lib/types';
import { llmProviderList } from '@/lib/tauri';

interface Props {
  session: AgentSession;
  /** Switch the session to a global (provider-pool) model, next turn on. */
  onModelChange: (providerId: string) => void;
  /** Tailwind max-w-* cap for the badge label (space varies by container). */
  maxWidthClass?: string;
}

/** Model badge + switcher over the global provider pool. Shared by the chat
 *  header and the pet panel title bars so both switch a session's model the
 *  same way (pool reference from the next turn, replacing any inline custom). */
export function SessionModelBadge({ session, onModelChange, maxWidthClass = 'max-w-[220px]' }: Props) {
  const [providers, setProviders] = useState<LlmProvider[]>([]);
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const modelMenuRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    setModelMenuOpen(false);
  }, [session.id]);

  useEffect(() => {
    llmProviderList().then(setProviders).catch(() => {});
  }, []);

  // Close the model menu on outside click / Escape.
  useEffect(() => {
    if (!modelMenuOpen) return;
    const onClick = (e: MouseEvent) => {
      if (modelMenuRef.current && !modelMenuRef.current.contains(e.target as Node)) {
        setModelMenuOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setModelMenuOpen(false);
    };
    window.addEventListener('mousedown', onClick);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('mousedown', onClick);
      window.removeEventListener('keydown', onKey);
    };
  }, [modelMenuOpen]);

  // Precedence must mirror the runtime (build_agent_config): the pool
  // reference wins over inline llm_models when BOTH are stored (legacy
  // sessions can carry a stale inline block next to the pool id). Showing
  // the inline block first made the badge disagree with the menu's check
  // mark — and with the model actually serving the turn.
  const llm = session.llm_models?.[0];
  const poolId = session.llm_provider_ids?.[0];
  const pool = poolId ? providers.find((p) => p.id === poolId) : undefined;
  const modelLabel = pool
    ? `${pool.provider} / ${pool.model}`
    : llm
      ? `${llm.provider} / ${llm.model}`
      : '';

  // Current selection in the switcher. A custom (inline) LLM that exactly
  // matches a global provider (provider+model+base_url) is deduped INTO that
  // entry; an unmatched custom gets its own display-only "自定义" row.
  const matchedCustom =
    llm && !poolId
      ? providers.find(
          (p) => p.provider === llm.provider && p.model === llm.model && p.base_url === llm.base_url
        )
      : undefined;
  const currentProviderId = poolId ?? matchedCustom?.id ?? null;
  const showCustomEntry = !!llm && !poolId && !matchedCustom;

  if (!modelLabel) return null;

  return (
    <div className="relative shrink-0" ref={modelMenuRef}>
      <button
        onClick={() => setModelMenuOpen((o) => !o)}
        className={`flex items-center gap-1 px-1.5 py-0.5 rounded bg-primary/10 text-primary hover:bg-primary/20 transition-colors ${maxWidthClass}`}
        title="点击切换模型（下一轮对话生效）"
      >
        <Bot size={11} className="shrink-0" />
        <span className="truncate">{modelLabel}</span>
        <ChevronDown size={10} className="shrink-0 opacity-60" />
      </button>
      {modelMenuOpen && (
        <div className="absolute top-full left-0 mt-1 z-50 min-w-[240px] max-h-[300px] overflow-y-auto py-1 bg-surface border border-surface-hover rounded-lg shadow-xl">
          {showCustomEntry && llm && (
            <div
              className="flex items-center gap-2 px-3 py-1.5 text-xs text-text-secondary"
              title="内嵌的自定义 LLM 配置；选择下方任一全局模型即覆盖它"
            >
              <Check size={12} className="shrink-0 text-primary" />
              <span className="truncate">自定义（{llm.provider} / {llm.model}）</span>
            </div>
          )}
          {providers.map((p) => (
            <button
              key={p.id}
              onClick={() => {
                setModelMenuOpen(false);
                if (p.id !== currentProviderId) onModelChange(p.id);
              }}
              className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-text-primary hover:bg-surface-hover transition-colors"
            >
              <span className="w-3 shrink-0">
                {p.id === currentProviderId && <Check size={12} className="text-primary" />}
              </span>
              <span className="truncate">
                {p.name} ({p.provider}/{p.model})
              </span>
            </button>
          ))}
          {providers.length === 0 && !showCustomEntry && (
            <div className="px-3 py-1.5 text-xs text-text-secondary">
              未配置全局模型（设置 → 模型）
            </div>
          )}
        </div>
      )}
    </div>
  );
}
