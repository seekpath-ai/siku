import { useState, useEffect } from 'react';
import { X, Bot, FolderOpen } from 'lucide-react';
import type { ApprovalConfig, LlmProvider } from '@/lib/types';
import { LlmConfigFields } from './LlmConfigFields';
import { ToolPicker } from './ToolPicker';
import { defaultLlmBlock } from '@/lib/llm-presets';
import { DEFAULT_TOOLS } from '@/lib/agent-tools';
import { settingsAppGet, llmProviderList } from '@/lib/tauri';
import { pickDirectory } from '@/lib/pickDirectory';
import { useDialog } from '@/hooks/useDialog';

interface Props {
  onClose: () => void;
  /** Project directory for the default sandbox scope. */
  projectPath?: string;
  onCreate: (input: {
    title: string;
    systemPrompt?: string;
    tools: string[];
    projectId?: string;
    workingDir: string | null;
    visionProviderId: string | null;
    webProxy: string | null;
    llmProviderIds: string[];
    llmModels: { provider: string; model: string; api_key: string; base_url: string }[];
    approvalConfig: ApprovalConfig;
    maxLoops: number;
    /** Per-round output cap; undefined = follow the model config. */
    maxTokens?: number;
    /** Conversation context truncation budget. */
    contextBudget: number;
    maxMemoryRounds: number;
    memoryDir?: string;
    skillsDir?: string;
  }) => Promise<void>;
}

const APPROVAL_OPTIONS: { value: ApprovalConfig['mode']; label: string }[] = [
  { value: 'auto', label: '自动（始终批准）' },
  { value: 'auto_expire_time', label: '时间窗口内自动批准' },
  { value: 'auto_by_rules', label: '白名单自动批准' },
  { value: 'manual', label: '手动（始终询问）' },
  { value: 'manual_all', label: '严格（读写均询问）' },
];

