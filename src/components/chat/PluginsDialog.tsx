import { useCallback, useEffect, useState } from 'react';
import {
  Puzzle, X, RefreshCw, Search, FolderOpen, FolderInput, FileArchive, Trash2, Loader2,
  ShieldCheck,
} from 'lucide-react';
import {
  skillsList, skillsGet, skillsImportFolder, skillsImportZip, skillsDelete,
  skillsOpenDirectory, agentSetSessionSkills, skillsReviewStart, skillsReviewCollect,
  type SkillInfo, type SkillDetail,
} from '@/lib/tauri';
import type { AgentStreamEvent } from '@/lib/types';
import { useChatStore } from '@/stores/chatStore';
import { useDialog } from '@/hooks/useDialog';

/** Plugins (skills) dialog — same modal shell as the task center.
 *
 *  Skills are per-session plugins: nothing here is visible to the LLM until
 *  the skill is mounted on a session ("挂载到当前会话" or the session config
 *  panel). This dialog manages the library: import (folder / zip), preview,
 *  mount/unmount on the active session, delete. */
export function PluginsDialog({ onClose }: { onClose: () => void }) {
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const [detail, setDetail] = useState<SkillDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  /** skill name → reviewer session id for in-flight AI reviews. */
  const [reviewing, setReviewing] = useState<Map<string, string>>(new Map());
  const { sessions, activeSessionId, setSessions } = useChatStore();
  const { confirm, alert } = useDialog();

  const activeSession = sessions.find((s) => s.id === activeSessionId) ?? null;
  const mounted = new Set(activeSession?.selected_skills ?? []);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setSkills(await skillsList());
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
          })();
        })
      )
      .then((u) => {
        un = u;
      });
    return () => un?.();
  }, [reviewing, load]);

  const openDetail = async (name: string) => {
    setDetailLoading(true);
    try {
      setDetail(await skillsGet(name));
    } catch (err) {
      setError(String(err));
    } finally {
      setDetailLoading(false);
    }
  };

  const importFolder = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({ directory: true, title: '选择技能文件夹（内含 SKILL.md）' });
      if (typeof selected !== 'string') return;
      setBusy(true);
      await skillsImportFolder(selected);
      await load();
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
      await load();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  /** Mount/unmount a skill on the ACTIVE session (per-session plugins). */
  const toggleMount = async (name: string) => {
    if (!activeSession) {
      await alert('请先在左侧选择一个会话，再将技能挂载到该会话', '挂载技能');
      return;
    }
    const current = activeSession.selected_skills ?? [];
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
      '删除技能'
    );
    if (!ok) return;
    try {
      await skillsDelete(name);
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
      !query.trim() ||
      s.name.toLowerCase().includes(query.trim().toLowerCase()) ||
      s.description.toLowerCase().includes(query.trim().toLowerCase())
  );

  /** Review state badge: 绿=通过 / 黄=注意 / 红=风险；stale（审查后内容变更）按待复审显示。 */
  const reviewBadge = (s: SkillInfo) => {
    const r = s.review;
    if (!r) return null;
    if (r.stale) {
      return (
        <span
          title="技能内容在审查后发生变更，建议重新审查"
          className="shrink-0 text-[10px] px-1.5 py-px rounded-full border border-surface-hover text-text-secondary/60"
        >
          待复审
        </span>
      );
    }
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

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
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
            <div className="grid grid-cols-2 gap-2.5">
              {filtered.map((s) => (
                <div
                  key={s.name}
                  role="button"
                  tabIndex={0}
                  onClick={() => openDetail(s.name)}
                  onKeyDown={(e) => e.key === 'Enter' && openDetail(s.name)}
                  className="group relative text-left rounded-xl border border-surface-hover bg-background/40 p-3 hover:border-primary/40 transition-colors cursor-pointer"
                >
                  {/* AI 审查 (hover, top-right) */}
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      void startReview(s.name);
                    }}
                    disabled={reviewing.has(s.name)}
                    title="AI 审查（提示词/脚本安全 + 依赖检查）"
                    className="absolute top-2 right-2 opacity-0 hover:opacity-100 group-hover:opacity-100 p-1 rounded-md text-text-secondary hover:text-primary hover:bg-surface-hover transition-opacity disabled:opacity-50"
                  >
                    {reviewing.has(s.name) ? (
                      <Loader2 size={13} className="animate-spin" />
                    ) : (
                      <ShieldCheck size={13} />
                    )}
                  </button>
                  <div className="flex items-center gap-2 mb-1 pr-6">
                    <span className="text-[13px] font-medium text-text-primary font-mono truncate">
                      {s.name}
                    </span>
                    {mounted.has(s.name) && (
                      <span className="shrink-0 text-[10px] px-1.5 py-px rounded-full border border-primary/30 text-primary bg-primary/10">
                        已挂载
                      </span>
                    )}
                    {reviewBadge(s)}
                    {detailLoading && <Loader2 size={11} className="animate-spin text-text-secondary/40" />}
                  </div>
                  <p className="text-[11px] text-text-secondary leading-relaxed line-clamp-2">
                    {s.description || '（无描述）'}
                  </p>
                </div>
              ))}
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
              <span className="text-sm font-medium text-text-primary font-mono">{detail.name}</span>
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
              <button
                onClick={() => toggleMount(detail.name)}
                className={`px-3 py-1.5 rounded-lg text-[12px] transition-colors ${
                  mounted.has(detail.name)
                    ? 'border border-surface-hover text-text-secondary hover:bg-surface-hover'
                    : 'bg-primary/80 text-white hover:bg-primary'
                }`}
              >
                {mounted.has(detail.name)
                  ? '从当前会话移除'
                  : activeSession
                    ? '挂载到当前会话'
                    : '挂载（需先选择会话）'}
              </button>
              <button
                onClick={() => removeSkill(detail.name)}
                className="flex items-center gap-1 px-3 py-1.5 rounded-lg text-[12px] text-red-400 hover:bg-red-400/10 transition-colors"
              >
                <Trash2 size={12} />
                删除
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
