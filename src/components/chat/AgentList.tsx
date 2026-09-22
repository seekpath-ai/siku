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
  Plus,
  Trash2,
  Settings,
  Loader2,
  Archive,
  ArchiveRestore,
  Pencil,
  ChevronDown,
  ChevronRight,
} from 'lucide-react';
import { useChatStore } from '@/stores/chatStore';
import { useProjectStore } from '@/stores/projectStore';
import {
  agentListSessions,
  agentCreateSession,
  agentUpdateSession,
  agentDeleteSession,
  agentPinSession,
  agentArchiveSession,
  fileBrowserRevealInSystem,
} from '@/lib/tauri';
import { useDialog } from '@/hooks/useDialog';
import { ConfirmButton } from '@/components/ui/ConfirmButton';
import { AgentAvatar } from './AgentAvatar';
import { AgentCreateDialog } from './AgentCreateDialog';
import { AgentConfigPanel } from './AgentConfigPanel';
import { NewProjectDialog } from './NewProjectDialog';
import { CreatePrDialog } from './CreatePrDialog';
import { TaskCenterDialog } from './TaskCenterDialog';
import { PluginsDialog } from './PluginsDialog';
import { DEFAULT_TOOLS } from '@/lib/agent-tools';
import type { AgentSession, LlmConfigBlock, ApprovalConfig } from '@/lib/types';

interface AgentCreateInput {
  title: string;
  systemPrompt?: string;
  tools: string[];
  /** Project binding; undefined = project-less (create), null = unbind (update). */
  projectId?: string | null;
  workingDir: string | null;
  visionProviderId: string | null;
  webProxy: string | null;
  llmProviderIds: string[];
  llmModels: LlmConfigBlock[];
  approvalConfig: ApprovalConfig;
  maxLoops: number;
  /** Per-round output cap; undefined = follow the model config. */
  maxTokens?: number;
  /** Conversation context truncation budget. */
  contextBudget: number;
  maxMemoryRounds: number;
  memoryDir?: string;
  skillsDir?: string;
  /** Skills mounted on this session; undefined = leave untouched. */
  selectedSkills?: string[];
}

interface ContextMenuState {
  visible: boolean;
  x: number;
  y: number;
  agentId: string;
}

interface ProjectMenuState {
  visible: boolean;
  x: number;
  y: number;
  projectId: string;
}

