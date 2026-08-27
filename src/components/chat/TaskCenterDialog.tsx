import { useState, useEffect, useCallback } from 'react';
import {
  CalendarClock, X, Loader2, Trash2, Plus, Square, ChevronRight, Terminal, Clock, AlertTriangle,
} from 'lucide-react';
import {
  cronList, cronCreate, cronDelete, cronSetEnabled,
  taskSnapshot, taskOutput, taskStop, agentGetSession,
  type TaskOutput,
} from '@/lib/tauri';
import type { CronJob, TaskInfo } from '@/lib/types';

const WEEKDAYS = ['日', '一', '二', '三', '四', '五', '六'];

function pad2(n: number): string {
  return String(n).padStart(2, '0');
}

function isNum(s: string): boolean {
  return /^\d+$/.test(s);
}

/** Best-effort human-readable description of a 5-field cron expression.
 *  Recognizes the shapes the create form produces; anything else is shown
 *  as the raw cron string. */
function humanizeCron(cron: string, recurring: boolean): string {
  const f = cron.trim().split(/\s+/);
  if (f.length !== 5) return cron;
  const [m, h, dom, mon, dow] = f;

  // m H * * * → 每天 HH:MM
  if (isNum(m) && isNum(h) && dom === '*' && mon === '*' && dow === '*') {
    return `每天 ${pad2(Number(h))}:${pad2(Number(m))}`;
  }
  // m H * * d → 每周周X HH:MM（0 和 7 都是周日）
  if (isNum(m) && isNum(h) && dom === '*' && mon === '*' && isNum(dow)) {
    const d = Number(dow);
    if (d >= 0 && d <= 7) {
      return `每周周${WEEKDAYS[d % 7]} ${pad2(Number(h))}:${pad2(Number(m))}`;
    }
  }
  // 0 */N * * * → 每隔 N 小时
  const hourly = h.match(/^\*\/(\d+)$/);
  if (m === '0' && hourly && dom === '*' && mon === '*' && dow === '*') {
    return `每隔 ${Number(hourly[1])} 小时`;
  }
  // m H D M *（非循环）→ 一次性 YYYY-MM-DD HH:MM
  // cron 表达式不含年份，年份用当前年补齐。
  if (!recurring && isNum(m) && isNum(h) && isNum(dom) && isNum(mon) && dow === '*') {
    const year = new Date().getFullYear();
    return `一次性 ${year}-${pad2(Number(mon))}-${pad2(Number(dom))} ${pad2(Number(h))}:${pad2(Number(m))}`;
  }
  return cron;
}

const TASK_STATUS: Record<TaskInfo['status'], { label: string; cls: string }> = {
  running: { label: '运行中', cls: 'bg-blue-500/15 text-blue-400 animate-pulse' },
  completed: { label: '已完成', cls: 'bg-emerald-500/15 text-emerald-400' },
  failed: { label: '失败', cls: 'bg-red-500/15 text-red-400' },
  stopped: { label: '已停止', cls: 'bg-surface-hover text-text-secondary' },
  timed_out: { label: '超时', cls: 'bg-orange-500/15 text-orange-400' },
};

function fmtTime(s: string): string {
  const d = new Date(s.includes('T') ? s : s.replace(' ', 'T'));
  if (Number.isNaN(d.getTime())) return s;
  return d.toLocaleString('zh-CN', { hour12: false });
}

interface Props {
  sessionId: string;
  onClose: () => void;
}

type Tab = 'cron' | 'tasks';
type SchedType = 'once' | 'daily' | 'weekly' | 'interval';

/** Task center: per-session scheduled prompts (cron jobs) plus the global
 *  list of background tasks with live status polling. */
