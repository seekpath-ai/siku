import { useEffect, useState } from 'react';
import { Pencil, FolderOpen, CalendarClock, Search, Package } from 'lucide-react';
import type { AgentSession } from '@/lib/types';
import { AgentAvatar } from './AgentAvatar';
import { SessionModelBadge } from './SessionModelBadge';
import { TaskCenterDialog } from './TaskCenterDialog';

interface Props {
  session: AgentSession;
  projectName?: string;
  projectPath?: string;
  onRename: (title: string) => void;
  /** Switch the session to a global (provider-pool) model, next turn on. */
  onModelChange: (providerId: string) => void;
  /** Toggle the in-conversation search bar. */
  onToggleSearch: () => void;
  /** Toggle the session artifact sidebar. */
  onToggleArtifacts: () => void;
  artifactsOpen: boolean;
  /** Bind a synced project (empty path) to a local folder. */
  onBindProject?: () => void;
  /** The session has its own local working directory; the project path is
   *  optional and must not show a redundant 「绑定」 prompt. */
  hasLocalWorkingDir?: boolean;
}

/** Chat panel header: editable thread title + project chip + model badge.
 *  The badge doubles as a model switcher over the global provider pool. */
export function ChatHeader({ session, projectName, projectPath, onRename, onModelChange, onToggleSearch, onToggleArtifacts, artifactsOpen, onBindProject, hasLocalWorkingDir = false }: Props) {
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(session.title);
  const [taskCenterOpen, setTaskCenterOpen] = useState(false);

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
            title={projectPath || '本项目尚未在本机绑定目录'}
          >
            <FolderOpen size={11} className="shrink-0" />
            <span className="truncate">{projectName}</span>
            {!projectPath && !hasLocalWorkingDir && onBindProject && (
              <button
                onClick={onBindProject}
                className="shrink-0 px-1 rounded bg-warning/20 text-text-primary hover:bg-warning/30"
                title="在本机绑定项目目录"
              >
                绑定
              </button>
            )}
          </span>
        )}
        <SessionModelBadge session={session} onModelChange={onModelChange} />
      </div>

      <div className="flex-1" />

      <button
        onClick={onToggleArtifacts}
        title={artifactsOpen ? '关闭会话产物侧栏' : '打开会话产物侧栏'}
        className={`p-1.5 rounded hover:bg-surface-hover ${
          artifactsOpen ? 'text-primary' : 'text-text-secondary hover:text-text-primary'
        }`}
      >
        <Package size={14} />
      </button>

      <button
        onClick={onToggleSearch}
        title="搜索对话（Ctrl+F）"
        className="p-1.5 rounded hover:bg-surface-hover text-text-secondary hover:text-text-primary"
      >
        <Search size={14} />
      </button>

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