export function AgentCreateDialog({ onClose, onCreate, projectPath }: Props) {
  const { alert } = useDialog();
  const [name, setName] = useState('');
  const [persona, setPersona] = useState('');
  const [tools, setTools] = useState<string[]>(DEFAULT_TOOLS);
  const [providers, setProviders] = useState<LlmProvider[]>([]);
  const [selectedProviderId, setSelectedProviderId] = useState<string>('');
  const [useCustomLlm, setUseCustomLlm] = useState(false);
  const [customLlm, setCustomLlm] = useState(defaultLlmBlock());
  const [workingDirMode, setWorkingDirMode] = useState<'project' | 'full'>('project');
  const [visionProviderId, setVisionProviderId] = useState('');
  const [webProxy, setWebProxy] = useState('');
  const [approvalMode, setApprovalMode] = useState<ApprovalConfig['mode']>('auto');
  const [expireSec, setExpireSec] = useState(60);
  const [whitelist, setWhitelist] = useState('');
  const [maxLoops, setMaxLoops] = useState(10);
  /** Per-round output cap; '' = follow the model config. */
  const [maxTokens, setMaxTokens] = useState('');
  const [contextBudget, setContextBudget] = useState(28000);
  const [maxMemoryRounds, setMaxMemoryRounds] = useState(10);
  const [memoryDir, setMemoryDir] = useState('');
  const [skillsDir, setSkillsDir] = useState('');
  const [creating, setCreating] = useState(false);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    Promise.all([settingsAppGet(), llmProviderList()])
      .then(([s, list]) => {
        setProviders(list);
        setSelectedProviderId(s.default_llm_provider_id ?? list.find((p) => p.is_default)?.id ?? list[0]?.id ?? '');
        setApprovalMode(s.default_approval.mode);
        setExpireSec(s.default_approval.expire_sec ?? 60);
        setWhitelist((s.default_approval.whitelist ?? []).join(', '));
        setMaxLoops(s.default_max_loops);
        setContextBudget(s.default_context_budget);
        setMaxMemoryRounds(s.default_max_memory_rounds);
      })
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  const approvalConfig: ApprovalConfig = {
    mode: approvalMode,
    expire_sec: approvalMode === 'auto_expire_time' || approvalMode === 'manual' ? expireSec : undefined,
    whitelist: approvalMode === 'auto_by_rules' ? whitelist.split(/[,\n]/).map((s) => s.trim()).filter(Boolean) : undefined,
  };

  const handleCreate = async () => {
    if (!name.trim()) return;
    if (!useCustomLlm && !selectedProviderId) {
      await alert('请选择一个模型提供商，或开启自定义 LLM');
      return;
    }
    setCreating(true);
    try {
      await onCreate({
        title: name.trim(),
        systemPrompt: persona.trim() || undefined,
        tools,
        projectId: undefined,
        workingDir: workingDirMode === 'project' ? (projectPath ?? null) : null,
        visionProviderId: visionProviderId || null,
        webProxy: webProxy.trim() || null,
        llmProviderIds: useCustomLlm ? [] : [selectedProviderId],
        llmModels: useCustomLlm ? [customLlm] : [],
        approvalConfig,
        maxLoops,
        maxTokens: maxTokens.trim() ? parseInt(maxTokens, 10) : undefined,
        contextBudget,
        maxMemoryRounds,
        memoryDir: memoryDir.trim() || undefined,
        skillsDir: skillsDir.trim() || undefined,
      });
      onClose();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      await alert(`创建失败: ${msg}`, '创建智能体失败');
    } finally {
      setCreating(false);
    }
  };

  if (loading) {
    return (
      <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
        <div className="p-6 rounded-2xl bg-codex-surface border border-codex-border text-codex-secondary text-sm">
          加载中...
        </div>
      </div>
    );
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      onClick={(e) => e.target === e.currentTarget && onClose()}
    >
      <div className="w-[520px] max-w-[95vw] max-h-[85vh] flex flex-col rounded-2xl bg-codex-surface border border-codex-border shadow-2xl">
        <div className="flex items-center justify-between px-6 py-4 border-b border-codex-border">
          <div className="flex items-center gap-2">
            <Bot size={18} className="text-codex-accent" />
            <h3 className="text-base font-semibold text-codex-primary">新建智能体</h3>
          </div>
          <button onClick={onClose} className="text-codex-muted hover:text-codex-primary">
            <X size={18} />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-6 py-4 space-y-4">
          <div className="space-y-1.5">
            <label className="text-xs text-codex-muted">
              智能体名称 <span className="text-codex-danger">*</span>
            </label>
            <input
              autoFocus
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="例如：开发助手、部署、代码审查"
              className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
            />
          </div>

          <div className="space-y-1.5">
            <label className="text-xs text-codex-muted">角色 / 系统提示词</label>
            <textarea
              value={persona}
              onChange={(e) => setPersona(e.target.value)}
              placeholder="描述此智能体的职责和行为方式..."
              rows={3}
              className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light resize-none"
            />
          </div>

          <div className="space-y-1.5">
            <label className="text-xs text-codex-muted">工具</label>
            <ToolPicker value={tools} onChange={setTools} />
          </div>

          <div className="space-y-2 border-t border-codex-border pt-3">
            <div className="flex items-center justify-between">
              <h4 className="text-xs font-semibold text-codex-muted uppercase tracking-wide">LLM 模型</h4>
              <label className="flex items-center gap-1.5 text-xs text-codex-secondary cursor-pointer">
                <input
                  type="checkbox"
                  checked={useCustomLlm}
                  onChange={(e) => setUseCustomLlm(e.target.checked)}
                  className="rounded border-codex-border bg-codex-bg text-codex-accent"
                />
                自定义
              </label>
            </div>

            {!useCustomLlm ? (
              <select
                value={selectedProviderId}
                onChange={(e) => setSelectedProviderId(e.target.value)}
                className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
              >
                {providers.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name} ({p.provider}/{p.model})
                  </option>
                ))}
              </select>
            ) : (
              <LlmConfigFields
                config={customLlm}
                onChange={(partial) => setCustomLlm((prev) => ({ ...prev, ...partial }))}
                apiKeyOptional
              />
            )}
          </div>

          <div className="space-y-2 border-t border-codex-border pt-3">
            <h4 className="text-xs font-semibold text-codex-muted uppercase tracking-wide">工作目录</h4>
            <select
              value={workingDirMode}
              onChange={(e) => setWorkingDirMode(e.target.value as 'project' | 'full')}
              className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
            >
              <option value="project">项目目录（沙箱，推荐）{projectPath ? `：${projectPath}` : ''}</option>
              <option value="full">全盘访问（无限制）</option>
            </select>
            <p className="text-[11px] text-codex-muted">
              文件工具（file_read / file_write / file_edit / file_grep / file_glob）只能操作所选目录；选择全盘访问则不受限制。
            </p>
          </div>

          <div className="space-y-1.5">
            <label className="text-xs text-codex-muted">网络代理（可选，留空用全局）</label>
            <input
              type="text"
              value={webProxy}
              onChange={(e) => setWebProxy(e.target.value)}
              placeholder="如 http://127.0.0.1:7890"
              className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
            />
            <p className="text-[11px] text-codex-muted">仅用于本智能体的 web_search / web_fetch，不影响全局设置。</p>
          </div>

          <div className="space-y-1.5">
            <label className="text-xs text-codex-muted">多模态模型（可选）</label>
            <select
              value={visionProviderId}
              onChange={(e) => setVisionProviderId(e.target.value)}
              className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
            >
              <option value="">无（不启用图片理解）</option>
              {providers
                .filter((p) => p.is_vision)
                .map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name} ({p.provider}/{p.model})
                  </option>
                ))}
            </select>
            {providers.filter((p) => p.is_vision).length === 0 && (
              <p className="text-[11px] text-codex-muted">
                暂无多模态模型，可在「设置 → 模型提供商」中将模型标记为多模态。
              </p>
            )}
          </div>

          <div className="space-y-2 border-t border-codex-border pt-3">
            <h4 className="text-xs font-semibold text-codex-muted uppercase tracking-wide">审批模式</h4>
            <select
              value={approvalMode}
              onChange={(e) => setApprovalMode(e.target.value as ApprovalConfig['mode'])}
              className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
            >
              {APPROVAL_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
            {approvalMode === 'auto_expire_time' && (
              <div className="space-y-1">
                <label className="text-xs text-codex-muted">过期秒数</label>
                <input
                  type="number"
                  value={expireSec}
                  onChange={(e) => setExpireSec(parseInt(e.target.value) || 0)}
                  className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
                />
              </div>
            )}
            {approvalMode === 'auto_by_rules' && (
              <div className="space-y-1">
                <label className="text-xs text-codex-muted">白名单（逗号或换行分隔）</label>
                <textarea
                  value={whitelist}
                  onChange={(e) => setWhitelist(e.target.value)}
                  placeholder={`例如:\nfile_read\nnote_read\npaper_search`}
                  rows={3}
                  className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light resize-none"
                />
              </div>
            )}
          </div>

          <div className="grid grid-cols-2 sm:grid-cols-4 gap-3 border-t border-codex-border pt-3">
            <div className="space-y-1">
              <label className="text-xs text-codex-muted">最大轮次</label>
              <input
                type="number"
                value={maxLoops}
                onChange={(e) => setMaxLoops(parseInt(e.target.value) || 0)}
                className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
              />
            </div>
            <div className="space-y-1">
              <label className="text-xs text-codex-muted">单轮输出上限</label>
              <input
                type="number"
                value={maxTokens}
                placeholder="跟随模型"
                onChange={(e) => setMaxTokens(e.target.value)}
                className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
              />
            </div>
            <div className="space-y-1">
              <label className="text-xs text-codex-muted">上下文限制</label>
              <input
                type="number"
                value={contextBudget}
                onChange={(e) => setContextBudget(parseInt(e.target.value) || 0)}
                className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
              />
            </div>
            <div className="space-y-1">
              <label className="text-xs text-codex-muted">最大记忆轮数</label>
              <input
                type="number"
                value={maxMemoryRounds}
                onChange={(e) => setMaxMemoryRounds(parseInt(e.target.value) || 0)}
                className="w-full bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
              />
            </div>
          </div>

          <div className="space-y-3 border-t border-codex-border pt-3">
            <h4 className="text-xs font-semibold text-codex-muted uppercase tracking-wide">存储路径</h4>
            <DirField
              label="记忆目录"
              value={memoryDir}
              onChange={setMemoryDir}
              placeholder="留空使用智能体默认值"
            />
            <DirField
              label="技能目录"
              value={skillsDir}
              onChange={setSkillsDir}
              placeholder="留空使用智能体默认值"
            />
          </div>
        </div>

        <div className="flex justify-end gap-3 px-6 py-4 border-t border-codex-border">
          <button
            onClick={onClose}
            className="px-4 py-2 rounded-lg border border-codex-border text-sm text-codex-secondary hover:bg-codex-hover"
          >
            取消
          </button>
          <button
            onClick={handleCreate}
            disabled={creating || !name.trim()}
            className="px-4 py-2 rounded-lg bg-codex-accent text-black text-sm font-semibold hover:bg-codex-accent-hover disabled:opacity-50"
          >
            {creating ? '创建中...' : '创建智能体'}
          </button>
        </div>
      </div>
    </div>
  );
}

function DirField({
  label,
  value,
  onChange,
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  const handlePick = async () => {
    const selected = await pickDirectory(value);
    if (selected) onChange(selected);
  };

  return (
    <div className="space-y-1.5">
      <label className="text-xs text-codex-muted">{label}</label>
      <div className="flex items-center gap-2">
        <input
          type="text"
          value={value}
          placeholder={placeholder}
          onChange={(e) => onChange(e.target.value)}
          className="flex-1 bg-codex-bg border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
        />
        <button
          onClick={handlePick}
          className="flex items-center gap-1 px-2.5 py-2 rounded-lg border border-codex-border text-codex-secondary hover:bg-codex-hover"
          title="选择文件夹"
        >
          <FolderOpen size={14} />
        </button>
      </div>
    </div>
  );
}
