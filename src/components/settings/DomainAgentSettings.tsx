import { useEffect, useMemo, useState } from 'react';
import { Loader2, Check, RotateCcw, AlertTriangle, Bot } from 'lucide-react';
import {
  settingsSet,
  taskPromptGroups,
  taskPromptsList,
  type TaskPromptGroup,
  type TaskPromptInfo,
} from '@/lib/tauri';

/** Settings page section: the system prompts behind the batch features
 *  (AI 综述 / AI 表格列 / AI 文献分类 / 科研追踪). These used to be hard-coded
 *  inside the Rust pipelines — this page is the only place they can be seen and
 *  tuned. The pet-ball agents keep their own page (宠物). */
export function DomainAgentSettings() {
  const [prompts, setPrompts] = useState<TaskPromptInfo[]>([]);
  const [groupsMeta, setGroupsMeta] = useState<TaskPromptGroup[]>([]);
  const [budgets, setBudgets] = useState<Record<string, string>>({});
  const [savingBudget, setSavingBudget] = useState<string | null>(null);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(true);
  const [savingKey, setSavingKey] = useState<string | null>(null);
  const [savedKey, setSavedKey] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [list, groups] = await Promise.all([taskPromptsList(), taskPromptGroups()]);
        if (cancelled) return;
        setPrompts(list);
        setGroupsMeta(groups);
        const nextBudgets: Record<string, string> = {};
        for (const g of groups) {
          nextBudgets[g.id] = g.overrideValue != null ? String(g.overrideValue) : '';
        }
        setBudgets(nextBudgets);
        // The textarea holds the override only; empty = use the default.
        const next: Record<string, string> = {};
        for (const p of list) {
          next[p.id] = p.overridden ? p.effectivePrompt : '';
        }
        setDrafts(next);
      } catch (e) {
        console.error('load task prompts:', e);
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const groups = useMemo(() => {
    const map = new Map<string, TaskPromptInfo[]>();
    for (const p of prompts) {
      const list = map.get(p.group) ?? [];
      list.push(p);
      map.set(p.group, list);
    }
    return [...map.entries()];
  }, [prompts]);

  const saveBudget = async (g: TaskPromptGroup) => {
    setSavingBudget(g.id);
    try {
      const raw = (budgets[g.id] ?? '').trim();
      await settingsSet(`budget.${g.id}`, raw);
      const groups = await taskPromptGroups();
      setGroupsMeta(groups);
      const next: Record<string, string> = {};
      for (const x of groups) next[x.id] = x.overrideValue != null ? String(x.overrideValue) : '';
      setBudgets(next);
    } catch (e) {
      console.error('save budget:', e);
    } finally {
      setSavingBudget(null);
    }
  };

  const save = async (p: TaskPromptInfo) => {
    setSavingKey(p.id);
    try {
      await settingsSet(`prompt.${p.id}`, (drafts[p.id] ?? '').trim());
      setPrompts((prev) =>
        prev.map((x) =>
          x.id === p.id
            ? {
                ...x,
                overridden: Boolean((drafts[p.id] ?? '').trim()),
                effectivePrompt: (drafts[p.id] ?? '').trim() || x.defaultPrompt,
              }
            : x
        )
      );
      setSavedKey(p.id);
      setTimeout(() => setSavedKey((k) => (k === p.id ? null : k)), 1600);
    } catch (e) {
      console.error('save prompt:', e);
    } finally {
      setSavingKey(null);
    }
  };

  const reset = async (p: TaskPromptInfo) => {
    setDrafts((prev) => ({ ...prev, [p.id]: '' }));
    setSavingKey(p.id);
    try {
      await settingsSet(`prompt.${p.id}`, '');
      setPrompts((prev) =>
        prev.map((x) =>
          x.id === p.id ? { ...x, overridden: false, effectivePrompt: x.defaultPrompt } : x
        )
      );
    } catch (e) {
      console.error('reset prompt:', e);
    } finally {
      setSavingKey(null);
    }
  };

  return (
    <div className="max-w-[900px] space-y-8">
      <div>
        <h2 className="text-base font-semibold text-text-primary">领域智能体</h2>
        <p className="text-[11px] text-amber-400/80 mt-2 leading-relaxed flex items-start gap-1.5">
          <AlertTriangle size={12} className="mt-0.5 shrink-0" />
          标有「影响解析」的提示词里包含 JSON 输出约定；删改它会让该功能解析失败（多数功能会降级而不是丢数据，但会消耗额度）。改坏了用「恢复默认」。
        </p>
      </div>

      {loading && (
        <div className="flex items-center justify-center py-10 text-text-secondary/60">
          <Loader2 size={18} className="animate-spin" />
        </div>
      )}

      {groups.map(([group, list]) => (
        <section key={group} className="space-y-3">
          <h3 className="text-sm font-medium text-text-primary flex items-center gap-1.5">
            <Bot size={14} className="text-primary" />
            {group}
          </h3>
          {(() => {
            const meta = groupsMeta.find((g) => g.name === group);
            if (!meta) return null;
            const draft = budgets[meta.id] ?? '';
            const dirty = draft.trim() !== (meta.overrideValue != null ? String(meta.overrideValue) : '');
            return (
              <div className="rounded-lg border border-surface-hover bg-surface-hover/20 px-4 py-3 space-y-1.5">
                <label className="flex flex-wrap items-center gap-2 text-[12px] text-text-primary">
                  单次调用词元输出上限
                  <input
                    type="number"
                    min={256}
                    max={32768}
                    step={1024}
                    value={draft}
                    onChange={(e) => setBudgets((prev) => ({ ...prev, [meta.id]: e.target.value }))}
                    placeholder={`自动（约 ${meta.builtinDefault}）`}
                    className="w-32 bg-background border border-surface-hover rounded px-2 py-1 text-[12px] text-text-primary outline-none focus:border-primary/50"
                  />
                  <button
                    onClick={() => void saveBudget(meta)}
                    disabled={!dirty || savingBudget === meta.id}
                    className="px-2.5 py-1 rounded-lg bg-primary text-black text-[11px] font-medium hover:bg-primary/90 disabled:opacity-40"
                  >
                    {savingBudget === meta.id ? <Loader2 size={11} className="animate-spin" /> : '保存'}
                  </button>
                  <span className="text-[11px] text-text-secondary/50">
                    当前生效 {meta.effective}
                  </span>
                </label>
              </div>
            );
          })()}
          {list.map((p) => {
            const draft = drafts[p.id] ?? '';
            const overriddenNow = Boolean(draft.trim());
            const missingJson = p.affectsParsing && overriddenNow && !/json|JSON|\{/.test(draft);
            return (
              <div key={p.id} className="rounded-lg border border-surface-hover bg-surface-hover/20 p-4 space-y-2.5">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-[13px] font-medium text-text-primary">{p.name}</span>
                  <span className="px-1.5 py-0.5 rounded bg-surface-hover text-[10px] text-text-secondary/70">
                    {p.description}
                  </span>
                  {p.affectsParsing && (
                    <span className="px-1.5 py-0.5 rounded bg-amber-500/15 text-[10px] text-amber-400">
                      影响解析
                    </span>
                  )}
                  {overriddenNow && (
                    <span className="px-1.5 py-0.5 rounded bg-primary/15 text-[10px] text-primary">
                      已自定义
                    </span>
                  )}
                </div>

                <textarea
                  value={draft}
                  onChange={(e) => setDrafts((prev) => ({ ...prev, [p.id]: e.target.value }))}
                  placeholder={p.defaultPrompt}
                  rows={Math.min(12, Math.max(4, Math.ceil(p.defaultPrompt.length / 80)))}
                  className="w-full bg-background border border-surface-hover rounded px-3 py-2 text-[12px] leading-relaxed text-text-primary outline-none focus:border-primary/50 resize-y font-mono"
                />

                {p.placeholders.length > 0 && (
                  <div className="flex flex-wrap items-center gap-1.5 text-[10px] text-text-secondary/60">
                    <span>可用占位符：</span>
                    {p.placeholders.map((ph) => (
                      <code
                        key={ph.name}
                        title={ph.description}
                        className="px-1.5 py-0.5 rounded bg-surface-hover text-text-secondary/80"
                      >
                        {`{${ph.name}}`}
                      </code>
                    ))}
                  </div>
                )}

                {missingJson && (
                  <p className="text-[11px] text-amber-400 flex items-center gap-1.5">
                    <AlertTriangle size={11} />
                    这段提示词里看不到 JSON 输出约定，保存后该功能可能解析失败。
                  </p>
                )}

                <div className="flex items-center gap-2">
                  <button
                    onClick={() => void save(p)}
                    disabled={savingKey === p.id}
                    className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-black text-[12px] font-medium hover:bg-primary/90 disabled:opacity-50"
                  >
                    {savingKey === p.id ? (
                      <Loader2 size={12} className="animate-spin" />
                    ) : savedKey === p.id ? (
                      <Check size={12} />
                    ) : null}
                    保存
                  </button>
                  <button
                    onClick={() => void reset(p)}
                    disabled={savingKey === p.id || !draft}
                    className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover disabled:opacity-40"
                  >
                    <RotateCcw size={12} />
                    恢复默认
                  </button>
                  {p.overridden && (
                    <span className="text-[11px] text-text-secondary/50">
                      当前使用自定义提示词（{p.effectivePrompt.length} 字）
                    </span>
                  )}
                </div>
              </div>
            );
          })}
        </section>
      ))}
    </div>
  );
}
