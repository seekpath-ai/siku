import { useCallback, useEffect, useState } from 'react';
import {
  Puzzle, X, RefreshCw, Search, FolderOpen, FolderInput, FileArchive, Trash2, Loader2,
  ShieldCheck,
} from 'lucide-react';
import {
  skillsList, skillsGet, skillsImportFolder, skillsImportZip, skillsDelete,
  skillsOpenDirectory, agentSetSessionSkills, skillsReviewStart, skillsReviewCollect,
  skillsEnvInstallMany, skillsEnvPython, skillsSetDependencyIgnored,
  type SkillInfo, type SkillDetail, type SkillInstallResult,
} from '@/lib/tauri';
import type { AgentStreamEvent } from '@/lib/types';
import { useChatStore } from '@/stores/chatStore';
import { useDialog } from '@/hooks/useDialog';
import { notify } from '@/lib/notify';
import { Modal } from '@/components/ui/Modal';
import { SkillIcon } from './SkillIcon';

/** Small module-level cache so reopening the dialog is instant; every mutating
 *  action invalidates it explicitly. */
let skillsListCache: SkillInfo[] | null = null;
const skillDetailCache = new Map<string, SkillDetail>();

/** Plugins (skills) dialog — same modal shell as the task center.
 *
 *  Skills are per-session plugins: nothing here is visible to the LLM until
 *  the skill is mounted on a session ("挂载到当前会话" or the session config
 *  panel). This dialog manages the library: import (folder / zip), preview,
 *  mount/unmount on the active session, delete. */
