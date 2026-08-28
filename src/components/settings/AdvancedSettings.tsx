import { useEffect, useState } from 'react';
import { Loader2, SlidersHorizontal } from 'lucide-react';
import { settingsAppGet, settingsAppSave } from '@/lib/tauri';
import { SaveButton } from '@/components/ui/SaveButton';
import type { AppSettings } from '@/lib/tauri';

interface LimitField {
  key: keyof AppSettings;
  label: string;
  unit: string;
  min: number;
}

const LOG_FIELDS: LimitField[] = [
  { key: 'log_max_size_mb', label: '日志文件大小上限', unit: 'MB', min: 1 },
  { key: 'log_max_files', label: '日志备份数量', unit: '个', min: 0 },
  { key: 'log_llm_response_preview_max_chars', label: 'LLM 响应日志预览长度', unit: '字符', min: 1 },
  { key: 'log_region_detection_preview_max_chars', label: '区域识别日志预览长度', unit: '字符', min: 1 },
];

const DISPLAY_FIELDS: LimitField[] = [
  { key: 'graph_node_label_max_chars', label: '图谱节点标签长度', unit: '字符', min: 1 },
];

const PROMPT_FIELDS: LimitField[] = [
  { key: 'region_detection_line_max_chars', label: '区域识别单行文本长度', unit: '字符', min: 1 },
  { key: 'rag_chunk_max_chars', label: 'RAG 检索片段长度', unit: '字符', min: 1 },
  { key: 'rag_max_context_tokens', label: 'RAG 上下文容量', unit: 'tokens', min: 500 },
];

const RESEARCH_FIELDS: LimitField[] = [
  { key: 'research_auto_discover_interval_hours', label: '科研自动发现间隔', unit: '小时', min: 0 },
  { key: 'research_discover_max_results', label: '每次发现条数上限', unit: '条', min: 1 },
];

const TOOL_FIELDS: LimitField[] = [
  { key: 'tool_web_fetch_max_chars', label: '网页抓取返回长度', unit: '字符', min: 1 },
  { key: 'tool_file_read_max_chars', label: '文件读取返回长度', unit: '字符', min: 1 },
  { key: 'tool_paper_read_max_chars', label: '论文阅读片段长度', unit: '字符', min: 1 },
  { key: 'tool_paper_read_total_max_chars', label: '论文阅读单次总长上限', unit: '字符', min: 1 },
  { key: 'tool_note_read_max_chars', label: '笔记列表预览长度', unit: '字符', min: 1 },
  { key: 'tool_knowledge_read_max_chars', label: '知识库列表预览长度', unit: '字符', min: 1 },
];

const DEFAULT_VALUES: AppSettings = {
  default_approval: { mode: 'auto' },
  default_max_loops: 10,
  default_context_budget: 28000,
  default_max_memory_rounds: 10,
  log_max_size_mb: 10,
  log_max_files: 5,
  log_llm_response_preview_max_chars: 500,
  log_region_detection_preview_max_chars: 300,
  graph_node_label_max_chars: 50,
  region_detection_line_max_chars: 200,
  rag_chunk_max_chars: 800,
  tool_web_fetch_max_chars: 10000,
  tool_file_read_max_chars: 8000,
  tool_paper_read_max_chars: 500,
  tool_paper_read_total_max_chars: 24000,
  tool_note_read_max_chars: 200,
  tool_knowledge_read_max_chars: 200,
  embedding_backend: 'hash',
  embedding_base_url: '',
  embedding_api_key: '',
  embedding_model: 'text-embedding-3-small',
  rag_max_context_tokens: 4000,
  research_auto_discover_interval_hours: 6,
  research_discover_max_results: 10,
};

