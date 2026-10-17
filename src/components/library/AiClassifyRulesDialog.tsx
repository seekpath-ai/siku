import { useCallback, useEffect, useState } from 'react';
import { X, Loader2, Plus, Trash2, Play, BookMarked, AlertTriangle } from 'lucide-react';
import { useAiClassifyStore } from '@/stores/aiClassifyStore';
import { useCollections } from '@/hooks/useLibrary';
import { useDialog } from '@/hooks/useDialog';
import {
  aiClassifyRuleDelete,
  aiClassifyRuleRun,
  aiClassifyRuleSave,
  aiClassifyRuleSetEnabled,
  aiClassifyRulesList,
  type ClassifyRule,
} from '@/lib/tauri';

const SCHEDULES = [
  { cron: '0 9 * * *', label: '每天 09:00' },
  { cron: '0 20 * * *', label: '每天 20:00' },
  { cron: '0 9 * * 1', label: '每周一 09:00' },
];

const THRESHOLDS = [
  { value: 0.75, label: '标准（0.75）' },
  { value: 0.85, label: '保守（0.85）' },
  { value: 0.9, label: '只信高把握（0.9）' },
];

/** Manage unattended classification rules: new papers get filed on a schedule,
 *  and only assignments above the rule's threshold are written — everything
 *  else waits in the normal review queue. */
