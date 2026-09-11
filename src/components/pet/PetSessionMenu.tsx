import { useEffect, useRef, useState } from 'react';
import { emit } from '@tauri-apps/api/event';
import { Bot, Check, Eraser, MoreVertical } from 'lucide-react';
import { usePetStore } from '@/stores/petStore';
import { useDialog } from '@/hooks/useDialog';
import { llmProviderList } from '@/lib/tauri';
import type { LlmProvider } from '@/lib/types';

interface Props {
  /** Runs after the session has been deleted (e.g. notify bubble / close the
   *  detached window). */
  onCleared: () => void;
}

/** Pet panel session menu ("⋯" in the header): model switcher (provider/model
 *  only, no custom display names — the panel is too narrow for a header
 *  badge) plus destructive session actions like clearing the history. */
export function PetSessionMenu({ onCleared }: Props) {
  const store = usePetStore();
  const { confirm, alert } = useDialog();
  const [open, setOpen] = useState(false);
  const [providers, setProviders] = useState<LlmProvider[]>([]);
  const menuRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    llmProviderList().then(setProviders).catch(() => {});
  }, []);

  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    window.addEventListener('mousedown', onClick);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('mousedown', onClick);
      window.removeEventListener('keydown', onKey);
    };
  }, [open]);

  const session = store.session;

  // Current model selection — mirrors SessionModelBadge: the pool reference
  // wins over a stored inline block; an unmatched inline custom config gets a
  // display-only "自定义" row.
  const llm = session?.llm_models?.[0];
  const poolId = session?.llm_provider_ids?.[0];
  const matchedCustom =
    llm && !poolId
      ? providers.find(
          (p) => p.provider === llm.provider && p.model === llm.model && p.base_url === llm.base_url
        )
      : undefined;
  const currentProviderId = poolId ?? matchedCustom?.id ?? null;
  const showCustomEntry = !!llm && !poolId && !matchedCustom;

  const handleModelChange = (providerId: string) => {
    if (providerId === currentProviderId) return;
    store.setSessionModel(providerId).catch((err) => {
      console.error('Failed to switch model:', err);
      alert(`切换模型失败：${err instanceof Error ? err.message : String(err)}`);
    });
  };

  const handleClear = async () => {
    setOpen(false);
    if (!session || store.streaming) return;
    const ok = await confirm(
      '清空当前会话的全部对话记录？此操作不可恢复，下次打开将开始新会话。',
      '清空会话记录'
    );
    if (!ok) return;
    const sessionId = session.id;
    try {
      await store.clearSession();
      // Other windows (main panel / detached chat) each hold their own store
      // instance — tell them this session is gone so they don't keep showing
      // a deleted conversation.
      emit('pet:session-cleared', sessionId).catch(() => {});
      onCleared();
    } catch (err) {
      console.error('clear pet session:', err);
      alert(`清空失败：${err instanceof Error ? err.message : String(err)}`);
    }
  };

  return (
    // mousedown must not bubble: in Pet.tsx the header doubles as the panel's
    // drag handle (DOM-level drag), and clicking the menu would start a drag.
    <div className="relative shrink-0" ref={menuRef} onMouseDown={(e) => e.stopPropagation()}>
      <button
        onClick={() => setOpen((o) => !o)}
        disabled={!session}
        title="会话菜单"
        className="w-7 h-7 flex items-center justify-center rounded text-text-secondary/70 hover:text-text-primary hover:bg-surface-hover transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
      >
        <MoreVertical size={13} strokeWidth={1.5} />
      </button>
      {open && (
        <div className="absolute top-full right-0 mt-1 z-50 min-w-[190px] py-1 bg-surface border border-surface-hover rounded-lg shadow-xl">
          <div className="px-3 pt-1 pb-0.5 text-[10px] uppercase tracking-wider text-text-secondary/50">
            模型
          </div>
          <div className="max-h-[220px] overflow-y-auto">
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
                onClick={() => handleModelChange(p.id)}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-text-primary hover:bg-surface-hover transition-colors"
                title="下一轮对话生效"
              >
                <span className="w-3 shrink-0">
                  {p.id === currentProviderId && <Check size={12} className="text-primary" />}
                </span>
                <Bot size={11} className="shrink-0 text-text-secondary/60" />
                <span className="truncate">
                  {p.provider} / {p.model}
                </span>
              </button>
            ))}
            {providers.length === 0 && !showCustomEntry && (
              <div className="px-3 py-1.5 text-xs text-text-secondary">
                未配置全局模型（设置 → 模型）
              </div>
            )}
          </div>

          <div className="my-1 border-t border-surface-hover" />

          <button
            onClick={handleClear}
            disabled={store.streaming}
            title={store.streaming ? '正在生成回复，请稍后再试' : undefined}
            className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-red-400 hover:bg-surface-hover transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            <Eraser size={12} className="shrink-0" />
            清空本会话记录
          </button>
        </div>
      )}
    </div>
  );
}
