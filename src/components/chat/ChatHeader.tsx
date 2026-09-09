import { useEffect, useRef, useState } from 'react';
import { Pencil, FolderOpen, Bot, CalendarClock, Check, ChevronDown } from 'lucide-react';
import type { AgentSession, LlmProvider } from '@/lib/types';
import { llmProviderList } from '@/lib/tauri';
import { AgentAvatar } from './AgentAvatar';
import { TaskCenterDialog } from './TaskCenterDialog';

interface Props {
  session: AgentSession;
  projectName?: string;
  projectPath?: string;
  onRename: (title: string) => void;
  /** Switch the session to a global (provider-pool) model, next turn on. */
  onModelChange: (providerId: string) => void;
}

/** Chat panel header: editable thread title + project chip + model badge.
 *  The badge doubles as a model switcher over the global provider pool. */
export function ChatHeader({ session, projectName, projectPath, onRename, onModelChange }: Props) {
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(session.title);
  const [taskCenterOpen, setTaskCenterOpen] = useState(false);
  const [providers, setProviders] = useState<LlmProvider[]>([]);
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const modelMenuRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    setTitle(session.title);
    setEditing(false);
    setModelMenuOpen(false);
  }, [session.id, session.title]);

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

  const commit = () => {
    const t = title.trim();
    setEditing(false);
    if (t && t !== session.title) onRename(t);
    else setTitle(session.title);
  };

  // Inline llm_models (resolved on get-session) wins; otherwise resolve the
  // provider-pool reference to a real "provider / model" label. List-loaded
  // sessions carry no resolved models, so without this the badge fell back
  // to a literal "模型提供商" placeholder.
  const llm = session.llm_models?.[0];
  const poolId = session.llm_provider_ids?.[0];
  const pool = poolId ? providers.find((p) => p.id === poolId) : undefined;
  const modelLabel = llm
    ? `${llm.provider} / ${llm.model}`
    : pool
      ? `${pool.provider} / ${pool.model}`
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

  return (
    <div className="shrink-0 flex items-center gap-3 px-5 py-2.5 border-b border-surface-hover bg-background">
      <AgentAvatar name={session.title} color={session.color} size={28} />

      {editing ? (
        <input
          autoFocus
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              commit();
            }
            if (e.key === 'Escape') {
              setEditing(false);
              setTitle(session.title);
            }
          }}
          className="w-64 bg-surface border border-primary/50 rounded-lg px-2 py-1 text-sm text-text-primary outline-none"
        />
      ) : (
        <div className="flex items-center gap-1 min-w-0">
          <span className="text-sm font-semibold text-text-primary truncate max-w-[240px]">
            {session.title}
          </span>
          <button
            onClick={() => setEditing(true)}
            className="p-1 rounded hover:bg-surface-hover text-text-secondary/50 hover:text-text-primary"
            title="重命名对话"
          >
            <Pencil size={12} />
          </button>
        </div>
      )}

      <div className="flex items-center gap-1.5 text-[11px] text-text-secondary/70 min-w-0">
        {projectName && (
          <span
            className="flex items-center gap-1 px-1.5 py-0.5 rounded bg-surface-hover/60 truncate max-w-[180px]"
            title={projectPath}
          >
            <FolderOpen size={11} className="shrink-0" />
            <span className="truncate">{projectName}</span>
          </span>
        )}
        {modelLabel && (
          <div className="relative" ref={modelMenuRef}>
            <button
              onClick={() => setModelMenuOpen((o) => !o)}
              className="flex items-center gap-1 px-1.5 py-0.5 rounded bg-primary/10 text-primary hover:bg-primary/20 transition-colors max-w-[220px]"
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
        )}
      </div>

      <div className="flex-1" />

      <button
        onClick={() => setTaskCenterOpen(true)}
        title="任务中心"
        className="p-1.5 rounded hover:bg-surface-hover text-text-secondary hover:text-text-primary"
      >
        <CalendarClock size={14} />
      </button>

      {taskCenterOpen && (
        <TaskCenterDialog sessionId={session.id} onClose={() => setTaskCenterOpen(false)} />
      )}
    </div>
  );
}