export function TaskCenterDialog({ sessionId, onClose }: Props) {
  const [tab, setTab] = useState<Tab>('cron');

  // ── 计划任务 ──
  const [jobs, setJobs] = useState<CronJob[] | null>(null);
  const [schedType, setSchedType] = useState<SchedType>('daily');
  const [onceAt, setOnceAt] = useState('');
  const [dailyTime, setDailyTime] = useState('09:00');
  const [weeklyDay, setWeeklyDay] = useState('1');
  const [weeklyTime, setWeeklyTime] = useState('09:00');
  const [intervalHours, setIntervalHours] = useState(6);
  const [prompt, setPrompt] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [manualApproval, setManualApproval] = useState(false);
  const [formError, setFormError] = useState('');

  // ── 后台任务 ──
  const [tasks, setTasks] = useState<TaskInfo[] | null>(null);
  const [expanded, setExpanded] = useState<Record<string, { loading: boolean; output?: TaskOutput }>>({});
  const [stopping, setStopping] = useState<Set<string>>(new Set());

  useEffect(() => {
    const onDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onDown);
    return () => window.removeEventListener('keydown', onDown);
  }, [onClose]);

  const refreshJobs = useCallback(() => {
    cronList()
      .then((all) => setJobs(all.filter((j) => j.session_id === sessionId)))
      .catch(() => setJobs([]));
  }, [sessionId]);

  useEffect(() => {
    refreshJobs();
    agentGetSession(sessionId)
      .then((s) => {
        const mode = s.approval_config?.mode;
        setManualApproval(mode === 'manual' || mode === 'manual_all');
      })
      .catch(() => setManualApproval(false));
  }, [refreshJobs, sessionId]);

  const refreshTasks = useCallback(() => {
    taskSnapshot()
      .then(setTasks)
      .catch(() => setTasks([]));
  }, []);

  // Poll the task snapshot only while the tasks tab is visible.
  useEffect(() => {
    if (tab !== 'tasks') return;
    let cancelled = false;
    const load = () =>
      taskSnapshot()
        .then((t) => {
          if (!cancelled) setTasks(t);
        })
        .catch(() => {
          if (!cancelled) setTasks([]);
        });
    load();
    const timer = setInterval(load, 2000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [tab]);

  const toggleEnabled = (job: CronJob) => {
    cronSetEnabled(job.id, !job.enabled)
      .then(refreshJobs)
      .catch(() => {});
  };

  const removeJob = (id: string) => {
    cronDelete(id)
      .then(refreshJobs)
      .catch(() => {});
  };

  /** Build the cron expression for the current form state; returns null when
   *  the required time input is missing/invalid. */
  const buildCron = (): { cron: string; recurring: boolean } | null => {
    if (schedType === 'once') {
      const d = new Date(onceAt);
      if (!onceAt || Number.isNaN(d.getTime())) return null;
      return {
        cron: `${d.getMinutes()} ${d.getHours()} ${d.getDate()} ${d.getMonth() + 1} *`,
        recurring: false,
      };
    }
    if (schedType === 'daily') {
      const [hh, mm] = dailyTime.split(':').map(Number);
      if (dailyTime.length < 4) return null;
      return { cron: `${mm} ${hh} * * *`, recurring: true };
    }
    if (schedType === 'weekly') {
      const [hh, mm] = weeklyTime.split(':').map(Number);
      if (weeklyTime.length < 4) return null;
      return { cron: `${mm} ${hh} * * ${weeklyDay}`, recurring: true };
    }
    const n = Math.floor(intervalHours);
    if (!Number.isFinite(n) || n < 1 || n > 23) return null;
    return { cron: `0 */${n} * * *`, recurring: true };
  };

  const handleCreate = async () => {
    setFormError('');
    const built = buildCron();
    if (!built) {
      setFormError('请填写有效的触发时间');
      return;
    }
    if (!prompt.trim()) {
      setFormError('请填写提示词');
      return;
    }
    setSubmitting(true);
    try {
      await cronCreate({
        sessionId,
        cron: built.cron,
        prompt: prompt.trim(),
        recurring: built.recurring,
      });
      setPrompt('');
      refreshJobs();
    } catch (err) {
      setFormError(err instanceof Error ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  };

  const toggleExpand = (id: string) => {
    if (expanded[id]) {
      setExpanded((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
      return;
    }
    setExpanded((prev) => ({ ...prev, [id]: { loading: true } }));
    taskOutput(id)
      .then((output) => setExpanded((prev) => ({ ...prev, [id]: { loading: false, output } })))
      .catch(() =>
        setExpanded((prev) => ({
          ...prev,
          [id]: {
            loading: false,
            output: { status: 'unknown', exit_code: null, content: '读取输出失败', truncated: false },
          },
        }))
      );
  };

  const stopTask = (id: string) => {
    setStopping((prev) => new Set(prev).add(id));
    taskStop(id)
      .then(refreshTasks)
      .catch(() => {})
      .finally(() =>
        setStopping((prev) => {
          const next = new Set(prev);
          next.delete(id);
          return next;
        })
      );
  };

  const inputCls =
    'bg-background border border-surface-hover rounded-lg px-2 py-1.5 text-[12px] text-text-primary outline-none focus:border-primary/50';

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div className="relative w-[640px] max-w-[94vw] h-[580px] max-h-[88vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        {/* Header */}
        <div className="flex items-center gap-2 px-4 py-2.5 border-b border-surface-hover shrink-0">
          <CalendarClock size={15} className="text-text-secondary" />
          <span className="text-sm font-medium text-text-primary">任务中心</span>
          <div className="flex-1" />
          <button
            onClick={onClose}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭"
          >
            <X size={14} />
          </button>
        </div>

        {/* Tabs */}
        <div className="flex items-center gap-1 px-3 py-2 border-b border-surface-hover shrink-0">
          {(
            [
              { id: 'cron', label: '计划任务', icon: Clock },
              { id: 'tasks', label: '后台任务', icon: Terminal },
            ] as const
          ).map(({ id, label, icon: Icon }) => (
            <button
              key={id}
              onClick={() => setTab(id)}
              className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[12px] transition-colors ${
                tab === id
                  ? 'bg-primary/15 text-primary'
                  : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
              }`}
            >
              <Icon size={13} />
              {label}
            </button>
          ))}
        </div>

        {/* ── 计划任务 tab ── */}
        {tab === 'cron' && (
          <>
            <div className="flex-1 min-h-0 overflow-y-auto px-3 py-2">
              {jobs === null ? (
                <div className="flex items-center justify-center h-full text-sm text-text-secondary/60">
                  <Loader2 size={15} className="animate-spin mr-2" />加载中…
                </div>
              ) : jobs.length === 0 ? (
                <div className="flex flex-col items-center justify-center h-full gap-2 text-text-secondary/60">
                  <Clock size={22} className="text-text-secondary/40" />
                  <p className="text-[13px]">暂无计划任务</p>
                  <p className="text-[11px] text-text-secondary/40">
                    在下方创建定时或一次性任务，到点会自动向当前会话发送提示词
                  </p>
                </div>
              ) : (
                jobs.map((job) => (
                  <div
                    key={job.id}
                    className="flex items-center gap-2.5 px-2.5 py-2 rounded-lg hover:bg-surface-hover"
                  >
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="text-[12px] font-medium text-text-primary shrink-0">
                          {humanizeCron(job.cron, job.recurring)}
                        </span>
                        <span
                          className={`px-1.5 py-0.5 rounded text-[10px] ${
                            job.enabled
                              ? 'bg-emerald-500/15 text-emerald-400'
                              : 'bg-surface-hover text-text-secondary/70'
                          }`}
                        >
                          {job.enabled ? '启用' : '停用'}
                        </span>
                      </div>
                      <div className="truncate text-[11px] text-text-secondary/70 mt-0.5" title={job.prompt}>
                        {job.prompt}
                      </div>
                      {job.last_fired && (
                        <div className="text-[10px] text-text-secondary/40 mt-0.5">
                          上次触发 {fmtTime(job.last_fired)}
                        </div>
                      )}
                    </div>
                    {/* 启用开关 */}
                    <button
                      onClick={() => toggleEnabled(job)}
                      title={job.enabled ? '点击停用' : '点击启用'}
                      className={`relative w-9 h-5 rounded-full transition-colors shrink-0 ${
                        job.enabled ? 'bg-primary' : 'bg-surface-hover'
                      }`}
                    >
                      <span
                        className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-all ${
                          job.enabled ? 'left-[18px]' : 'left-0.5'
                        }`}
                      />
                    </button>
                    <button
                      onClick={() => removeJob(job.id)}
                      title="删除"
                      className="p-1.5 rounded text-text-secondary/60 hover:text-red-400 hover:bg-background transition-colors shrink-0"
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                ))
              )}
            </div>

            {/* 创建表单 */}
            <div className="border-t border-surface-hover px-3 py-2.5 shrink-0 space-y-2">
              <div className="flex items-center gap-2 flex-wrap">
                <select
                  value={schedType}
                  onChange={(e) => setSchedType(e.target.value as SchedType)}
                  className={inputCls}
                >
                  <option value="once">一次性</option>
                  <option value="daily">每天</option>
                  <option value="weekly">每周</option>
                  <option value="interval">每隔 N 小时</option>
                </select>
                {schedType === 'once' && (
                  <input
                    type="datetime-local"
                    value={onceAt}
                    onChange={(e) => setOnceAt(e.target.value)}
                    className={inputCls}
                  />
                )}
                {schedType === 'daily' && (
                  <input
                    type="time"
                    value={dailyTime}
                    onChange={(e) => setDailyTime(e.target.value)}
                    className={inputCls}
                  />
                )}
                {schedType === 'weekly' && (
                  <>
                    <select
                      value={weeklyDay}
                      onChange={(e) => setWeeklyDay(e.target.value)}
                      className={inputCls}
                    >
                      {WEEKDAYS.map((w, i) => (
                        <option key={i} value={i}>
                          周{w}
                        </option>
                      ))}
                    </select>
                    <input
                      type="time"
                      value={weeklyTime}
                      onChange={(e) => setWeeklyTime(e.target.value)}
                      className={inputCls}
                    />
                  </>
                )}
                {schedType === 'interval' && (
                  <span className="flex items-center gap-1.5 text-[12px] text-text-secondary">
                    每隔
                    <input
                      type="number"
                      min={1}
                      max={23}
                      value={intervalHours}
                      onChange={(e) => setIntervalHours(Number(e.target.value))}
                      className={`${inputCls} w-16`}
                    />
                    小时
                  </span>
                )}
              </div>
              <textarea
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                rows={2}
                placeholder="到点发送给当前会话的提示词…"
                className="w-full bg-background border border-surface-hover rounded-lg px-2.5 py-1.5 text-[12px] text-text-primary outline-none focus:border-primary/50 resize-none placeholder:text-text-secondary/50"
              />
              {manualApproval && (
                <div className="flex items-center gap-1.5 text-[11px] text-amber-400/90">
                  <AlertTriangle size={12} className="shrink-0" />
                  该会话为手动审批，定时触发时写操作将等待你确认
                </div>
              )}
              {formError && <div className="text-[11px] text-red-400">{formError}</div>}
              <div className="flex justify-end">
                <button
                  onClick={handleCreate}
                  disabled={submitting}
                  className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[12px] bg-primary text-white hover:opacity-90 disabled:opacity-40 transition-opacity"
                >
                  {submitting ? <Loader2 size={13} className="animate-spin" /> : <Plus size={13} />}
                  创建任务
                </button>
              </div>
            </div>
          </>
        )}

        {/* ── 后台任务 tab ── */}
        {tab === 'tasks' && (
          <div className="flex-1 min-h-0 overflow-y-auto px-3 py-2">
            {tasks === null ? (
              <div className="flex items-center justify-center h-full text-sm text-text-secondary/60">
                <Loader2 size={15} className="animate-spin mr-2" />加载中…
              </div>
            ) : tasks.length === 0 ? (
              <div className="flex items-center justify-center h-full text-[13px] text-text-secondary/60">
                暂无后台任务
              </div>
            ) : (
              tasks.map((t) => {
                const st = TASK_STATUS[t.status] ?? TASK_STATUS.stopped;
                const exp = expanded[t.id];
                return (
                  <div key={t.id} className="rounded-lg hover:bg-surface-hover">
                    <div
                      className="flex items-center gap-2.5 px-2.5 py-2 cursor-pointer"
                      onClick={() => toggleExpand(t.id)}
                    >
                      <ChevronRight
                        size={13}
                        className={`text-text-secondary/50 shrink-0 transition-transform ${exp ? 'rotate-90' : ''}`}
                      />
                      <div className="flex-1 min-w-0">
                        <div className="truncate text-[12px] text-text-primary" title={t.description}>
                          {t.description || t.id}
                        </div>
                        <div className="flex items-center gap-2 text-[10px] text-text-secondary/50 mt-0.5">
                          <span>{fmtTime(t.created_at)}</span>
                          {t.session_id && <span className="truncate">会话 {t.session_id.slice(0, 8)}</span>}
                          {t.exit_code !== null && <span>exit {t.exit_code}</span>}
                        </div>
                      </div>
                      <span className={`px-1.5 py-0.5 rounded text-[10px] shrink-0 ${st.cls}`}>{st.label}</span>
                      {t.status === 'running' && (
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            stopTask(t.id);
                          }}
                          disabled={stopping.has(t.id)}
                          title="停止任务"
                          className="p-1.5 rounded text-text-secondary/60 hover:text-red-400 hover:bg-background transition-colors shrink-0 disabled:opacity-40"
                        >
                          {stopping.has(t.id) ? (
                            <Loader2 size={13} className="animate-spin" />
                          ) : (
                            <Square size={12} />
                          )}
                        </button>
                      )}
                    </div>
                    {exp && (
                      <div className="px-3 pb-2">
                        {exp.loading ? (
                          <div className="flex items-center gap-2 px-2 py-2 text-[11px] text-text-secondary/60">
                            <Loader2 size={12} className="animate-spin" />读取输出…
                          </div>
                        ) : exp.output ? (
                          <>
                            <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-background border border-surface-hover px-2.5 py-2 text-[11px] leading-relaxed text-text-secondary">
                              {exp.output.content || '（无输出）'}
                            </pre>
                            {exp.output.truncated && (
                              <div className="mt-1 text-[10px] text-amber-400/80">输出已截断</div>
                            )}
                          </>
                        ) : null}
                      </div>
                    )}
                  </div>
                );
              })
            )}
          </div>
        )}
      </div>
    </div>
  );
}
