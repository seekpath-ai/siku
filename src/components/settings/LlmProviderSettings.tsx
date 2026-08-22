import { useState, useEffect, useCallback } from 'react';
import { Trash2, Edit2, X, Star, Loader2, CheckCircle } from 'lucide-react';
import { useDialog } from '@/hooks/useDialog';
import type { LlmProvider } from '@/lib/types';
import {
  llmProviderList,
  llmProviderCreate,
  llmProviderUpdate,
  llmProviderDelete,
  llmProviderSetDefault,
  llmProviderValidate,
} from '@/lib/tauri';
import { Combobox } from '@/components/ui/Combobox';
import { SaveButton } from '@/components/ui/SaveButton';
import { LLM_PRESETS, defaultLlmBlock, findPreset } from '@/lib/llm-presets';

function emptyForm(): Partial<LlmProvider> & { provider: string } {
  const block = defaultLlmBlock();
  return {
    name: '',
    provider: block.provider,
    model: block.model,
    api_key: '',
    base_url: block.base_url,
    proxy: '',
    max_tokens: 4096,
    temperature: 0.7,
    extra_body: '',
    is_vision: false,
  };
}

export function LlmProviderSettings() {
  const { alert, confirm } = useDialog();
  const [providers, setProviders] = useState<LlmProvider[]>([]);
  const [loading, setLoading] = useState(true);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState(emptyForm());
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [validatingId, setValidatingId] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const list = await llmProviderList();
      setProviders(list);
    } catch (err) {
      console.error('Failed to load LLM providers:', err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const resetForm = () => {
    setEditingId(null);
    setForm(emptyForm());
  };

  const startEdit = (p: LlmProvider) => {
    setEditingId(p.id);
    setForm({
      name: p.name,
      provider: p.provider,
      model: p.model,
      api_key: p.api_key,
      base_url: p.base_url,
      proxy: p.proxy ?? '',
      max_tokens: p.max_tokens ?? 4096,
      temperature: p.temperature ?? 0.7,
      extra_body: p.extra_body ?? '',
      is_vision: p.is_vision ?? false,
    });
  };

  const updateProvider = (provider: string) => {
    const preset = findPreset(provider);
    setForm((prev) => ({
      ...prev,
      provider,
      base_url: preset?.baseURL || prev.base_url,
      model: preset?.models[0] || prev.model,
    }));
  };

  const handleSave = async () => {
    if (!form.name?.trim()) {
      await alert('请输入配置名称');
      return;
    }
    if (!form.model?.trim()) {
      await alert('请输入模型名称');
      return;
    }

    setSaving(true);
    try {
      const input = {
        name: form.name.trim(),
        provider: form.provider.trim(),
        model: form.model.trim(),
        api_key: form.api_key?.trim() ?? '',
        base_url: form.base_url?.trim() || findPreset(form.provider)?.baseURL || '',
        proxy: form.proxy?.trim() || null,
        max_tokens: form.max_tokens ?? 4096,
        temperature: form.temperature ?? 0.7,
        extra_body: form.extra_body?.trim() || null,
        is_default: editingId ? undefined : providers.length === 0,
        is_vision: form.is_vision ?? false,
      };

      if (editingId) {
        await llmProviderUpdate(editingId, input);
      } else {
        await llmProviderCreate(input);
      }
      await load();
      resetForm();
      setSaved(true);
    } catch (err) {
      await alert(`保存失败: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (p: LlmProvider) => {
    const ok = await confirm(`确定删除「${p.name}」？`, '删除提供商');
    if (!ok) return;
    try {
      await llmProviderDelete(p.id);
      await load();
      if (editingId === p.id) resetForm();
    } catch (err) {
      await alert(`删除失败: ${err}`);
    }
  };

  const handleSetDefault = async (p: LlmProvider) => {
    try {
      await llmProviderSetDefault(p.id);
      await load();
    } catch (err) {
      await alert(`设置默认失败: ${err}`);
    }
  };

  const handleValidate = async (p: LlmProvider) => {
    setValidatingId(p.id);
    try {
      await llmProviderValidate(p.id);
      await alert('连接成功', '测试连接');
    } catch (err) {
      await alert(`连接失败: ${err}`, '测试连接');
    } finally {
      setValidatingId(null);
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
      <h2 className="text-lg font-semibold text-text-primary">模型提供商</h2>
      <p className="text-xs text-text-secondary">
        统一管理 LLM 配置。翻译、RAG、PDF 区域检测和智能体都可以从这里选择模型。
      </p>

      <div className="space-y-3">
        {providers.map((p) => (
          <div
            key={p.id}
            className={`flex items-center gap-3 p-3 rounded-lg border ${
              p.is_default
                ? 'border-primary/50 bg-primary/5'
                : 'border-surface-hover bg-surface/30'
            }`}
          >
            <button
              onClick={() => handleSetDefault(p)}
              className={`shrink-0 ${p.is_default ? 'text-yellow-400' : 'text-text-secondary/40 hover:text-yellow-400'}`}
              title={p.is_default ? '默认配置' : '设为默认'}
            >
              <Star size={16} fill={p.is_default ? 'currentColor' : 'none'} />
            </button>
            <div className="flex-1 min-w-0">
              <div className="text-sm font-medium text-text-primary truncate">{p.name}</div>
              <div className="text-xs text-text-secondary truncate">
                {p.provider} / {p.model}
              </div>
            </div>
            <div className="flex items-center gap-1">
              <button
                onClick={() => handleValidate(p)}
                disabled={validatingId === p.id}
                className="p-1.5 rounded text-text-secondary hover:text-accent hover:bg-surface-hover"
                title="测试连接"
              >
                {validatingId === p.id ? <Loader2 size={14} className="animate-spin" /> : <CheckCircle size={14} />}
              </button>
              <button
                onClick={() => startEdit(p)}
                className="p-1.5 rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover"
                title="编辑"
              >
                <Edit2 size={14} />
              </button>
              <button
                onClick={() => handleDelete(p)}
                className="p-1.5 rounded text-text-secondary hover:text-red-400 hover:bg-red-500/10"
                title="删除"
              >
                <Trash2 size={14} />
              </button>
            </div>
          </div>
        ))}

        {providers.length === 0 && (
          <div className="text-sm text-text-secondary/60 py-4 text-center border border-dashed border-surface-hover rounded-lg">
            暂无模型提供商，请添加一个。
          </div>
        )}
      </div>

      <div className="border border-surface-hover rounded-lg p-4 space-y-4 bg-surface/20">
        <h3 className="text-sm font-medium text-text-secondary">
          {editingId ? '编辑提供商' : '添加提供商'}
        </h3>

        {/* Row 1: 名称全宽 */}
        <div className="space-y-1.5">
          <label className="text-xs text-text-secondary">名称</label>
          <input
            value={form.name}
            onChange={(e) => setForm((prev) => ({ ...prev, name: e.target.value }))}
            placeholder="例如：DeepSeek 主账号"
            className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
          />
        </div>

        {/* Row 2: Provider + Model */}
        <div className="grid grid-cols-2 gap-4">
          <div className="space-y-1.5">
            <label className="text-xs text-text-secondary">Provider</label>
            <Combobox
              value={form.provider}
              onChange={(v) => updateProvider(v)}
              options={LLM_PRESETS.map((p) => ({ value: p.provider, label: p.label }))}
              placeholder="选择或输入 provider"
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-xs text-text-secondary">Model</label>
            <Combobox
              value={form.model ?? ''}
              onChange={(v) => setForm((prev) => ({ ...prev, model: v }))}
              options={findPreset(form.provider)?.models || []}
              placeholder="选择或输入模型"
            />
          </div>
        </div>

        {/* Row 3: API Key 全宽 */}
        <div className="space-y-1.5">
          <label className="text-xs text-text-secondary">API Key</label>
          <input
            type="password"
            value={form.api_key}
            onChange={(e) => setForm((prev) => ({ ...prev, api_key: e.target.value }))}
            placeholder={form.provider.toLowerCase() === 'ollama' ? '本地无需 API Key' : '输入 API Key'}
            className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
          />
        </div>

        {/* Base URL 全宽 */}
        <div className="space-y-1.5">
          <label className="text-xs text-text-secondary">Base URL</label>
          <input
            value={form.base_url}
            onChange={(e) => setForm((prev) => ({ ...prev, base_url: e.target.value }))}
            placeholder="https://api.example.com/v1"
            className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
          />
        </div>

        {/* Row 4: HTTP Proxy + Max Tokens */}
        <div className="grid grid-cols-2 gap-4">
          <div className="space-y-1.5">
            <label className="text-xs text-text-secondary">HTTP Proxy（可选，留空直连）</label>
            <input
              value={form.proxy ?? ''}
              onChange={(e) => setForm((prev) => ({ ...prev, proxy: e.target.value }))}
              placeholder="如 http://127.0.0.1:7890"
              className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-xs text-text-secondary">Max Tokens</label>
            <input
              type="number"
              value={form.max_tokens ?? 4096}
              onChange={(e) => setForm((prev) => ({ ...prev, max_tokens: parseInt(e.target.value) || 0 }))}
              className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
            />
          </div>
        </div>

        {/* Row 5: Temperature + Extra Body */}
        <div className="grid grid-cols-2 gap-4">
          <div className="space-y-1.5">
            <label className="text-xs text-text-secondary">Temperature</label>
            <input
              type="number"
              step="0.1"
              min="0"
              max="2"
              value={form.temperature ?? 0.7}
              onChange={(e) => setForm((prev) => ({ ...prev, temperature: parseFloat(e.target.value) }))}
              className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-xs text-text-secondary">Extra Body (JSON)</label>
            <input
              value={form.extra_body ?? ''}
              onChange={(e) => setForm((prev) => ({ ...prev, extra_body: e.target.value }))}
              placeholder='{"top_p": 0.9}'
              className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
            />
          </div>
        </div>

        <label className="flex items-center gap-2 text-sm text-text-secondary cursor-pointer select-none">
          <input
            type="checkbox"
            checked={form.is_vision ?? false}
            onChange={(e) => setForm((prev) => ({ ...prev, is_vision: e.target.checked }))}
            className="rounded border-surface-hover bg-surface text-primary"
          />
          多模态（视觉）模型 — 可作为智能体的图片理解模型
        </label>

        <div className="flex justify-end items-center gap-3 pt-2">
          {editingId && (
            <button
              onClick={resetForm}
              className="flex items-center gap-1.5 px-4 py-2 rounded-lg border border-surface-hover text-sm text-text-secondary hover:bg-surface-hover"
            >
              <X size={14} /> 取消
            </button>
          )}
          <SaveButton
            saving={saving}
            saved={saved}
            onClick={handleSave}
            label={editingId ? '保存' : '添加'}
          />
        </div>
      </div>
    </div>
  );
}
