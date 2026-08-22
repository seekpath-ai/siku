import { useEffect, useState } from 'react';
import { Pencil, FolderOpen, Bot } from 'lucide-react';
import type { AgentSession } from '@/lib/types';
import { AgentAvatar } from './AgentAvatar';

interface Props {
  session: AgentSession;
  projectName?: string;
  projectPath?: string;
  onRename: (title: string) => void;
}

/** Chat panel header: editable thread title + project chip + model badge. */
export function ChatHeader({ session, projectName, projectPath, onRename }: Props) {
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(session.title);

  useEffect(() => {
    setTitle(session.title);
    setEditing(false);
  }, [session.id, session.title]);

  const commit = () => {
    const t = title.trim();
    setEditing(false);
    if (t && t !== session.title) onRename(t);
    else setTitle(session.title);
  };

  const llm = session.llm_models?.[0];
  const modelLabel = llm
    ? `${llm.provider} / ${llm.model}`
    : session.llm_provider_ids?.length
      ? '模型提供商'
      : '';

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
          <span
            className="flex items-center gap-1 px-1.5 py-0.5 rounded bg-primary/10 text-primary truncate max-w-[180px]"
            title={modelLabel}
          >
            <Bot size={11} className="shrink-0" />
            <span className="truncate">{modelLabel}</span>
          </span>
        )}
      </div>

      <div className="flex-1" />
    </div>
  );
}