export function AiClassifyRulesDialog() {
  const { paperIds, scope, rulesOpen, rulesPrefill, closeRules } = useAiClassifyStore();
  const { alert, confirm } = useDialog();
  const { data: collections } = useCollections();

  const [rules, setRules] = useState<ClassifyRule[]>([]);
  const [loading, setLoading] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  // Prefill for the new-rule form, taken from the dialog that opened it.
  const [dimension, setDimension] = useState('研究主题');
  const [target, setTarget] = useState<'collection' | 'tag'>('collection');
  const [cron, setCron] = useState(SCHEDULES[0].cron);
  const [threshold, setThreshold] = useState(0.85);
  const [scopeCollectionId, setScopeCollectionId] = useState<string>('');
  const [name, setName] = useState('');

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      setRules(await aiClassifyRulesList());
    } catch (err) {
      console.error('load rules:', err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!rulesOpen) return;
    void reload();
    // Opened from a finished run: prefill the form with what that run used.
    if (rulesPrefill) {
      setDimension(rulesPrefill.dimension);
      setTarget(rulesPrefill.target === 'tag' ? 'tag' : 'collection');
      setScopeCollectionId(rulesPrefill.scopeId ?? '');
      setCreating(true);
    }
  }, [rulesOpen, rulesPrefill, reload]);

  const create = async () => {
    if (!dimension.trim()) {
      await alert('请填写分类维度');
      return;
    }
    try {
      await aiClassifyRuleSave(null, {
        name: name.trim() || `${dimension.trim()} 自动归类`,
        dimension: dimension.trim(),
        target,
        scopeKind: scopeCollectionId ? 'collection' : 'all',
        scopeId: scopeCollectionId || null,
        maxGroups: 8,
        threshold,
        twoLevel: true,
        reuseExisting: true,
        cron,
        enabled: true,
        providerId: null,
      });
      setCreating(false);
      setName('');
      await reload();
    } catch (err) {
      await alert(`创建规则失败: ${err}`);
    }
  };

  const toggle = async (rule: ClassifyRule) => {
    setBusyId(rule.id);
    try {
      await aiClassifyRuleSetEnabled(rule.id, !rule.enabled);
      await reload();
    } catch (err) {
      await alert(`更新失败: ${err}`);
    } finally {
      setBusyId(null);
    }
  };

  const runNow = async (rule: ClassifyRule) => {
    setBusyId(rule.id);
    try {
      await aiClassifyRuleRun(rule.id);
      // The run is spawned: give it a moment, then refresh the summary column.
      setTimeout(() => void reload(), 4000);
    } catch (err) {
      await alert(`启动失败: ${err}`);
    } finally {
      setBusyId(null);
    }
  };

  const remove = async (rule: ClassifyRule) => {
    if (!(await confirm(`删除规则「${rule.name}」？已写入的分类不受影响。`))) return;
    setBusyId(rule.id);
    try {
      await aiClassifyRuleDelete(rule.id);
      await reload();
    } catch (err) {
      await alert(`删除失败: ${err}`);
    } finally {
      setBusyId(null);
    }
  };

  if (!rulesOpen) return null;

  return (
    <div className="fixed inset-0 z-[6000] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={closeRules} />
      <div className="relative w-[92vw] max-w-[900px] h-[80vh] mx-4 flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-2 px-4 py-2.5 border-b border-surface-hover shrink-0">
          <BookMarked size={15} className="text-primary" />
          <span className="text-sm font-medium text-text-primary">AI 自动分类规则</span>
          <span className="text-[11px] text-text-secondary/60">
            {rules.filter((r) => r.enabled).length} 条启用
          </span>
          <div className="ml-auto flex items-center gap-2">
            <button
              onClick={() => setCreating((v) => !v)}
              className="flex items-center gap-1 px-2.5 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
            >
              <Plus size={12} />
              新建规则
            </button>
            <button
              onClick={closeRules}
              className="p-1.5 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover"
            >
              <X size={15} />
            </button>
          </div>
        </div>

        <p className="px-4 py-2 text-[11px] text-text-secondary/55 leading-snug border-b border-surface-hover/60">
          规则按计划只处理<strong className="text-text-secondary/80">新导入、且尚未按该维度分类</strong>的文献（最多 500 篇/次），
          并且只写入置信度高于门槛的判定——其余仍进「待审」。程序未运行时不会触发。
        </p>

        {creating && (
          <div className="px-4 py-3 border-b border-surface-hover bg-surface-hover/20 space-y-2.5 shrink-0">
            <div className="grid grid-cols-2 gap-2.5">
              <label className="block">
                <span className="block text-[11px] text-text-secondary/60 mb-1">规则名</span>
                <input
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  placeholder={`${dimension} 自动归类`}
                  className="w-full bg-background border border-surface-hover rounded px-2 py-1 text-[12px] text-text-primary outline-none focus:border-primary/50"
                />
              </label>
              <label className="block">
                <span className="block text-[11px] text-text-secondary/60 mb-1">分类维度</span>
                <input
                  value={dimension}
                  onChange={(e) => setDimension(e.target.value)}
                  className="w-full bg-background border border-surface-hover rounded px-2 py-1 text-[12px] text-text-primary outline-none focus:border-primary/50"
                />
              </label>
            </div>
            <div className="grid grid-cols-2 gap-2.5">
              <label className="block">
                <span className="block text-[11px] text-text-secondary/60 mb-1">范围</span>
                <select
                  value={scopeCollectionId}
                  onChange={(e) => setScopeCollectionId(e.target.value)}
                  className="w-full bg-background border border-surface-hover rounded px-2 py-1 text-[12px] text-text-primary outline-none"
                >
                  <option value="">整个图书馆</option>
                  {(collections ?? []).map((c) => (
                    <option key={c.id} value={c.id}>
                      {c.name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="block">
                <span className="block text-[11px] text-text-secondary/60 mb-1">写入到</span>
                <select
                  value={target}
                  onChange={(e) => setTarget(e.target.value as 'collection' | 'tag')}
                  className="w-full bg-background border border-surface-hover rounded px-2 py-1 text-[12px] text-text-primary outline-none"
                >
                  <option value="collection">集合（分类）</option>
                  <option value="tag">标签</option>
                </select>
              </label>
            </div>
            <div className="flex flex-wrap items-center gap-1.5">
              {SCHEDULES.map((s) => (
                <button
                  key={s.cron}
                  onClick={() => setCron(s.cron)}
                  className={`px-2 py-0.5 rounded border text-[11px] ${
                    cron === s.cron
                      ? 'border-primary/50 bg-primary/10 text-primary'
                      : 'border-surface-hover text-text-secondary hover:bg-surface-hover'
                  }`}
                >
                  {s.label}
                </button>
              ))}
              <span className="mx-1 text-text-secondary/30">|</span>
              {THRESHOLDS.map((t) => (
                <button
                  key={t.value}
                  onClick={() => setThreshold(t.value)}
                  className={`px-2 py-0.5 rounded border text-[11px] ${
                    threshold === t.value
                      ? 'border-primary/50 bg-primary/10 text-primary'
                      : 'border-surface-hover text-text-secondary hover:bg-surface-hover'
                  }`}
                >
                  {t.label}
                </button>
              ))}
            </div>
            <div className="flex items-center gap-2">
              <button
                onClick={() => void create()}
                className="px-3 py-1.5 rounded-lg bg-primary text-black text-[12px] font-medium hover:bg-primary/90"
              >
                创建
              </button>
              <button
                onClick={() => setCreating(false)}
                className="px-3 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
              >
                取消
              </button>
              <span className="text-[11px] text-text-secondary/50">
                预填自 {scope.label} · {paperIds.length} 篇当前选择
              </span>
            </div>
          </div>
        )}

        <div className="flex-1 min-h-0 overflow-y-auto p-3 space-y-2">
          {loading && rules.length === 0 && (
            <div className="flex items-center justify-center py-12 text-text-secondary/60">
              <Loader2 size={18} className="animate-spin" />
            </div>
          )}
          {!loading && rules.length === 0 && (
            <div className="flex flex-col items-center justify-center py-12 text-text-secondary/60 gap-2">
              <AlertTriangle size={22} className="opacity-50" />
              <p className="text-[12px]">还没有自动分类规则</p>
              <p className="text-[11px] opacity-60">
                在「AI 文献分类」里应用一次分类后可以「存为自动规则」，也可以点右上角新建。
              </p>
            </div>
          )}
          {rules.map((rule) => (
            <div
              key={rule.id}
              className="rounded-lg border border-surface-hover/70 p-2.5 space-y-1.5"
            >
              <div className="flex items-center gap-2">
                <span className="text-[12px] font-medium text-text-primary truncate">
                  {rule.name}
                </span>
                <span className="px-1.5 py-0.5 rounded bg-primary/15 text-primary text-[10px] shrink-0">
                  {rule.dimension}
                </span>
                <span className="px-1.5 py-0.5 rounded bg-surface-hover text-text-secondary text-[10px] shrink-0">
                  {rule.target === 'tag' ? '标签' : '集合'}
                </span>
                <span className="text-[10px] text-text-secondary/60 shrink-0">
                  {SCHEDULES.find((s) => s.cron === rule.cron)?.label ?? rule.cron}
                </span>
                <span className="text-[10px] text-text-secondary/50 shrink-0">
                  门槛 {Math.round(rule.threshold * 100)}%
                </span>
                <div className="ml-auto flex items-center gap-1.5">
                  <button
                    onClick={() => void toggle(rule)}
                    disabled={busyId === rule.id}
                    className={`px-2 py-0.5 rounded text-[11px] ${
                      rule.enabled
                        ? 'bg-emerald-500/15 text-emerald-400'
                        : 'bg-surface-hover text-text-secondary'
                    }`}
                  >
                    {rule.enabled ? '已启用' : '已停用'}
                  </button>
                  <button
                    onClick={() => void runNow(rule)}
                    disabled={busyId === rule.id}
                    className="flex items-center gap-0.5 px-2 py-0.5 rounded text-[11px] text-text-secondary hover:bg-surface-hover disabled:opacity-40"
                  >
                    {busyId === rule.id ? (
                      <Loader2 size={10} className="animate-spin" />
                    ) : (
                      <Play size={10} />
                    )}
                    立即运行
                  </button>
                  <button
                    onClick={() => void remove(rule)}
                    disabled={busyId === rule.id}
                    className="p-1 rounded text-text-secondary/60 hover:text-red-400 hover:bg-surface-hover disabled:opacity-40"
                  >
                    <Trash2 size={11} />
                  </button>
                </div>
              </div>
              <p className="text-[11px] text-text-secondary/55 leading-snug">
                {rule.scopeKind === 'collection'
                  ? `范围：集合「${
                      (collections ?? []).find((c) => c.id === rule.scopeId)?.name ?? rule.scopeId ?? '未知'
                    }」`
                  : '范围：整个图书馆'}
                {rule.lastRunAt ? ` · 上次运行 ${isoShort(rule.lastRunAt)}` : ' · 尚未运行'}
              </p>
              {rule.lastResult && (
                <p className="text-[11px] text-text-secondary/70 leading-snug">{rule.lastResult}</p>
              )}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

function isoShort(iso: string): string {
  // "2026-01-02T03:04:05Z" → "01-02 03:04"
  const m = iso.match(/^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})/);
  return m ? `${m[2]}-${m[3]} ${m[4]}:${m[5]}` : iso;
}