export function AdvancedSettings() {
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_VALUES);

  useEffect(() => {
    settingsAppGet()
      .then((s) => {
        setSettings((prev) => ({ ...prev, ...s }));
      })
      .catch((err) => console.error('Failed to load advanced settings:', err))
      .finally(() => setLoading(false));
  }, []);

  const updateField = (key: keyof AppSettings, value: number) => {
    setSettings((prev) => ({ ...prev, [key]: value }));
  };

  const updateText = (key: keyof AppSettings, value: string) => {
    setSettings((prev) => ({ ...prev, [key]: value }));
  };

  const handleSave = async () => {
    setSaving(true);
    try {
      const current = await settingsAppGet();
      await settingsAppSave({ ...current, ...settings });
      setSaved(true);
    } catch (err) {
      console.error('Failed to save advanced settings:', err);
    } finally {
      setSaving(false);
    }
  };

  const renderFields = (fields: LimitField[]) => (
    <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
      {fields.map((field) => (
        <div key={field.key} className="space-y-1.5">
          <label className="block text-sm text-text-secondary">
            {field.label}
          </label>
          <div className="flex items-center gap-2">
            <input
              type="number"
              min={field.min}
              value={settings[field.key] as number}
              onChange={(e) => updateField(field.key, parseInt(e.target.value) || field.min)}
              className="flex-1 bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
            />
            <span className="text-xs text-text-secondary w-10 shrink-0">{field.unit}</span>
          </div>
        </div>
      ))}
    </div>
  );

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-sm text-text-secondary">
        <Loader2 size={14} className="animate-spin" /> 加载中...
      </div>
    );
  }

  return (
    <div className="space-y-8">
      <div className="flex items-center gap-2">
        <SlidersHorizontal size={18} className="text-primary" />
        <h2 className="text-lg font-semibold text-text-primary">高级设置</h2>
      </div>

      <p className="text-xs text-text-secondary">
        修改以下截断/限制数值会立即在后台生效（日志文件大小需重启后生效）。数值为 0 表示禁用对应限制，但不建议设置为 0。
      </p>

      <section className="space-y-3">
        <h3 className="text-sm font-medium text-text-primary">日志与预览</h3>
        {renderFields(LOG_FIELDS)}
      </section>

      <section className="space-y-3">
        <h3 className="text-sm font-medium text-text-primary">界面显示</h3>
        {renderFields(DISPLAY_FIELDS)}
      </section>

      <section className="space-y-3">
        <h3 className="text-sm font-medium text-text-primary">Prompt 与 RAG</h3>
        {renderFields(PROMPT_FIELDS)}
      </section>

      <section className="space-y-3">
        <h3 className="text-sm font-medium text-text-primary">向量嵌入</h3>
        <p className="text-xs text-text-secondary">
          默认使用内置哈希嵌入（离线、无语义）。选择「API 嵌入」后，文献重建索引时将通过 OpenAI 兼容的 embeddings 接口生成真实语义向量。
        </p>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div className="space-y-1.5">
            <label className="block text-sm text-text-secondary">嵌入后端</label>
            <select
              value={settings.embedding_backend || 'hash'}
              onChange={(e) => updateText('embedding_backend', e.target.value)}
              className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
            >
              <option value="hash">内置哈希嵌入（离线）</option>
              <option value="api">API 嵌入（OpenAI 兼容）</option>
            </select>
          </div>
          <div className="space-y-1.5">
            <label className="block text-sm text-text-secondary">嵌入模型</label>
            <input
              type="text"
              value={settings.embedding_model || ''}
              onChange={(e) => updateText('embedding_model', e.target.value)}
              placeholder="text-embedding-3-small"
              className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
            />
          </div>
          <div className="space-y-1.5">
            <label className="block text-sm text-text-secondary">API 地址（Base URL）</label>
            <input
              type="text"
              value={settings.embedding_base_url || ''}
              onChange={(e) => updateText('embedding_base_url', e.target.value)}
              placeholder="https://api.openai.com/v1"
              className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
            />
          </div>
          <div className="space-y-1.5">
            <label className="block text-sm text-text-secondary">API Key</label>
            <input
              type="password"
              value={settings.embedding_api_key || ''}
              onChange={(e) => updateText('embedding_api_key', e.target.value)}
              placeholder="sk-..."
              className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
            />
          </div>
        </div>
        <p className="text-xs text-text-secondary/70">
          配置后，请在图书馆对文献右键执行「重建索引」以生成新向量。
        </p>
      </section>

      <section className="space-y-3">
        <h3 className="text-sm font-medium text-text-primary">科研追踪</h3>
        <p className="text-xs text-text-secondary">
          自动发现间隔设为 0 可关闭定时扫描。修改后下轮扫描生效。
        </p>
        {renderFields(RESEARCH_FIELDS)}
      </section>

      <section className="space-y-3">
        <h3 className="text-sm font-medium text-text-primary">工具输出</h3>
        {renderFields(TOOL_FIELDS)}
      </section>

      <SaveButton saving={saving} saved={saved} onClick={handleSave} />
    </div>
  );
}