export function PluginsDialog({ onClose }: { onClose: () => void }) {
  const [skills, setSkills] = useState<SkillInfo[]>(() => skillsListCache ?? []);
  const [loading, setLoading] = useState(() => skillsListCache === null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const [sourceTab, setSourceTab] = useState<'all' | 'builtin' | 'user'>('all');
  const [detail, setDetail] = useState<SkillDetail | null>(null);
  /** 正在加载详情的技能名（null = 无）：只让被点击的卡片显示加载圈。 */
  const [detailLoading, setDetailLoading] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** pip 依赖一键安装进行中（详情弹窗内）。 */
  const [installingDeps, setInstallingDeps] = useState(false);
  const [installingCard, setInstallingCard] = useState<string | null>(null);
  const [depSelection, setDepSelection] = useState<Set<string>>(new Set());
  const [depResults, setDepResults] = useState<{ skill: string; results: SkillInstallResult[] } | null>(null);
  /** 隔离环境安装的当前进度行（skills:env-install 事件推送）。 */
  const [installProgress, setInstallProgress] = useState<string | null>(null);
  /** skill name → reviewer session id for in-flight AI reviews. */
  const [reviewing, setReviewing] = useState<Map<string, string>>(new Map());
  const { sessions, activeSessionId, setSessions } = useChatStore();
  const { confirm } = useDialog();

  const activeSession = sessions.find((s) => s.id === activeSessionId) ?? null;
  const mounted = new Set(activeSession?.selected_skills ?? []);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const list = await skillsList();
      skillsListCache = list;
      setSkills(list);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  /** Start an AI review: deterministic static scan + a no-tool 安全审查
   * domain session. The review runs in a detached pet-chat window so the
   * user can watch the process. */
  const startReview = async (name: string) => {
    setError(null);
    try {
      const { sessionId } = await skillsReviewStart(name);
      setReviewing((m) => new Map(m).set(name, sessionId));
      const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
      new WebviewWindow(`pet-chat-${Date.now()}`, {
        url: `index.html?petSession=${sessionId}`,
        title: '技能安全审查 - 思库',
        width: 440,
        height: 680,
        minWidth: 360,
        minHeight: 480,
        center: true,
        decorations: false,
        transparent: true,
        shadow: false,
      });
    } catch (err) {
      setError(String(err));
    }
  };

  // When a review turn finishes, collect + persist the verdict and refresh
  // the badges.
  useEffect(() => {
    if (reviewing.size === 0) return;
    let un: (() => void) | undefined;
    import('@tauri-apps/api/event')
      .then(({ listen }) =>
        listen<AgentStreamEvent>('agent:event', (e) => {
          const ev = e.payload;
          if (!['done', 'cancelled', 'error'].includes(ev.type)) return;
          const entry = [...reviewing.entries()].find(([, sid]) => sid === ev.session_id);
          if (!entry) return;
          const [name] = entry;
          (async () => {
            if (ev.type === 'done') {
              try {
                await skillsReviewCollect(ev.session_id);
                skillDetailCache.clear();
              } catch (err) {
                setError(String(err));
              }
            }
            setReviewing((m) => {
              const n = new Map(m);
              n.delete(name);
              return n;
            });
            await load();
            if (detail?.name === name) {
              const next = await skillsGet(name);
              skillDetailCache.set(name, next);
              setDetail(next);
            }
          })();
        })
      )
      .then((u) => {
        un = u;
      });
    return () => un?.();
  }, [detail, reviewing, load]);

  const openDetail = async (name: string) => {
    const cached = skillDetailCache.get(name);
    if (cached) {
      setDetail(cached);
      setDepSelection(defaultSelection(cached));
      setDetailLoading(null);
      return;
    }
    setDetailLoading(name);
    try {
      const next = await skillsGet(name);
      skillDetailCache.set(name, next);
      setDetail(next);
      setDepSelection(defaultSelection(next));
    } catch (err) {
      setError(String(err));
    } finally {
      setDetailLoading(null);
    }
  };

  const importFolder = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({ directory: true, title: '选择技能文件夹（内含 SKILL.md）' });
      if (typeof selected !== 'string') return;
      setBusy(true);
      await skillsImportFolder(selected);
      skillsListCache = null;
      skillDetailCache.clear();
      await load();
      notify('技能已导入；用户技能需要先完成 AI 安全审查才能挂载', 'warning');
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const importZip = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({
        filters: [{ name: 'Zip 压缩包', extensions: ['zip'] }],
        title: '选择技能压缩包',
      });
      if (typeof selected !== 'string') return;
      setBusy(true);
      await skillsImportZip(selected);
      skillsListCache = null;
      skillDetailCache.clear();
      await load();
      notify('技能已导入；用户技能需要先完成 AI 安全审查才能挂载', 'warning');
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  /** Mount/unmount a skill on the ACTIVE session (per-session plugins). */
  const toggleMount = async (name: string) => {
    if (!activeSession) {
      notify('请先在左侧选择一个会话，再将技能挂载到该会话', 'warning');
      return;
    }
    const current = activeSession.selected_skills ?? [];
    const isAdding = !current.includes(name);
    if (isAdding) {
      const skill = skills.find((s) => s.name === name);
      const state = detail?.name === name ? detail.reviewState : skill?.reviewState;
      const source = detail?.name === name ? detail.source : skill?.source;
      if (source !== 'builtin') {
        if (state === 'unreviewed') {
          notify('用户技能需要先完成 AI 安全审查，审查后才能挂载', 'warning');
          return;
        }
        if (state === 'stale') {
          notify('技能在审查后已变更，请重新审查后再挂载', 'warning');
          return;
        }
        if (state === 'risk') {
          notify('该技能审查结果为风险，禁止挂载', 'error');
          return;
        }
        if (state === 'warn') {
          const ok = await confirm(
            '该技能审查结果为「注意」，可能包含需要人工确认的权限或模式。仍要挂载吗？',
            { title: '挂载审查注意的技能' },
          );
          if (!ok) return;
        }
      }
    }
    if (isAdding) {
      const deps =
        detail?.name === name
          ? detail.dependencies
          : skills.find((s) => s.name === name)?.dependencies ?? [];
      const missing = deps.filter((d) => !d.available && !d.optional && !d.ignored);
      if (missing.length > 0) {
        const ok = await confirm(
          `缺少运行依赖：${missing.map((d) => d.name).join('、')}。挂载后相关功能可能不可用，仍要挂载吗？`,
          { title: '依赖未齐' },
        );
        if (!ok) return;
      }
    }
    const next = current.includes(name)
      ? current.filter((n) => n !== name)
      : [...current, name];
    try {
      await agentSetSessionSkills(activeSession.id, next);
      setSessions(
        sessions.map((s) => (s.id === activeSession.id ? { ...s, selected_skills: next } : s))
      );
    } catch (err) {
      setError(String(err));
    }
  };

  const removeSkill = async (name: string) => {
    const ok = await confirm(
      `删除技能「${name}」？其目录将被移除；挂载了它的会话下一轮起不再看到该技能。`,
      { title: '删除技能', danger: true }
    );
    if (!ok) return;
    try {
      await skillsDelete(name);
      skillsListCache = null;
      skillDetailCache.clear();
      setDetail(null);
      // Unmount from any session that referenced it (local state only; the
      // stale name in the DB is harmless — the registry just skips it).
      setSessions(
        sessions.map((s) =>
          s.selected_skills?.includes(name)
            ? { ...s, selected_skills: s.selected_skills.filter((n) => n !== name) }
            : s
        )
      );
      await load();
    } catch (err) {
      setError(String(err));
    }
  };

  const filtered = skills.filter(
    (s) =>
      (sourceTab === 'all' ||
        (sourceTab === 'builtin' ? s.source === 'builtin' : s.source !== 'builtin')) &&
      (!query.trim() ||
        s.name.toLowerCase().includes(query.trim().toLowerCase()) ||
        s.description.toLowerCase().includes(query.trim().toLowerCase()))
  );
  const builtinSkills = filtered.filter((s) => s.source === 'builtin');
  const userSkills = filtered.filter((s) => s.source !== 'builtin');

  /** Dependencies selected by default when opening the detail page. Declared
   *  packages are opt-in checked; review-inferred packages are NOT installed
   *  automatically (they are heuristics and may be false positives). */
  const defaultSelection = (d: SkillDetail) =>
    new Set(
      d.dependencies
        .filter(
          (dep) =>
            dep.kind === 'pip' &&
            !dep.available &&
            !dep.ignored &&
            d.dependencySource === 'declared'
        )
        .map((dep) => dep.name)
    );

  const runInstall = async (skillName: string, packages: string[]) => {
    if (packages.length === 0) return;
    setInstallProgress(null);
    const { listen } = await import('@tauri-apps/api/event');
    const unlisten = await listen<{ message: string }>('skills:env-install', (e) => {
      setInstallProgress(e.payload.message);
    });
    try {
      const results = await skillsEnvInstallMany(packages);
      setDepResults({ skill: skillName, results });
      skillDetailCache.clear();
      await load();
      if (detail?.name === skillName) {
        const next = await skillsGet(skillName);
        skillDetailCache.set(skillName, next);
        setDetail(next);
        setDepSelection(defaultSelection(next));
      }
    } catch (err) {
      setError(String(err));
    } finally {
      unlisten();
      setInstallProgress(null);
      skillsListCache = null;
    }
  };

  /** Declared deps may be installed in one click; review-inferred deps must be
   *  inspected/selected in the detail dialog. */
  const installForCard = async (skill: SkillInfo) => {
    if (skill.dependencySource === 'review') {
      await openDetail(skill.name);
      return;
    }
    const missing = skill.dependencies
      .filter((d) => d.kind === 'pip' && !d.available && !d.ignored)
      .map((d) => d.name);
    if (missing.length === 0 || installingCard || installingDeps) return;
    setInstallingCard(skill.name);
    try {
      await runInstall(skill.name, missing);
    } finally {
      setInstallingCard(null);
    }
  };

  const installSelectedDeps = async (d: SkillDetail) => {
    if (installingDeps) return;
    const packages = detailDependencies(d).filter(
      (dep) => dep.kind === 'pip' && !dep.available && !dep.ignored && depSelection.has(dep.name)
    );
    if (packages.length === 0) return;
    setInstallingDeps(true);
    try {
      await runInstall(d.name, packages.map((dep) => dep.name));
    } finally {
      setInstallingDeps(false);
    }
  };

  const detailDependencies = (d: SkillDetail) => d.dependencies;

  const retryFailed = async () => {
    if (!depResults) return;
    const failed = depResults.results.filter((r) => !r.ok).map((r) => r.name);
    if (failed.length === 0) return;
    await runInstall(depResults.skill, failed);
  };

  const ignoreDependency = async (skillName: string, dependency: string) => {
    try {
      await skillsSetDependencyIgnored(skillName, dependency, true);
      skillDetailCache.clear();
      setDepResults((prev) =>
        prev ? { ...prev, results: prev.results.filter((r) => r.name !== dependency) } : prev
      );
      await load();
      if (detail?.name === skillName) {
        const next = await skillsGet(skillName);
        skillDetailCache.set(skillName, next);
        setDetail(next);
        setDepSelection(defaultSelection(next));
      }
    } catch (err) {
      setError(String(err));
    }
  };

  const restoreDependency = async (skillName: string, dependency: string) => {
    try {
      await skillsSetDependencyIgnored(skillName, dependency, false);
      skillDetailCache.clear();
      await load();
      if (detail?.name === skillName) {
        const next = await skillsGet(skillName);
        skillDetailCache.set(skillName, next);
        setDetail(next);
        setDepSelection(defaultSelection(next));
      }
    } catch (err) {
      setError(String(err));
    }
  };

  const copyFailedInstallCommand = async () => {
    if (!depResults) return;
    const failed = depResults.results.filter((r) => !r.ok).map((r) => r.name);
    if (failed.length === 0) return;
    const python = await skillsEnvPython().catch(() => null);
    const command = `${python ? `"${python}"` : 'python'} -m pip install ${failed.join(' ')}`;
    try {
      await navigator.clipboard.writeText(command);
      notify('安装命令已复制，可在终端手动执行', 'success');
    } catch {
      setError(`无法复制命令，请手动执行：${command}`);
    }
  };

  const toggleDepSelection = (name: string) => {
    setDepSelection((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  /** Review state badge: 绿=通过 / 黄=注意 / 红=风险；stale（审查后内容变更）按待复审显示。 */
  const reviewBadge = (s: SkillInfo) => {
    const r = s.review;
    if (!r) return null;
    if (r.stale) return null; // top-right status already shows 待复审
    const styles: Record<string, [string, string]> = {
      pass: ['审查通过', 'border-emerald-400/30 text-emerald-400 bg-emerald-400/10'],
      warn: ['审查注意', 'border-amber-400/30 text-amber-400 bg-amber-400/10'],
      risk: ['存在风险', 'border-red-400/30 text-red-400 bg-red-400/10'],
    };
    const [label, cls] = styles[r.verdict] ?? styles.warn;
    return (
      <span className={`shrink-0 text-[10px] px-1.5 py-px rounded-full border ${cls}`}>
        {label}
      </span>
    );
  };

  const detailMountBlocked =
    !!detail &&
    detail.source !== 'builtin' &&
    (detail.reviewState === 'unreviewed' ||
      detail.reviewState === 'stale' ||
      detail.reviewState === 'risk');
  const detailMountBlockLabel =
    detail?.reviewState === 'stale'
      ? '需复审'
      : detail?.reviewState === 'risk'
        ? '禁止挂载'
        : '需先审查';

  return (
    <Modal
      onClose={onClose}
      backdropClassName="bg-black/50 backdrop-blur-sm"
    >
      <div className="relative w-[680px] max-w-[94vw] h-[580px] max-h-[88vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        {/* Header */}
        <div className="flex items-center gap-2 px-4 py-3 border-b border-surface-hover">
          <Puzzle size={16} className="text-primary" />
          <span className="text-sm font-medium text-text-primary">插件</span>
          <button
            onClick={load}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            title="刷新"
          >
            <RefreshCw size={13} className={loading ? 'animate-spin' : ''} />
          </button>
          <div className="ml-auto flex items-center gap-1.5">
            <button
              onClick={importFolder}
              disabled={busy}
              className="flex items-center gap-1 px-2 py-1 rounded-lg border border-surface-hover text-[11px] text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors disabled:opacity-50"
            >
              <FolderInput size={12} />
              文件夹导入
            </button>
            <button
              onClick={importZip}
              disabled={busy}
              className="flex items-center gap-1 px-2 py-1 rounded-lg border border-surface-hover text-[11px] text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors disabled:opacity-50"
            >
              <FileArchive size={12} />
              压缩包导入
            </button>
            <button
              onClick={() => skillsOpenDirectory().catch((e) => setError(String(e)))}
              className="flex items-center gap-1 px-2 py-1 rounded-lg border border-surface-hover text-[11px] text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
              title="打开插件目录"
            >
              <FolderOpen size={12} />
            </button>
            <button
              onClick={onClose}
              className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
              aria-label="关闭"
            >
              <X size={15} />
            </button>
          </div>
        </div>

        {/* Search */}
        <div className="px-4 pt-3">
          <div className="flex items-center gap-2 px-2.5 py-1.5 rounded-lg bg-background border border-surface-hover">
            <Search size={13} className="text-text-secondary/50 shrink-0" />
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="搜索插件"
              className="flex-1 bg-transparent text-[12px] text-text-primary outline-none placeholder:text-text-secondary/40"
            />
          </div>
        </div>

        {/* Source tabs */}
        <div className="px-4 pt-2 flex items-center gap-1">
          {([
            ['all', '全部', skills.length],
            ['builtin', '内置', skills.filter((s) => s.source === 'builtin').length],
            ['user', '我的插件', skills.filter((s) => s.source !== 'builtin').length],
          ] as const).map(([key, label, count]) => (
            <button
              key={key}
              type="button"
              onClick={() => setSourceTab(key)}
              className={`px-2.5 py-1 rounded-md text-[11px] transition-colors ${
                sourceTab === key
                  ? 'bg-primary/15 text-primary'
                  : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
              }`}
            >
              {label} <span className="text-[10px] opacity-70">{count}</span>
            </button>
          ))}
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto px-4 py-3">
          {error && <div className="mb-3 text-xs text-red-400">{error}</div>}
          <p className="mb-3 text-[11px] text-text-secondary/60 leading-relaxed">
            插件（技能）按会话挂载：未挂载的插件不会出现在对话中，其描述也不占用上下文。
            点击卡片查看详情并挂载到当前会话。
          </p>
          {loading && skills.length === 0 ? (
            <div className="flex items-center justify-center py-12 text-text-secondary/60">
              <Loader2 size={16} className="animate-spin" />
            </div>
          ) : filtered.length === 0 ? (
            <div className="text-sm text-text-secondary/60 py-10 text-center">
              {skills.length === 0
                ? '还没有安装任何插件。点击右上角「文件夹导入」或「压缩包导入」添加技能。'
                : '没有匹配的插件。'}
            </div>
          ) : (
            <div className="space-y-4">
              {[
                { label: '内置', items: builtinSkills },
                { label: '我的插件', items: userSkills },
              ].map(
                (group) =>
                  group.items.length > 0 && (
                    <div key={group.label}>
                      {sourceTab === 'all' && (
                        <div className="text-[11px] text-text-secondary/50 mb-1.5">{group.label}</div>
                      )}
                      <div className="grid grid-cols-2 gap-2.5">
                        {group.items.map((s) => {
                          const missingRequired = s.dependencies.filter(
                            (d) => !d.available && !d.optional && !d.ignored
                          );
                          const missingOptional = s.dependencies.filter(
                            (d) => !d.available && d.optional && !d.ignored
                          );
                          const missingPip = missingRequired.filter((d) => d.kind === 'pip');
                          const missingBin = missingRequired.filter((d) => d.kind !== 'pip');
                          const installing = installingCard === s.name;
                          return (
                            <div
                              key={s.name}
                              role="button"
                              tabIndex={0}
                              onClick={() => openDetail(s.name)}
                              onKeyDown={(e) => e.key === 'Enter' && openDetail(s.name)}
                              className="group relative flex min-h-[190px] text-left flex-col rounded-xl border border-surface-hover bg-background/40 p-3 hover:border-primary/40 transition-colors cursor-pointer"
                            >
                              <div className="flex items-start gap-2 pr-24">
                                <SkillIcon name={s.name} size={16} />
                                <div className="min-w-0 flex-1">
                                  <div className="flex flex-wrap items-center gap-1.5">
                                    <span className="text-[13px] font-medium text-text-primary font-mono truncate">
                                      {s.name}
                                    </span>
                                    {mounted.has(s.name) && (
                                      <span className="shrink-0 text-[10px] px-1.5 py-px rounded-full border border-primary/30 text-primary bg-primary/10">
                                        已挂载
                                      </span>
                                    )}
                                    {s.source === 'builtin' ? (
                                      <span className="shrink-0 text-[10px] px-1.5 py-px rounded-full border border-sky-400/30 text-sky-400 bg-sky-400/10">
                                        内置
                                      </span>
                                    ) : (
                                      reviewBadge(s)
                                    )}
                                    {detailLoading === s.name && (
                                      <Loader2 size={11} className="animate-spin text-text-secondary/40" />
                                    )}
                                  </div>
                                  <p className="mt-1.5 text-[11px] text-text-secondary leading-relaxed line-clamp-3">
                                    {s.description || '（无描述）'}
                                  </p>
                                  {s.scenarios.length > 0 && (
                                    <div className="mt-1.5 flex flex-wrap gap-1">
                                      {s.scenarios.slice(0, 3).map((scenario) => (
                                        <span
                                          key={scenario}
                                          className="rounded-full border border-surface-hover bg-surface-hover/40 px-1.5 py-px text-[9px] text-text-secondary"
                                        >
                                          {scenario}
                                        </span>
                                      ))}
                                      {s.scenarios.length > 3 && (
                                        <span className="text-[9px] text-text-secondary/50">
                                          +{s.scenarios.length - 3}
                                        </span>
                                      )}
                                    </div>
                                  )}
                                </div>
                              </div>

                              <div className="absolute right-2 top-2 flex items-center gap-1">
                                {s.source !== 'builtin' && s.reviewState === 'unreviewed' ? (
                                  <span className="rounded-md bg-amber-500/10 px-2 py-1 text-[10px] text-amber-300">
                                    待审查
                                  </span>
                                ) : s.source !== 'builtin' && s.reviewState === 'stale' ? (
                                  <span className="rounded-md bg-amber-500/10 px-2 py-1 text-[10px] text-amber-300">
                                    待复审
                                  </span>
                                ) : s.source !== 'builtin' && s.reviewState === 'risk' ? (
                                  <span className="rounded-md bg-red-500/10 px-2 py-1 text-[10px] text-red-300">
                                    存在风险
                                  </span>
                                ) : installing ? (
                                  <span className="flex items-center gap-1 rounded-md bg-sky-500/10 px-2 py-1 text-[10px] text-sky-300">
                                    <Loader2 size={10} className="animate-spin" />
                                    安装中
                                  </span>
                                ) : missingPip.length > 0 ? (
                                  <button
                                    type="button"
                                    onClick={(e) => {
                                      e.stopPropagation();
                                      void installForCard(s);
                                    }}
                                    disabled={installingCard !== null || installingDeps}
                                    className="rounded-md bg-primary/15 px-2 py-1 text-[10px] text-primary hover:bg-primary/25 disabled:opacity-50"
                                  >
                                    {s.dependencySource === 'review' ? '选择依赖' : '安装依赖'}
                                  </button>
                                ) : missingBin.length > 0 ? (
                                  <span
                                    className="rounded-md bg-amber-500/10 px-2 py-1 text-[10px] text-amber-300"
                                    title={`系统程序需手动安装：${missingBin.map((d) => d.name).join(', ')}`}
                                  >
                                    需系统依赖
                                  </span>
                                ) : (
                                  <span className="rounded-md bg-emerald-500/10 px-2 py-1 text-[10px] text-emerald-300">
                                    可运行
                                  </span>
                                )}
                              </div>

                              <div className="mt-auto space-y-1 pt-3">
                                <div className="truncate text-[10px] text-text-secondary/70" title={s.tools.join(', ')}>
                                  工具：{s.tools.length > 0 ? s.tools.join(' / ') : '继承会话'}
                                </div>
                                {s.source !== 'builtin' && s.reviewState === 'unreviewed' ? (
                                  <div className="text-[10px] text-amber-300/80">依赖：审查后生成</div>
                                ) : s.dependencies.length > 0 ? (
                                  <div className="truncate text-[10px] text-text-secondary/70">
                                    {s.dependencySource === 'review' ? '依赖（审查推断）：' : '依赖：'}
                                    {s.dependencies.filter((d) => d.available).length}/{s.dependencies.length} 可用
                                    {missingBin.length > 0 && (
                                      <span className="ml-1 text-amber-300">
                                        · 缺 {missingBin.map((d) => d.name).join(', ')}
                                      </span>
                                    )}
                                    {missingOptional.length > 0 && (
                                      <span className="ml-1 text-text-secondary/50">
                                        · 可选缺 {missingOptional.map((d) => d.name).join(', ')}
                                      </span>
                                    )}
                                  </div>
                                ) : (
                                  <div className="text-[10px] text-text-secondary/50">
                                    {s.dependencySource === 'review' ? '审查未发现依赖' : '未声明外部依赖'}
                                  </div>
                                )}
                                {installing && installProgress && (
                                  <div className="truncate text-[10px] text-primary/70" title={installProgress}>
                                    {installProgress}
                                  </div>
                                )}
                              </div>

                              <div className="flex items-center justify-between pt-2">
                                <span className="text-[10px] text-text-secondary/40">点击查看详情</span>
                                {s.source !== 'builtin' && (
                                  <button
                                    type="button"
                                    onClick={(e) => {
                                      e.stopPropagation();
                                      void startReview(s.name);
                                    }}
                                    disabled={reviewing.has(s.name)}
                                    title="AI 审查（提示词/脚本安全 + 依赖检查）"
                                    className={
                                      s.reviewState === 'unreviewed' || s.reviewState === 'stale'
                                        ? 'flex items-center gap-1 rounded-md bg-amber-500/10 px-2 py-1 text-[10px] text-amber-300 hover:bg-amber-500/20 disabled:opacity-50'
                                        : 'rounded p-1 text-text-secondary/50 opacity-100 hover:text-primary hover:bg-surface-hover transition-colors disabled:opacity-50'
                                    }
                                  >
                                    {reviewing.has(s.name) ? (
                                      <Loader2 size={12} className="animate-spin" />
                                    ) : s.reviewState === 'unreviewed' || s.reviewState === 'stale' ? (
                                      <>
                                        <ShieldCheck size={11} />
                                        {s.reviewState === 'stale' ? '重新审查' : '开始审查'}
                                      </>
                                    ) : (
                                      <ShieldCheck size={12} />
                                    )}
                                  </button>
                                )}
                              </div>
                            </div>
                          );
                        })}
                      </div>
                    </div>
                  )
              )}
            </div>
          )}
        </div>
      </div>

      {/* Detail dialog (second level) */}
      {detail && (
        <div className="absolute inset-0 z-10 flex items-center justify-center">
          <div className="absolute inset-0 bg-black/40" onClick={() => setDetail(null)} />
          <div className="relative w-[560px] max-w-[90%] max-h-[86%] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
            <div className="flex items-center gap-2 px-4 py-3 border-b border-surface-hover">
              <SkillIcon name={detail.name} size={16} />
              <span className="text-sm font-medium text-text-primary font-mono">{detail.name}</span>
              {detail.source === 'builtin' && (
                <span className="text-[10px] px-1.5 py-px rounded-full border border-sky-400/30 text-sky-400 bg-sky-400/10">
                  内置
                </span>
              )}
              {mounted.has(detail.name) && (
                <span className="text-[10px] px-1.5 py-px rounded-full border border-primary/30 text-primary bg-primary/10">
                  已挂载
                </span>
              )}
              <button
                onClick={() => setDetail(null)}
                className="ml-auto p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
                aria-label="返回列表"
              >
                <X size={15} />
              </button>
            </div>
            <div className="flex-1 overflow-y-auto px-4 py-3 space-y-3">
              {detail.description && (
                <p className="text-[12px] text-text-secondary">{detail.description}</p>
              )}
              <p className="text-[10px] text-text-secondary/50 truncate" title={detail.path}>
                {detail.path}
              </p>
              {(detail.scenarios.length > 0 || detail.rules.length > 0) && (
                <div className="rounded-lg border border-surface-hover bg-background p-3 space-y-2">
                  {detail.scenarios.length > 0 && (
                    <div>
                      <div className="text-[10px] text-text-secondary/60 mb-1">适用场景</div>
                      <div className="flex flex-wrap gap-1">
                        {detail.scenarios.map((scenario) => (
                          <span key={scenario} className="rounded-full border border-surface-hover bg-surface-hover/40 px-1.5 py-px text-[10px] text-text-secondary">
                            {scenario}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                  {detail.rules.length > 0 && (
                    <div>
                      <div className="text-[10px] text-text-secondary/60 mb-1">模板 / 规范</div>
                      <div className="flex flex-wrap gap-1">
                        {detail.rules.map((rule) => (
                          <span key={rule} className="rounded-full border border-surface-hover px-1.5 py-px text-[10px] text-text-secondary">
                            {rule}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                </div>
              )}
              {detail.tools.length > 0 && (
                <div className="rounded-lg border border-surface-hover bg-background p-3 space-y-1.5">
                  <span className="text-[12px] font-medium text-text-primary">工具权限</span>
                  <div className="text-[11px] text-text-secondary">
                    {detail.tools.map((t) => (
                      <span key={t} className="inline-block mr-1.5 px-1 rounded bg-surface-hover/60 font-mono">
                        {t}
                      </span>
                    ))}
                  </div>
                  <p className="text-[10px] text-text-secondary/50 leading-relaxed">
                    挂载后这些工具将对挂载的会话开放（即使会话本身限制了工具范围）；卸载即回收。
                  </p>
                </div>
              )}
              {detail.dependencies.length > 0 && (
                <div className="rounded-lg border border-surface-hover bg-background p-3 space-y-2">
                  <div className="flex items-center gap-2">
                    <span className="text-[12px] font-medium text-text-primary">运行依赖</span>
                    {detail.dependencySource === 'review' && (
                      <span className="text-[10px] text-amber-300/80">审查推断，默认不安装</span>
                    )}
                    <button
                      onClick={() => void installSelectedDeps(detail)}
                      disabled={
                        installingDeps ||
                        installingCard !== null ||
                        !detail.dependencies.some(
                          (d) =>
                            d.kind === 'pip' &&
                            !d.available &&
                            !d.ignored &&
                            depSelection.has(d.name)
                        )
                      }
                      className="ml-auto flex items-center gap-1 px-2 py-0.5 rounded-md text-[11px] bg-primary/10 text-primary hover:bg-primary/20 transition-colors disabled:opacity-50"
                    >
                      {installingDeps && <Loader2 size={11} className="animate-spin" />}
                      安装选中（{depSelection.size}）
                    </button>
                  </div>
                  {installingDeps && (
                    <p className="text-[10px] text-primary/70 leading-relaxed">
                      {installProgress ?? '正在准备…'}（首次需下载并安装 Miniconda，可能持续数分钟，请保持窗口打开）
                    </p>
                  )}
                  <div className="space-y-1">
                    {detail.dependencies.map((d) => (
                      <div
                        key={`${d.kind}-${d.name}`}
                        className={`flex items-center gap-2 rounded-md px-1.5 py-1 text-[11px] ${
                          d.ignored ? 'opacity-50' : ''
                        }`}
                      >
                        {d.kind === 'pip' && !d.available && !d.ignored ? (
                          <input
                            type="checkbox"
                            checked={depSelection.has(d.name)}
                            onChange={() => toggleDepSelection(d.name)}
                            className="accent-primary"
                          />
                        ) : (
                          <span className="w-3" />
                        )}
                        <span
                          className={
                            d.ignored
                              ? 'text-text-secondary/60 line-through'
                              : d.available
                                ? 'text-emerald-400/80'
                                : d.optional
                                  ? 'text-text-secondary/70'
                                  : 'text-red-400'
                          }
                          title={
                            d.kind === 'binary'
                              ? '系统程序'
                              : `Python 包（隔离环境 siku-office）${d.hint ? `：${d.hint}` : ''}`
                          }
                        >
                          {d.name}
                          {d.optional ? '（可选）' : ''}
                          {d.ignored ? '（已忽略）' : d.available ? ' ✓' : ' ✗'}
                        </span>
                        {!d.available && (
                          <button
                            type="button"
                            onClick={() =>
                              d.ignored
                                ? void restoreDependency(detail.name, d.name)
                                : void ignoreDependency(detail.name, d.name)
                            }
                            className="ml-auto text-[10px] text-text-secondary/50 hover:text-primary"
                          >
                            {d.ignored ? '恢复' : '忽略'}
                          </button>
                        )}
                      </div>
                    ))}
                  </div>
                  <p className="text-[10px] text-text-secondary/50 leading-relaxed">
                    pip 包安装在应用托管的隔离 conda 环境（siku-office），不影响系统 Python。
                    {detail.dependencies.some(
                      (d) => d.kind === 'binary' && !d.available && !d.optional && !d.ignored
                    ) && ' 缺失的系统程序需手动安装（soffice 来自 LibreOffice）。'}
                  </p>
                </div>
              )}
              <pre className="text-[11px] leading-relaxed text-text-secondary bg-background border border-surface-hover rounded-lg p-3 whitespace-pre-wrap break-words max-h-[300px] overflow-y-auto">
                {detail.content || '（SKILL.md 无正文）'}
              </pre>
              {detail.review && (
                <div className="rounded-lg border border-surface-hover bg-background p-3 space-y-2">
                  <div className="flex items-center gap-2">
                    <span className="text-[12px] font-medium text-text-primary">AI 审查报告</span>
                    <span
                      className={`text-[10px] px-1.5 py-px rounded-full border ${
                        detail.review.verdict === 'pass'
                          ? 'border-emerald-400/30 text-emerald-400 bg-emerald-400/10'
                          : detail.review.verdict === 'warn'
                            ? 'border-amber-400/30 text-amber-400 bg-amber-400/10'
                            : 'border-red-400/30 text-red-400 bg-red-400/10'
                      }`}
                    >
                      {detail.review.verdict === 'pass'
                        ? '通过'
                        : detail.review.verdict === 'warn'
                          ? '注意'
                          : '风险'}
                    </span>
                    <span className="text-[10px] text-text-secondary/50">
                      {detail.review.reviewedAt.slice(0, 19).replace('T', ' ')}
                    </span>
                  </div>
                  {detail.review.summary && (
                    <p className="text-[11px] text-text-secondary">{detail.review.summary}</p>
                  )}
                  {detail.review.dependencies.length > 0 && (
                    <div className="text-[11px] text-text-secondary">
                      依赖：
                      {detail.review.dependencies.map((d) => (
                        <span
                          key={`${d.kind}-${d.name}`}
                          className={`inline-block mr-1.5 px-1 rounded ${
                            d.available ? 'text-emerald-400/80' : 'text-red-400'
                          }`}
                          title={d.kind === 'binary' ? '系统程序' : 'Python 包'}
                        >
                          {d.name}
                          {d.available ? '✓' : '✗'}
                        </span>
                      ))}
                    </div>
                  )}
                  {detail.review.findings.length > 0 && (
                    <div className="max-h-[140px] overflow-y-auto space-y-1">
                      {detail.review.findings.slice(0, 20).map((f, i) => (
                        <div key={i} className="text-[10px] text-text-secondary/80 font-mono">
                          <span className={f.severity === 'risk' ? 'text-red-400' : 'text-amber-400'}>
                            [{f.category}]
                          </span>{' '}
                          {f.file}:{f.line} {f.excerpt}
                        </div>
                      ))}
                      {detail.review.findings.length > 20 && (
                        <div className="text-[10px] text-text-secondary/50">
                          … 共 {detail.review.findings.length} 条
                        </div>
                      )}
                    </div>
                  )}
                </div>
              )}
            </div>
            <div className="flex items-center gap-2 px-4 py-3 border-t border-surface-hover">
              {detail.source !== 'builtin' && (
                <button
                  onClick={() => void startReview(detail.name)}
                  disabled={reviewing.has(detail.name)}
                  className="flex items-center gap-1 px-3 py-1.5 rounded-lg text-[12px] border border-surface-hover text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors disabled:opacity-50"
                >
                  {reviewing.has(detail.name) ? (
                    <Loader2 size={12} className="animate-spin" />
                  ) : (
                    <ShieldCheck size={12} />
                  )}
                  {detail.review ? '重新审查' : 'AI 审查'}
                </button>
              )}
              <button
                onClick={() => toggleMount(detail.name)}
                disabled={!mounted.has(detail.name) && detailMountBlocked}
                className={`px-3 py-1.5 rounded-lg text-[12px] transition-colors ${
                  mounted.has(detail.name)
                    ? 'border border-surface-hover text-text-secondary hover:bg-surface-hover'
                    : detailMountBlocked
                      ? 'bg-surface-hover text-text-secondary/50 cursor-not-allowed'
                      : 'bg-primary/80 text-white hover:bg-primary'
                }`}
              >
                {mounted.has(detail.name)
                  ? '从当前会话移除'
                  : !activeSession
                    ? '挂载（需先选择会话）'
                    : detailMountBlocked
                      ? detailMountBlockLabel
                      : '挂载到当前会话'}
              </button>
              {detail.source !== 'builtin' && (
                <button
                  onClick={() => removeSkill(detail.name)}
                  className="flex items-center gap-1 px-3 py-1.5 rounded-lg text-[12px] text-red-400 hover:bg-red-400/10 transition-colors"
                >
                  <Trash2 size={12} />
                  删除
                </button>
              )}
            </div>
          </div>
        </div>
      )}

      {depResults && (() => {
        const failed = depResults.results.filter((r) => !r.ok);
        const installed = depResults.results.filter((r) => r.ok);
        return (
          <div className="absolute inset-0 z-20 flex items-center justify-center">
            <div className="absolute inset-0 bg-black/40" onClick={() => setDepResults(null)} />
            <div className="relative w-[460px] max-w-[90%] max-h-[70%] overflow-y-auto bg-surface border border-surface-hover rounded-xl shadow-2xl p-4 space-y-3">
              <div className="text-sm font-medium text-text-primary">依赖安装结果</div>
              {installed.length > 0 && (
                <div className="text-[11px] text-emerald-300">
                  已安装：{installed.map((r) => r.name).join('、')}
                </div>
              )}
              {failed.length === 0 ? (
                <p className="text-[11px] text-text-secondary">全部选中依赖已安装。</p>
              ) : (
                <div className="space-y-2">
                  <p className="text-[11px] text-amber-300">
                    以下依赖安装失败。可以跳过、忽略误报，或复制命令手动安装；依赖不阻塞挂载。
                  </p>
                  {failed.map((r) => (
                    <div key={r.name} className="rounded-lg border border-surface-hover bg-background p-2 space-y-1">
                      <div className="flex items-center gap-2 text-[12px]">
                        <span className="font-mono text-text-primary">{r.name}</span>
                        <span className="text-[10px] text-red-400">
                          {r.code === 'not_found' ? 'PyPI 无此包' : r.code === 'network' ? '网络失败' : '安装失败'}
                        </span>
                        <button
                          onClick={() => void ignoreDependency(depResults.skill, r.name)}
                          className="ml-auto text-[10px] text-text-secondary/60 hover:text-primary"
                        >
                          忽略此依赖
                        </button>
                      </div>
                      {r.error && (
                        <div className="max-h-24 overflow-y-auto whitespace-pre-wrap break-words text-[9px] text-text-secondary/60 font-mono">
                          {r.error}
                        </div>
                      )}
                    </div>
                  ))}
                </div>
              )}
              <div className="flex items-center gap-2 pt-1">
                {failed.length > 0 && (
                  <>
                    <button
                      onClick={() => void retryFailed()}
                      className="px-3 py-1.5 rounded-lg text-[11px] bg-primary/15 text-primary hover:bg-primary/25"
                    >
                      重试失败项
                    </button>
                    <button
                      onClick={() => void copyFailedInstallCommand()}
                      className="px-3 py-1.5 rounded-lg text-[11px] border border-surface-hover text-text-secondary hover:text-text-primary"
                    >
                      复制安装命令
                    </button>
                  </>
                )}
                <button
                  onClick={() => setDepResults(null)}
                  className="ml-auto px-3 py-1.5 rounded-lg text-[11px] bg-surface-hover text-text-secondary hover:text-text-primary"
                >
                  关闭并继续
                </button>
              </div>
            </div>
          </div>
        );
      })()}
    </Modal>
  );
}
