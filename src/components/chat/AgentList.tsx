import { useEffect, useCallback, useState, useRef, useMemo } from 'react';
import {
  MessageSquarePlus,
  GitPullRequest,
  CalendarClock,
  Puzzle,
  MoreHorizontal,
  FolderPlus,
  FolderOpen,
  Pin,
  Trash2,
  Settings,
  Loader2,
} from 'lucide-react';
import { useChatStore } from '@/stores/chatStore';
import { useProjectStore } from '@/stores/projectStore';
import {
  agentListSessions,
  agentCreateSession,
  agentUpdateSession,
  agentDeleteSession,
  agentPinSession,
} from '@/lib/tauri';
import { useDialog } from '@/hooks/useDialog';
import { ConfirmButton } from '@/components/ui/ConfirmButton';
import { AgentAvatar } from './AgentAvatar';
import { AgentCreateDialog } from './AgentCreateDialog';
import { AgentConfigPanel } from './AgentConfigPanel';
import { DEFAULT_TOOLS } from '@/lib/agent-tools';
import type { AgentSession, LlmConfigBlock, ApprovalConfig } from '@/lib/types';

interface AgentCreateInput {
  title: string;
  systemPrompt?: string;
  tools: string[];
  projectId?: string;
  workingDir: string | null;
  visionProviderId: string | null;
  webProxy: string | null;
  llmProviderIds: string[];
  llmModels: LlmConfigBlock[];
  approvalConfig: ApprovalConfig;
  maxLoops: number;
  maxTokens: number;
  maxMemoryRounds: number;
  memoryDir?: string;
  skillsDir?: string;
}

interface ContextMenuState {
  visible: boolean;
  x: number;
  y: number;
  agentId: string;
}

function MenuRow({
  icon,
  label,
  onClick,
  primary,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
  primary?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      className={`w-full flex items-center gap-2.5 px-2 py-1.5 rounded-md text-[13px] transition-colors ${
        primary ? 'text-codex-primary' : 'text-codex-secondary'
      } hover:bg-codex-hover hover:text-codex-primary`}
    >
      <span className="text-codex-muted shrink-0">{icon}</span>
      {label}
    </button>
  );
}

