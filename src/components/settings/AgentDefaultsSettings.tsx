import { useEffect, useState } from 'react';
import { Loader2, FolderOpen } from 'lucide-react';
import { settingsAppGet, settingsAppSave, llmProviderList, settingsGetDataDir } from '@/lib/tauri';
import { pickDirectory } from '@/lib/pickDirectory';
import { Combobox } from '@/components/ui/Combobox';
import { SaveButton } from '@/components/ui/SaveButton';
import type { ApprovalConfig, LlmProvider } from '@/lib/types';

const APPROVAL_OPTIONS: { value: ApprovalConfig['mode']; label: string }[] = [
  { value: 'auto', label: '始终自动批准' },
  { value: 'auto_expire_time', label: '时间窗口内自动批准' },
  { value: 'auto_by_rules', label: '白名单自动批准' },
  { value: 'manual', label: '手动（始终询问）' },
  { value: 'manual_all', label: '严格（读写均询问）' },
];

export function AgentDefaultsSettings() {
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [providers, setProviders] = useState<LlmProvider[]>([]);
  const [defaultProviderId, setDefaultProviderId] = useState<string>('');
  const [approval, setApproval] = useState<ApprovalConfig>({ mode: 'auto' });
  const [maxLoops, setMaxLoops] = useState(10);
  const [contextBudget, setContextBudget] = useState(28000);
  const [maxMemoryRounds, setMaxMemoryRounds] = useState(10);
  const [memoryDir, setMemoryDir] = useState('');
  const [skillsDir, setSkillsDir] = useState('');
  const [actualDataDir, setActualDataDir] = useState('');
  const [dataDir, setDataDir] = useState<string | null>(null);

  useEffect(() => {
    Promise.all([settingsAppGet(), llmProviderList(), settingsGetDataDir()])
      .then(([s, list, actual]) => {
        setDefaultProviderId(
          s.default_llm_provider_id && list.some((p) => p.id === s.default_llm_provider_id)
            ? s.default_llm_provider_id
            : (list.find((p) => p.is_default)?.id ?? '')
        );
        setApproval({ ...s.default_approval });
        setMaxLoops(s.default_max_loops);
        setContextBudget(s.default_context_budget);
        setMaxMemoryRounds(s.default_max_memory_rounds);
        setMemoryDir(s.memory_dir ?? '');
        setSkillsDir(s.skills_dir ?? '');
        setDataDir(s.data_dir ?? null);
        setActualDataDir(actual);
        setProviders(list);
      })
      .catch((err) => console.error('Failed to load agent defaults:', err))
      .finally(() => setLoading(false));
  }, []);

  const baseDir = dataDir || actualDataDir;
  const memoryPlaceholder = memoryDir || (baseDir ? `${baseDir}/memory` : '');
  const skillsPlaceholder = skillsDir || (baseDir ? `${baseDir}/skills` : '');

  const handleSave = async () => {
    setSaving(true);
    try {
      const current = await settingsAppGet();
      await settingsAppSave({
        ...current,
        default_llm_provider_id: defaultProviderId || null,
        default_approval: approval,
        default_max_loops: maxLoops,
        default_context_budget: contextBudget,
        default_max_memory_rounds: maxMemoryRounds,
        memory_dir: memoryDir || null,
        skills_dir: skillsDir || null,
      });
      setSaved(true);
    } catch (err) {
      console.error('Failed to save app settings:', err);
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-sm text-text-secondary">
        <Loader2 size={14} className="animate-spin" /> 加载中...
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <h2 className="text-lg font-semibold text-text-primary">智能体默认值</h2>

      <div className="space-y-1.5">
        <label className="block text-sm text-text-secondary">默认模型提供商</label>
        <Combobox
          value={defaultProviderId}
          onChange={(v) => setDefaultProviderId(v)}
          options={[
            { value: '', label: '使用第一个可用提供商' },
            ...providers.map((p) => ({
              value: p.id,
              label: `${p.name} (${p.provider}/${p.model})`,
            })),
          ]}
          placeholder="选择默认模型提供商"
        />
        <p className="text-xs text-text-secondary/60">
          在「模型提供商」页面添加或管理配置。
        </p>
      </div>

      <div className="grid grid-cols-3 gap-4">
        <div className="space-y-2">
          <label className="block text-sm text-text-secondary">最大轮次</label>
          <input
            type="number"
            value={maxLoops}
            onChange={(e) => setMaxLoops(parseInt(e.target.value) || 0)}
            className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
          />
          <p className="text-xs text-text-secondary/60">0 表示不限制（硬上限 1000）</p>
        </div>
        <div className="space-y-2">
          <label className="block text-sm text-text-secondary">上下文限制</label>
          <input
            type="number"
            value={contextBudget}
            onChange={(e) => setContextBudget(parseInt(e.target.value) || 0)}
            className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
          />
          <p className="text-xs text-text-secondary/60">0 表示不截断</p>
        </div>
        <div className="space-y-2">
          <label className="block text-sm text-text-secondary">最大记忆轮数</label>
          <input
            type="number"
            value={maxMemoryRounds}
            onChange={(e) => setMaxMemoryRounds(parseInt(e.target.value) || 0)}
            className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
          />
          <p className="text-xs text-text-secondary/60">0 表示加载全部</p>
        </div>
      </div>

      <DirField
        label="默认记忆目录"
        value={memoryDir}
        placeholder={memoryPlaceholder}
        onChange={setMemoryDir}
      />
      <DirField
        label="默认技能目录"
        value={skillsDir}
        placeholder={skillsPlaceholder}
        onChange={setSkillsDir}
      />

      <div className="space-y-3">
        <label className="block text-sm text-text-secondary">默认审批策略</label>
        <select
          value={approval.mode}
          onChange={(e) => setApproval({ mode: e.target.value as ApprovalConfig['mode'] })}
          className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
        >
          {APPROVAL_OPTIONS.map((opt) => (
            <option key={opt.value} value={opt.value}>
              {opt.label}
            </option>
          ))}
        </select>
        {approval.mode === 'auto_expire_time' && (
          <div className="space-y-2">
            <label className="block text-sm text-text-secondary">过期秒数</label>
            <input
              type="number"
              value={approval.expire_sec ?? 60}
              onChange={(e) => setApproval((prev) => ({ ...prev, expire_sec: parseInt(e.target.value) || 0 }))}
              className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
            />
          </div>
        )}
        {approval.mode === 'auto_by_rules' && (
          <div className="space-y-2">
            <label className="block text-sm text-text-secondary">白名单（逗号或换行分隔）</label>
            <textarea
              value={(approval.whitelist || []).join('\n')}
              onChange={(e) =>
                setApproval((prev) => ({
                  ...prev,
                  whitelist: e.target.value.split(/[,\n]/).map((s) => s.trim()).filter(Boolean),
                }))
              }
              placeholder={`例如:\nfile_read\nnote_read\npaper_search`}
              rows={3}
              className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary resize-none"
            />
          </div>
        )}
      </div>

      <SaveButton saving={saving} saved={saved} onClick={handleSave} />
    </div>
  );
}

function DirField({
  label,
  value,
  placeholder,
  onChange,
}: {
  label: string;
  value: string;
  placeholder: string;
  onChange: (v: string) => void;
}) {
  const handlePick = async () => {
    const selected = await pickDirectory(value || placeholder);
    if (selected) onChange(selected);
  };

  return (
    <div className="space-y-1.5">
      <label className="block text-sm text-text-secondary">{label}</label>
      <div className="flex items-center gap-2">
        <input
          type="text"
          value={value}
          placeholder={placeholder}
          onChange={(e) => onChange(e.target.value)}
          className="flex-1 bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
        />
        <button
          onClick={handlePick}
          className="p-2.5 rounded-lg bg-surface border border-surface-hover text-text-secondary hover:bg-surface-hover shrink-0"
          title="选择文件夹"
        >
          <FolderOpen size={16} />
        </button>
      </div>
    </div>
  );
}
