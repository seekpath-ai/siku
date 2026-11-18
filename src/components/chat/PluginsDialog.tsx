import { useCallback, useEffect, useState } from 'react';
import {
  Puzzle, X, RefreshCw, Search, FolderOpen, FolderInput, FileArchive, Trash2, Loader2,
  ShieldCheck, ChevronRight,
} from 'lucide-react';
import {
  skillsList, skillsGet, skillsImportFolder, skillsImportZip, skillsDelete,
  skillsOpenDirectory, agentSetSessionSkills, skillsReviewStart, skillsReviewRun, skillsReviewCollect,
  type SkillInfo, type SkillDetail,
} from '@/lib/tauri';
import type { AgentStreamEvent } from '@/lib/types';
import { useChatStore } from '@/stores/chatStore';
import { useDialog } from '@/hooks/useDialog';
import { notify } from '@/lib/notify';
import { Modal } from '@/components/ui/Modal';
import { SkillIcon } from './SkillIcon';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';

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
  /** skill name → reviewer session id for in-flight AI reviews. */
  const [reviewing, setReviewing] = useState<Map<string, string>>(new Map());
  const [reviewStarting, setReviewStarting] = useState<string | null>(null);
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
    setReviewStarting(name);
    notify('正在创建审查会话…', 'info');
    try {
      const { sessionId } = await Promise.race([
        skillsReviewStart(name),
        new Promise<never>((_, reject) =>
          window.setTimeout(() => reject(new Error('创建审查会话超时，请重启应用后重试')), 20000)
        ),
      ]);
      setReviewing((m) => new Map(m).set(name, sessionId));
      const win = new WebviewWindow(`pet-chat-${Date.now()}`, {
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
      win.once('tauri://error', (event) => {
        const message = String((event as { payload?: unknown }).payload ?? '无法打开审查窗口');
        setError(message);
        notify(message, 'error');
      });
      notify('审查窗口已打开，正在等待模型…', 'info');
      // Fire-and-forget: the backend keeps running this turn while the
      // window is already visible. Awaiting it here was what made the
      // button appear to do nothing during a slow LLM review.
      void skillsReviewRun(sessionId, name).catch((err) => {
        const message = String(err);
        setError(message);
        notify(message, 'error');
        setReviewing((m) => {
          const next = new Map(m);
          next.delete(name);
          return next;
        });
      });
    } catch (err) {
      const message = String(err);
      setError(message);
      notify(message, 'error');
      setReviewing((m) => {
        const next = new Map(m);
        next.delete(name);
        return next;
      });
    } finally {
      setReviewStarting(null);
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
            } else if (ev.type === 'error') {
              setError(ev.content ? String(ev.content) : '技能审查失败');
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
      setDetailLoading(null);
      return;
    }
    setDetailLoading(name);
    try {
      const next = await skillsGet(name);
      skillDetailCache.set(name, next);
      setDetail(next);
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
          notify('该技能审查结果为高风险，禁止挂载', 'error');
          return;
        }
        if (state === 'warn') {
          const ok = await confirm(
            '该技能审查结果为中风险，可能包含需要人工确认的权限或模式。仍要挂载吗？',
            { title: '挂载中风险的技能' },
          );
          if (!ok) return;
        }
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

  /** Review state badge: 绿=低风险 / 黄=中风险 / 红=高风险；stale（审查后内容变更）按待复审显示。 */
  const reviewBadge = (s: SkillInfo) => {
    const r = s.review;
    if (!r) return null;
    if (r.stale) return null; // top-right status already shows 待复审
    const styles: Record<string, [string, string, string]> = {
      pass: [
        '低风险',
        'border-emerald-400/30 text-emerald-400 bg-emerald-400/10',
        'AI 审查未发现明显风险',
      ],
      warn: [
        '中风险',
        'border-amber-400/30 text-amber-400 bg-amber-400/10',
        'AI 审查发现需人工确认的风险项，挂载前会再次确认',
      ],
      risk: [
        '高风险',
        'border-red-400/30 text-red-400 bg-red-400/10',
        'AI 审查发现高风险内容，禁止挂载',
      ],
    };
    const [label, cls, hint] = styles[r.verdict] ?? styles.warn;
    return (
      <span
        className={`shrink-0 rounded-full border px-1.5 py-px text-[10px] ${cls}`}
        title={hint}
      >
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
      <div className="relative w-[680px] max-w-[94vw] h-[620px] max-h-[88vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
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
            插件按会话挂载，未挂载不会进入对话。审查结果：低风险可直接挂载，中风险需确认，高风险禁止挂载。
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
                          const isMounted = mounted.has(s.name);
                          return (
                            <div
                              key={s.name}
                              role="button"
                              tabIndex={0}
                              onClick={() => openDetail(s.name)}
                              onKeyDown={(e) => e.key === 'Enter' && openDetail(s.name)}
                              className={`group relative flex h-full cursor-pointer flex-col rounded-xl border p-3 text-left transition-colors ${
                                isMounted
                                  ? 'border-primary/35 bg-primary/[0.04] hover:border-primary/55'
                                  : 'border-surface-hover bg-background/40 hover:border-primary/40 hover:bg-background/60'
                              }`}
                            >
                              <div className="flex items-center gap-2">
                                <SkillIcon name={s.name} size={20} />
                                <span
                                  className="min-w-0 flex-1 truncate font-mono text-[13px] font-medium text-text-primary"
                                  title={s.name}
                                >
                                  {s.name}
                                </span>
                                <div className="flex shrink-0 items-center gap-1">
                                  {isMounted && (
                                    <span className="rounded-full border border-primary/30 bg-primary/10 px-1.5 py-px text-[10px] text-primary">
                                      已挂载
                                    </span>
                                  )}
                                  {s.source === 'builtin' ? (
                                    <span className="rounded-full border border-sky-400/30 bg-sky-400/10 px-1.5 py-px text-[10px] text-sky-400">
                                      内置
                                    </span>
                                  ) : (
                                    <>
                                      {reviewBadge(s)}
                                      {s.reviewState === 'unreviewed' && (
                                        <span
                                          className="rounded-full border border-surface-hover bg-surface-hover/40 px-1.5 py-px text-[10px] text-text-secondary"
                                          title="尚未进行 AI 安全审查，审查后才能挂载"
                                        >
                                          待审查
                                        </span>
                                      )}
                                      {s.reviewState === 'stale' && (
                                        <span
                                          className="rounded-full border border-amber-400/30 bg-amber-400/10 px-1.5 py-px text-[10px] text-amber-400"
                                          title="审查后技能内容已变更，需要重新审查"
                                        >
                                          待复审
                                        </span>
                                      )}
                                    </>
                                  )}
                                  {detailLoading === s.name && (
                                    <Loader2 size={11} className="animate-spin text-text-secondary/40" />
                                  )}
                                </div>
                              </div>
                              <p className="mt-1.5 line-clamp-2 text-[11px] leading-relaxed text-text-secondary">
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

                              <div className="mt-auto flex items-center gap-2 border-t border-surface-hover/60 pt-2">
                                <span
                                  className="min-w-0 flex-1 truncate text-[10px] text-text-secondary/70"
                                  title={s.tools.join(', ')}
                                >
                                  工具：{s.tools.length > 0 ? s.tools.join(' / ') : '继承会话'}
                                </span>
                                {s.source === 'builtin' && (
                                  <ChevronRight
                                    size={13}
                                    className="shrink-0 text-text-secondary/25 transition-colors group-hover:text-primary/70"
                                  />
                                )}
                                {s.source !== 'builtin' && (
                                  <button
                                    type="button"
                                    onClick={(e) => {
                                      e.stopPropagation();
                                      void startReview(s.name);
                                    }}
                                    disabled={reviewing.has(s.name) || reviewStarting === s.name}
                                    title="AI 审查（提示词/脚本安全）"
                                    className={
                                      s.reviewState === 'unreviewed' || s.reviewState === 'stale'
                                        ? 'flex shrink-0 items-center gap-1 rounded-md bg-amber-500/10 px-2 py-1 text-[10px] text-amber-300 hover:bg-amber-500/20 disabled:opacity-50'
                                        : 'shrink-0 rounded p-1 text-text-secondary/50 transition-colors hover:bg-surface-hover hover:text-primary disabled:opacity-50'
                                    }
                                  >
                                    {reviewing.has(s.name) || reviewStarting === s.name ? (
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
                        ? '低风险'
                        : detail.review.verdict === 'warn'
                          ? '中风险'
                          : '高风险'}
                    </span>
                    <span className="text-[10px] text-text-secondary/50">
                      {detail.review.reviewedAt.slice(0, 19).replace('T', ' ')}
                    </span>
                  </div>
                  {detail.review.summary && (
                    <p className="text-[11px] text-text-secondary">{detail.review.summary}</p>
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
                  disabled={reviewing.has(detail.name) || reviewStarting === detail.name}
                  className="flex items-center gap-1 px-3 py-1.5 rounded-lg text-[12px] border border-surface-hover text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors disabled:opacity-50"
                >
                  {reviewing.has(detail.name) || reviewStarting === detail.name ? (
                    <Loader2 size={12} className="animate-spin" />
                  ) : (
                    <ShieldCheck size={12} />
                  )}
                  {reviewStarting === detail.name
                    ? '准备中…'
                    : reviewing.has(detail.name)
                      ? '审查中…'
                      : detail.review
                        ? '重新审查'
                        : 'AI 审查'}
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

    </Modal>
  );
}