export function AgentList() {
  const { sessions, activeSessionId, setSessions, setActiveSession, removeSession } =
    useChatStore();
  const {
    projects,
    activeProjectId,
    loading: projectsLoading,
    groupBy,
    sortBy,
    load,
    addProject,
    removeProject,
    switchProject,
    setGroupBy,
    setSortBy,
  } = useProjectStore();
  const { alert } = useDialog();

  const [showCreate, setShowCreate] = useState(false);
  const [configAgent, setConfigAgent] = useState<AgentSession | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenuState>({
    visible: false,
    x: 0,
    y: 0,
    agentId: '',
  });
  const [organizeOpen, setOrganizeOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const organizeRef = useRef<HTMLDivElement>(null);

  const activeProject = projects.find((p) => p.id === activeProjectId) ?? null;

  // Load projects once on mount.
  useEffect(() => {
    load();
  }, [load]);

  // Load all sessions; select the first when none is active.
  // Pet/domain sessions (built-in per-page agents) stay hidden from this list.
  const loadSessions = useCallback(async () => {
    try {
      const list = await agentListSessions();
      const visible = list.filter((s) => !s.domain);
      setSessions(visible);
      const current = useChatStore.getState().activeSessionId;
      if (!visible.some((s) => s.id === current)) {
        setActiveSession(visible[0]?.id ?? null);
      }
    } catch (err) {
      console.error('Failed to load sessions:', err);
    }
  }, [setSessions, setActiveSession]);

  useEffect(() => {
    loadSessions();
  }, [loadSessions]);

  // Close the organize menu on outside click.
  useEffect(() => {
    if (!organizeOpen) return;
    const close = (e: MouseEvent) => {
      if (organizeRef.current?.contains(e.target as Node)) return;
      setOrganizeOpen(false);
    };
    document.addEventListener('mousedown', close, true);
    return () => document.removeEventListener('mousedown', close, true);
  }, [organizeOpen]);

  // Close the agent context menu on outside click.
  useEffect(() => {
    if (!contextMenu.visible) return;
    const close = (e: MouseEvent) => {
      if (menuRef.current?.contains(e.target as Node)) return;
      setContextMenu({ visible: false, x: 0, y: 0, agentId: '' });
    };
    const timer = setTimeout(() => document.addEventListener('mousedown', close, true), 0);
    return () => {
      clearTimeout(timer);
      document.removeEventListener('mousedown', close, true);
    };
  }, [contextMenu.visible]);

  // Chats visible in the list: filtered by the selected project, then sorted.
  const visibleSessions = useMemo(() => {
    const list = activeProjectId
      ? sessions.filter((s) => s.project_id === activeProjectId)
      : sessions;
    const sorted = [...list];
    switch (sortBy) {
      case 'updated':
        sorted.sort((a, b) => b.updated_at.localeCompare(a.updated_at));
        break;
      case 'manual':
        sorted.sort(
          (a, b) =>
            (a.sort_order ?? 0) - (b.sort_order ?? 0) ||
            b.updated_at.localeCompare(a.updated_at)
        );
        break;
      default: // priority
        sorted.sort(
          (a, b) =>
            (b.is_pinned ? 1 : 0) - (a.is_pinned ? 1 : 0) ||
            b.updated_at.localeCompare(a.updated_at)
        );
    }
    return sorted;
  }, [sessions, activeProjectId, sortBy]);

  // Group chats by project (only when no project filter and grouping is enabled).
  const grouped = useMemo(() => {
    if (activeProjectId || groupBy !== 'project') return null;
    const map = new Map<string, AgentSession[]>();
    for (const s of visibleSessions) {
      const key = s.project_id ?? '';
      const arr = map.get(key) ?? [];
      arr.push(s);
      map.set(key, arr);
    }
    return map;
  }, [visibleSessions, activeProjectId, groupBy]);

  const projectChatCount = (pid: string | null) =>
    sessions.filter((s) => s.project_id === pid).length;

  const handleContextMenu = (e: React.MouseEvent, agentId: string) => {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ visible: true, x: e.clientX, y: e.clientY, agentId });
  };

  const handleNewAgent = async (input: AgentCreateInput) => {
    const session = await agentCreateSession({
      title: input.title,
      agentMode: 'chat',
      toolsEnabled: input.tools,
      systemPrompt: input.systemPrompt,
      projectId: input.projectId ?? activeProjectId ?? undefined,
      workingDir: input.workingDir,
      visionProviderId: input.visionProviderId,
      webProxy: input.webProxy,
      llmProviderIds: input.llmProviderIds,
      llmModels: input.llmModels,
      approvalConfig: input.approvalConfig,
      maxLoops: input.maxLoops,
      maxTokens: input.maxTokens,
      maxMemoryRounds: input.maxMemoryRounds,
      memoryDir: input.memoryDir,
      skillsDir: input.skillsDir,
    });
    await loadSessions();
    setActiveSession(session.id);
  };

  const handleUpdateAgent = async (sessionId: string, input: AgentCreateInput) => {
    await agentUpdateSession(sessionId, {
      title: input.title,
      agentMode: 'chat',
      toolsEnabled: input.tools,
      systemPrompt: input.systemPrompt,
      workingDir: input.workingDir,
      visionProviderId: input.visionProviderId,
      webProxy: input.webProxy,
      llmProviderIds: input.llmProviderIds,
      llmModels: input.llmModels,
      approvalConfig: input.approvalConfig,
      maxLoops: input.maxLoops,
      maxTokens: input.maxTokens,
      maxMemoryRounds: input.maxMemoryRounds,
      memoryDir: input.memoryDir,
      skillsDir: input.skillsDir,
    });
    await loadSessions();
  };

  const handleDelete = async (id: string) => {
    try {
      await agentDeleteSession(id);
      removeSession(id);
    } catch (err) {
      console.error('Failed to delete agent:', err);
    }
  };

  const handlePin = async (id: string, pinned: boolean) => {
    try {
      await agentPinSession(id, pinned);
      await loadSessions();
    } catch (err) {
      console.error('Failed to pin agent:', err);
    }
  };

  const handleAddProject = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({ directory: true, multiple: false });
      if (selected && typeof selected === 'string') {
        const created = await addProject(selected);
        if (created) await handleSelectProject(created.id);
      }
    } catch (err) {
      console.error('Failed to add project:', err);
    }
  };

  const handleRemoveProject = async (id: string) => {
    await removeProject(id);
  };

  // Create a default-agent conversation for a project (Codex-style "默认对话").
  const createDefaultSession = async (projectId: string) => {
    try {
      const session = await agentCreateSession({
        title: '新对话',
        agentMode: 'chat',
        toolsEnabled: DEFAULT_TOOLS,
        projectId,
      });
      await loadSessions();
      setActiveSession(session.id);
    } catch (err) {
      console.error('Failed to create default session:', err);
    }
  };

  // Selecting a project switches the working context: open its most recent
  // conversation, or auto-create a default-agent conversation if it has none.
  const handleSelectProject = async (id: string | null) => {
    if (id === null) {
      switchProject(null);
      return;
    }
    switchProject(id);
    const projectSessions = useChatStore
      .getState()
      .sessions.filter((s) => s.project_id === id);
    if (projectSessions.length > 0) {
      const recent = [...projectSessions].sort((a, b) =>
        b.updated_at.localeCompare(a.updated_at)
      )[0];
      setActiveSession(recent.id);
    } else {
      await createDefaultSession(id);
    }
  };

  const handlePlaceholder = async (name: string) => {
    await alert(`${name}功能尚未开放`, '敬请期待');
  };

  const renderChatRow = (session: AgentSession) => (
    <div
      key={session.id}
      onClick={() => setActiveSession(session.id)}
      onContextMenu={(e) => handleContextMenu(e, session.id)}
      className={`group flex items-center gap-2 px-2 py-1.5 rounded-md cursor-pointer text-[13px] transition-colors ${
        activeSessionId === session.id
          ? 'bg-codex-hover text-codex-primary'
          : 'text-codex-secondary hover:bg-codex-hover hover:text-codex-primary'
      }`}
    >
      <AgentAvatar name={session.title} color={session.color} size={22} />
      <span className="flex-1 truncate">{session.title}</span>
      {session.is_pinned && <Pin size={12} className="shrink-0 text-codex-accent" />}
      {activeSessionId === session.id && (
        <span className="w-1.5 h-1.5 rounded-full bg-codex-accent shrink-0" />
      )}
      <ConfirmButton
        icon
        onConfirm={() => handleDelete(session.id)}
        confirmText="确认删除"
        aria-label="删除对话"
        className="opacity-0 group-hover:opacity-100"
      >
        <Trash2 size={12} />
      </ConfirmButton>
    </div>
  );

  return (
    <aside className="w-full bg-background flex flex-col h-full">
      {/* Top menu */}
      <nav className="px-2 pt-2 space-y-0.5">
        <MenuRow
          icon={<MessageSquarePlus size={16} />}
          label="新建智能体"
          onClick={() => setShowCreate(true)}
          primary
        />
        <MenuRow
          icon={<GitPullRequest size={16} />}
          label="拉取请求"
          onClick={() => handlePlaceholder('拉取请求')}
        />
        <MenuRow
          icon={<CalendarClock size={16} />}
          label="计划任务"
          onClick={() => handlePlaceholder('计划任务')}
        />
        <MenuRow
          icon={<Puzzle size={16} />}
          label="插件"
          onClick={() => handlePlaceholder('插件')}
        />
      </nav>

      {/* Projects */}
      <section className="mt-4">
        <div className="relative" ref={organizeRef}>
          <div className="flex items-center justify-between px-3 mb-1">
            <span className="text-[11px] font-semibold text-codex-muted uppercase tracking-wide">
              项目
            </span>
            <div className="flex items-center gap-0.5">
              <button
                onClick={() => setOrganizeOpen((v) => !v)}
                className="w-6 h-6 flex items-center justify-center rounded-md text-codex-muted hover:bg-codex-hover hover:text-codex-primary"
                title="组织侧边栏"
                aria-label="组织侧边栏"
              >
                <MoreHorizontal size={14} />
              </button>
              <button
                onClick={handleAddProject}
                className="w-6 h-6 flex items-center justify-center rounded-md text-codex-muted hover:bg-codex-hover hover:text-codex-primary"
                title="新建项目"
                aria-label="新建项目"
              >
                <FolderPlus size={14} />
              </button>
            </div>
          </div>

          {organizeOpen && (
            <div className="absolute left-2 right-2 top-6 z-40 rounded-lg border border-codex-border bg-codex-surface shadow-xl py-1.5">
              <div className="px-3 pb-1 text-[11px] font-medium text-codex-muted">
                组织侧边栏
              </div>
              <div className="px-3 pb-0.5 text-[10px] text-codex-muted">分组</div>
              <label className="flex items-center gap-2 px-3 py-1 text-[13px] text-codex-primary hover:bg-codex-hover cursor-pointer">
                <input
                  type="radio"
                  name="sidebarGroup"
                  checked={groupBy === 'project'}
                  onChange={() => setGroupBy('project')}
                  className="accent-codex-accent"
                />
                按项目
              </label>
              <label className="flex items-center gap-2 px-3 py-1 text-[13px] text-codex-primary hover:bg-codex-hover cursor-pointer">
                <input
                  type="radio"
                  name="sidebarGroup"
                  checked={groupBy === 'list'}
                  onChange={() => setGroupBy('list')}
                  className="accent-codex-accent"
                />
                在一个列表中
              </label>
              <div className="my-1 border-t border-codex-border" />
              <div className="px-3 pb-0.5 text-[10px] text-codex-muted">排序聊天依据</div>
              {(
                [
                  ['priority', '优先级'],
                  ['updated', '最新更新'],
                  ['manual', '手动排序'],
                ] as const
              ).map(([value, label]) => (
                <label
                  key={value}
                  className="flex items-center gap-2 px-3 py-1 text-[13px] text-codex-primary hover:bg-codex-hover cursor-pointer"
                >
                  <input
                    type="radio"
                    name="sidebarSort"
                    checked={sortBy === value}
                    onChange={() => setSortBy(value)}
                    className="accent-codex-accent"
                  />
                  {label}
                </label>
              ))}
            </div>
          )}
        </div>

        <div className="px-2 space-y-0.5">
          {projectsLoading ? (
            <div className="flex items-center gap-1.5 px-2 py-1 text-[12px] text-codex-muted">
              <Loader2 size={12} className="animate-spin" />
              加载中…
            </div>
          ) : projects.length === 0 ? (
            <div className="px-2 py-1 text-[12px] text-codex-muted">无项目</div>
          ) : (
            projects.map((p) => {
              const active = p.id === activeProjectId;
              return (
                <div
                  key={p.id}
                  onClick={() => handleSelectProject(active ? null : p.id)}
                  className={`group flex items-center gap-2 px-2 py-1.5 rounded-md cursor-pointer text-[13px] transition-colors ${
                    active
                      ? 'bg-codex-hover text-codex-primary'
                      : 'text-codex-secondary hover:bg-codex-hover hover:text-codex-primary'
                  }`}
                  title={p.path}
                >
                  <FolderOpen size={14} className="shrink-0 text-codex-muted" />
                  <span className="flex-1 truncate">{p.name}</span>
                  <span className="text-[11px] text-codex-muted shrink-0">
                    {projectChatCount(p.id)}
                  </span>
                  <ConfirmButton
                    icon
                    onConfirm={() => handleRemoveProject(p.id)}
                    confirmText="确认删除项目？（对话保留）"
                    aria-label="删除项目"
                    className="opacity-0 group-hover:opacity-100"
                  >
                    <Trash2 size={12} />
                  </ConfirmButton>
                </div>
              );
            })
          )}
        </div>
      </section>

      {/* Recents */}
      <section className="flex-1 min-h-0 flex flex-col mt-4">
        <div className="px-3 mb-1 text-[11px] font-semibold text-codex-muted uppercase tracking-wide">
          最近
        </div>
        <div className="flex-1 overflow-y-auto px-2 pb-3 space-y-0.5">
          {visibleSessions.length === 0 ? (
            <div className="px-2 py-1 text-[12px] text-codex-muted">暂无对话</div>
          ) : grouped ? (
            [...grouped.entries()].map(([pid, list]) => (
              <div key={pid}>
                <div className="px-2 pt-2 pb-0.5 text-[11px] text-codex-muted truncate">
                  {projects.find((p) => p.id === pid)?.name || '未分组'}
                </div>
                {list.map((s) => renderChatRow(s))}
              </div>
            ))
          ) : (
            visibleSessions.map((s) => renderChatRow(s))
          )}
        </div>
      </section>

      {/* Agent context menu */}
      {contextMenu.visible && (
        <div
          ref={menuRef}
          className="fixed z-[3000] min-w-[150px] rounded-lg border border-codex-border bg-codex-surface shadow-xl py-1"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          {(() => {
            const agent = sessions.find((s) => s.id === contextMenu.agentId);
            if (!agent) return null;
            return (
              <>
                <button
                  onClick={() => {
                    handlePin(agent.id, !agent.is_pinned);
                    setContextMenu({ visible: false, x: 0, y: 0, agentId: '' });
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-[13px] text-codex-secondary hover:bg-codex-hover hover:text-codex-primary"
                >
                  <Pin size={14} />
                  {agent.is_pinned ? '取消置顶' : '置顶'}
                </button>
                <button
                  onClick={() => {
                    setConfigAgent(agent);
                    setContextMenu({ visible: false, x: 0, y: 0, agentId: '' });
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-[13px] text-codex-secondary hover:bg-codex-hover hover:text-codex-primary"
                >
                  <Settings size={14} />
                  个性设置
                </button>
                <button
                  onClick={() => {
                    handleDelete(agent.id);
                    setContextMenu({ visible: false, x: 0, y: 0, agentId: '' });
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-[13px] text-codex-danger hover:bg-codex-hover"
                >
                  <Trash2 size={14} />
                  删除
                </button>
              </>
            );
          })()}
        </div>
      )}

      {showCreate && (
        <AgentCreateDialog
          onClose={() => setShowCreate(false)}
          onCreate={handleNewAgent}
          projectPath={activeProject?.path}
        />
      )}

      {configAgent && (
        <AgentConfigPanel agent={configAgent} onClose={() => setConfigAgent(null)} onSave={handleUpdateAgent} />
      )}
    </aside>
  );
}