function MenuRow({
  icon,
  label,
  onClick,
  primary,
  shortcut,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
  primary?: boolean;
  shortcut?: string;
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
      {shortcut && (
        <span className="ml-auto text-[10px] text-codex-muted/70">{shortcut}</span>
      )}
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
    sortBy,
    load,
    addProject,
    removeProject,
    renameProject,
    archiveProject,
    switchProject,
    setSortBy,
  } = useProjectStore();
  const { alert, confirm, prompt } = useDialog();

  const [showCreate, setShowCreate] = useState(false);
  const [newProjectOpen, setNewProjectOpen] = useState(false);
  /** Project the create-PR dialog is open for. */
  const [prProject, setPrProject] = useState<{ id: string; name: string } | null>(null);
  const [showTaskCenter, setShowTaskCenter] = useState(false);
  const [showPlugins, setShowPlugins] = useState(false);
  const [configAgent, setConfigAgent] = useState<AgentSession | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenuState>({
    visible: false,
    x: 0,
    y: 0,
    agentId: '',
  });
  const [projectMenu, setProjectMenu] = useState<ProjectMenuState>({
    visible: false,
    x: 0,
    y: 0,
    projectId: '',
  });
  const [organizeOpen, setOrganizeOpen] = useState(false);
  /** Collapsed-state toggles for the archived sections. */
  const [showArchivedChats, setShowArchivedChats] = useState(false);
  const [showArchivedProjects, setShowArchivedProjects] = useState(false);
  /** Projects expanded to show their sessions inline (Codex workspace style). */
  const [expandedProjects, setExpandedProjects] = useState<Set<string>>(new Set());
  const menuRef = useRef<HTMLDivElement>(null);
  const projectMenuRef = useRef<HTMLDivElement>(null);
  const organizeRef = useRef<HTMLDivElement>(null);

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

  // Ctrl+Shift+N (AppShell) opens the new-agent dialog.
  useEffect(() => {
    const open = () => setShowCreate(true);
    window.addEventListener('siku:new-agent', open);
    return () => window.removeEventListener('siku:new-agent', open);
  }, []);

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

  // Close the project context menu on outside click.
  useEffect(() => {
    if (!projectMenu.visible) return;
    const close = (e: MouseEvent) => {
      if (projectMenuRef.current?.contains(e.target as Node)) return;
      setProjectMenu({ visible: false, x: 0, y: 0, projectId: '' });
    };
    const timer = setTimeout(() => document.addEventListener('mousedown', close, true), 0);
    return () => {
      clearTimeout(timer);
      document.removeEventListener('mousedown', close, true);
    };
  }, [projectMenu.visible]);

  // Archived projects/sessions hide from the default lists; they live in the
  // collapsed "已归档" sections at the bottom.
  const visibleProjects = useMemo(() => projects.filter((p) => !p.archived), [projects]);
  const archivedProjects = useMemo(() => projects.filter((p) => p.archived), [projects]);
  const activeSessions = useMemo(() => sessions.filter((s) => !s.archived), [sessions]);
  const archivedSessions = useMemo(() => sessions.filter((s) => s.archived), [sessions]);

  const sortSessions = useCallback(
    (list: AgentSession[]) => {
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
    },
    [sortBy]
  );

  // Non-archived sessions bucketed by project ('' = unbound), each bucket
  // sorted. Project buckets render nested under their project row; the ''
  // bucket is the 对话 section.
  const sessionsByProject = useMemo(() => {
    const map = new Map<string, AgentSession[]>();
    for (const s of activeSessions) {
      const key = s.project_id ?? '';
      const arr = map.get(key) ?? [];
      arr.push(s);
      map.set(key, arr);
    }
    for (const [key, list] of map) map.set(key, sortSessions(list));
    return map;
  }, [activeSessions, sortSessions]);

  const toggleProjectExpanded = (id: string) => {
    setExpandedProjects((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  // On first load, expand the project containing the active session so its
  // row isn't hidden inside a collapsed project.
  const autoExpandedRef = useRef(false);
  useEffect(() => {
    if (autoExpandedRef.current || sessions.length === 0) return;
    autoExpandedRef.current = true;
    const active = sessions.find((s) => s.id === useChatStore.getState().activeSessionId);
    if (active?.project_id) setExpandedProjects(new Set([active.project_id]));
  }, [sessions]);

  const handleContextMenu = (e: React.MouseEvent, agentId: string) => {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ visible: true, x: e.clientX, y: e.clientY, agentId });
  };

  const handleProjectContextMenu = (e: React.MouseEvent, projectId: string) => {
    e.preventDefault();
    e.stopPropagation();
    setProjectMenu({ visible: true, x: e.clientX, y: e.clientY, projectId });
  };

  const handleArchiveSession = async (id: string, archived: boolean) => {
    try {
      await agentArchiveSession(id, archived);
      setSessions(sessions.map((s) => (s.id === id ? { ...s, archived } : s)));
    } catch (err) {
      console.error('Failed to archive session:', err);
    }
  };

  const handleRenameProject = async (id: string) => {
    const project = projects.find((p) => p.id === id);
    if (!project) return;
    const name = await prompt('输入新的项目名称', {
      title: '重命名项目',
      defaultValue: project.name,
    });
    if (!name || !name.trim() || name.trim() === project.name) return;
    try {
      await renameProject(id, name.trim());
    } catch (err) {
      await alert(`重命名失败：${err}`, '重命名项目');
    }
  };

  const handleDeleteProject = async (id: string) => {
    const project = projects.find((p) => p.id === id);
    const ok = await confirm(
      `删除项目「${project?.name ?? ''}」？项目下的对话会保留（变为无项目）。`,
      '删除项目'
    );
    if (ok) await removeProject(id);
  };

  const handleNewAgent = async (input: AgentCreateInput) => {
    const session = await agentCreateSession({
      title: input.title,
      agentMode: 'chat',
      toolsEnabled: input.tools,
      systemPrompt: input.systemPrompt,
      // New agents start project-less on purpose (a project can be bound
      // later from the session settings); no forced fallback to the sidebar's
      // active project.
      projectId: input.projectId,
      workingDir: input.workingDir,
      visionProviderId: input.visionProviderId,
      webProxy: input.webProxy,
      llmProviderIds: input.llmProviderIds,
      llmModels: input.llmModels,
      approvalConfig: input.approvalConfig,
      maxLoops: input.maxLoops,
      maxTokens: input.maxTokens,
      contextBudget: input.contextBudget,
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
      projectId: input.projectId ?? null,
      workingDir: input.workingDir,
      visionProviderId: input.visionProviderId,
      webProxy: input.webProxy,
      llmProviderIds: input.llmProviderIds,
      llmModels: input.llmModels,
      approvalConfig: input.approvalConfig,
      maxLoops: input.maxLoops,
      maxTokens: input.maxTokens,
      contextBudget: input.contextBudget,
      maxMemoryRounds: input.maxMemoryRounds,
      memoryDir: input.memoryDir,
      skillsDir: input.skillsDir,
      selectedSkills: input.selectedSkills,
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

  const handleAddProject = () => setNewProjectOpen(true);

  const handleCreateProject = async (path: string, name?: string, gitInit?: boolean) => {
    const created = await addProject(path, name, gitInit);
    if (created) {
      switchProject(created.id);
      setExpandedProjects((prev) => new Set(prev).add(created.id));
    }
    return created?.id ?? null;
  };

  // Create a default-agent conversation inside a project (the row's + button).
  const createDefaultSession = async (projectId: string) => {
    try {
      const session = await agentCreateSession({
        title: '新对话',
        agentMode: 'chat',
        toolsEnabled: DEFAULT_TOOLS,
        projectId,
      });
      setExpandedProjects((prev) => new Set(prev).add(projectId));
      await loadSessions();
      setActiveSession(session.id);
    } catch (err) {
      console.error('Failed to create default session:', err);
    }
  };

  // Clicking a project row selects it as the working context (used by the
  // hero input's binding and the 拉取请求 menu); the chevron expands its
  // session list inline.
  const handleSelectProject = (id: string) => {
    switchProject(activeProjectId === id ? null : id);
  };

  // 拉取请求: opens the create-PR dialog for the selected project (the dialog
  // itself reports non-git / no-origin / unsupported-host states).
  const handlePullRequest = async () => {
    if (!activeProjectId) {
      await alert('请先在下方项目列表选择一个项目', '拉取请求');
      return;
    }
    const name = projects.find((p) => p.id === activeProjectId)?.name ?? '项目';
    setPrProject({ id: activeProjectId, name });
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
          shortcut="Ctrl+Shift+N"
          onClick={() => setShowCreate(true)}
          primary
        />
        <MenuRow
          icon={<GitPullRequest size={16} />}
          label="拉取请求"
          onClick={() => void handlePullRequest()}
        />
        <MenuRow
          icon={<CalendarClock size={16} />}
          label="计划任务"
          onClick={() => {
            // Scheduled prompts are per-session; without an active session
            // the cron tab has nothing to bind to.
            if (!activeSessionId) {
              void alert('请先在左侧选择一个会话', '计划任务');
              return;
            }
            setShowTaskCenter(true);
          }}
        />
        <MenuRow
          icon={<Puzzle size={16} />}
          label="插件"
          onClick={() => setShowPlugins(true)}
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
          ) : visibleProjects.length === 0 && archivedProjects.length === 0 ? (
            <div className="px-2 py-1 text-[12px] text-codex-muted">无项目</div>
          ) : (
            <>
              {visibleProjects.map((p) => {
                const active = p.id === activeProjectId;
                const expanded = expandedProjects.has(p.id);
                const projectSessions = sessionsByProject.get(p.id) ?? [];
                return (
                  <div key={p.id}>
                    <div
                      onClick={() => handleSelectProject(p.id)}
                      onContextMenu={(e) => handleProjectContextMenu(e, p.id)}
                      className={`group flex items-center gap-1.5 px-2 py-1.5 rounded-md cursor-pointer text-[13px] transition-colors ${
                        active
                          ? 'bg-codex-hover text-codex-primary'
                          : 'text-codex-secondary hover:bg-codex-hover hover:text-codex-primary'
                      }`}
                      title={p.path}
                    >
                      {/* Chevron: appears on hover, stays visible while expanded. */}
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          toggleProjectExpanded(p.id);
                        }}
                        aria-label={expanded ? '收起' : '展开'}
                        className={`shrink-0 -ml-1 p-0.5 rounded text-codex-muted hover:text-codex-primary transition-opacity ${
                          expanded ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'
                        }`}
                      >
                        {expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                      </button>
                      <FolderOpen size={14} className="shrink-0 text-codex-muted" />
                      <span className="flex-1 truncate">{p.name}</span>
                      <span className="text-[11px] text-codex-muted shrink-0 group-hover:hidden">
                        {projectSessions.length}
                      </span>
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          void createDefaultSession(p.id);
                        }}
                        title="在该项目下新建对话"
                        aria-label="在该项目下新建对话"
                        className="hidden group-hover:block shrink-0 p-0.5 rounded text-codex-muted hover:text-codex-primary hover:bg-codex-bg"
                      >
                        <Plus size={13} />
                      </button>
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          fileBrowserRevealInSystem(p.path).catch((err) =>
                            console.error('reveal project dir:', err)
                          );
                        }}
                        title="打开目录位置"
                        aria-label="打开目录位置"
                        className="opacity-0 group-hover:opacity-100 p-0.5 rounded text-codex-muted hover:text-codex-primary hover:bg-codex-bg transition-opacity"
                      >
                        <FolderOpen size={12} />
                      </button>
                    </div>
                    {expanded && (
                      <div className="ml-5 space-y-0.5">
                        {projectSessions.length === 0 ? (
                          <div className="px-2 py-1 text-[12px] text-codex-muted">暂无对话</div>
                        ) : (
                          projectSessions.map((s) => renderChatRow(s))
                        )}
                      </div>
                    )}
                  </div>
                );
              })}
              {archivedProjects.length > 0 && (
                <div>
                  <button
                    onClick={() => setShowArchivedProjects((v) => !v)}
                    className="w-full flex items-center gap-1.5 px-2 py-1 text-[12px] text-codex-muted hover:text-codex-primary"
                  >
                    {showArchivedProjects ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                    已归档（{archivedProjects.length}）
                  </button>
                  {showArchivedProjects &&
                    archivedProjects.map((p) => (
                      <div
                        key={p.id}
                        onContextMenu={(e) => handleProjectContextMenu(e, p.id)}
                        className="flex items-center gap-2 px-2 py-1.5 rounded-md text-[13px] text-codex-muted hover:bg-codex-hover cursor-default"
                        title={p.path}
                      >
                        <Archive size={14} className="shrink-0" />
                        <span className="flex-1 truncate">{p.name}</span>
                      </div>
                    ))}
                </div>
              )}
            </>
          )}
        </div>
      </section>

      {/* Unbound conversations */}
      <section className="flex-1 min-h-0 flex flex-col mt-4">
        <div className="px-3 mb-1 text-[11px] font-semibold text-codex-muted uppercase tracking-wide">
          对话
        </div>
        <div className="flex-1 overflow-y-auto px-2 pb-3 space-y-0.5">
          {(sessionsByProject.get('') ?? []).length === 0 ? (
            <div className="px-2 py-1 text-[12px] text-codex-muted">暂无对话</div>
          ) : (
            (sessionsByProject.get('') ?? []).map((s) => renderChatRow(s))
          )}
          {archivedSessions.length > 0 && (
            <div className="pt-1">
              <button
                onClick={() => setShowArchivedChats((v) => !v)}
                className="w-full flex items-center gap-1.5 px-2 py-1 text-[12px] text-codex-muted hover:text-codex-primary"
              >
                {showArchivedChats ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                已归档（{archivedSessions.length}）
              </button>
              {showArchivedChats && archivedSessions.map((s) => renderChatRow(s))}
            </div>
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
                    handleArchiveSession(agent.id, !agent.archived);
                    setContextMenu({ visible: false, x: 0, y: 0, agentId: '' });
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-[13px] text-codex-secondary hover:bg-codex-hover hover:text-codex-primary"
                >
                  {agent.archived ? <ArchiveRestore size={14} /> : <Archive size={14} />}
                  {agent.archived ? '取消归档' : '归档'}
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

      {/* Project context menu */}
      {projectMenu.visible && (
        <div
          ref={projectMenuRef}
          className="fixed z-[3000] min-w-[170px] rounded-lg border border-codex-border bg-codex-surface shadow-xl py-1"
          style={{ left: projectMenu.x, top: projectMenu.y }}
        >
          {(() => {
            const project = projects.find((p) => p.id === projectMenu.projectId);
            if (!project) return null;
            const closeMenu = () => setProjectMenu({ visible: false, x: 0, y: 0, projectId: '' });
            return (
              <>
                <button
                  onClick={() => {
                    fileBrowserRevealInSystem(project.path).catch((err) =>
                      console.error('reveal project dir:', err)
                    );
                    closeMenu();
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-[13px] text-codex-secondary hover:bg-codex-hover hover:text-codex-primary"
                >
                  <FolderOpen size={14} />
                  打开目录位置
                </button>
                {!project.archived && (
                  <>
                    <button
                      onClick={() => {
                        void handleRenameProject(project.id);
                        closeMenu();
                      }}
                      className="w-full flex items-center gap-2 px-3 py-2 text-[13px] text-codex-secondary hover:bg-codex-hover hover:text-codex-primary"
                    >
                      <Pencil size={14} />
                      重命名
                    </button>
                    <button
                      onClick={() => {
                        setPrProject({ id: project.id, name: project.name });
                        closeMenu();
                      }}
                      className="w-full flex items-center gap-2 px-3 py-2 text-[13px] text-codex-secondary hover:bg-codex-hover hover:text-codex-primary"
                    >
                      <GitPullRequest size={14} />
                      创建拉取请求
                    </button>
                  </>
                )}
                <button
                  onClick={() => {
                    archiveProject(project.id, !project.archived).catch((err) =>
                      console.error('archive project:', err)
                    );
                    closeMenu();
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-[13px] text-codex-secondary hover:bg-codex-hover hover:text-codex-primary"
                >
                  {project.archived ? <ArchiveRestore size={14} /> : <Archive size={14} />}
                  {project.archived ? '取消归档' : '归档'}
                </button>
                <button
                  onClick={() => {
                    void handleDeleteProject(project.id);
                    closeMenu();
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
          // No projectPath: new agents are project-less; the sandbox's
          // "项目目录" option stays disabled until a project is bound.
        />
      )}

      {configAgent && (
        <AgentConfigPanel agent={configAgent} onClose={() => setConfigAgent(null)} onSave={handleUpdateAgent} />
      )}

      {showTaskCenter && activeSessionId && (
        <TaskCenterDialog sessionId={activeSessionId} onClose={() => setShowTaskCenter(false)} />
      )}

      {showPlugins && <PluginsDialog onClose={() => setShowPlugins(false)} />}

      <NewProjectDialog
        open={newProjectOpen}
        onClose={() => setNewProjectOpen(false)}
        onCreate={handleCreateProject}
      />

      {prProject && (
        <CreatePrDialog
          projectId={prProject.id}
          projectName={prProject.name}
          onClose={() => setPrProject(null)}
        />
      )}
    </aside>
  );
}
