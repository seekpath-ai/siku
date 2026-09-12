import { useEffect, useState } from 'react';
import { Loader2, SlidersHorizontal, Zap } from 'lucide-react';
import {
  settingsAppGet,
  settingsAppSave,
  searchEmbeddingStatus,
  searchTestEmbeddingEndpoint,
} from '@/lib/tauri';
import { SaveButton } from '@/components/ui/SaveButton';
import type { AppSettings, EmbeddingStatus, EmbeddingProbe } from '@/lib/tauri';

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
  tool_paper_read_max_chars: 2500,
  tool_paper_read_total_max_chars: 24000,
  tool_note_read_max_chars: 200,
  tool_knowledge_read_max_chars: 200,
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
  const [embeddingStatus, setEmbeddingStatus] = useState<EmbeddingStatus | null>(null);
  const [probe, setProbe] = useState<EmbeddingProbe | null>(null);
  const [testing, setTesting] = useState(false);
  // The service address is the switch: empty means no semantic search.
  const semanticEnabled = (settings.embedding_base_url || '').trim().length > 0;

  const refreshEmbeddingStatus = () => {
    searchEmbeddingStatus()
      .then(setEmbeddingStatus)
      .catch((err) => console.error('Failed to load embedding status:', err));
  };

  useEffect(() => {
    settingsAppGet()
      .then((s) => {
        setSettings((prev) => ({ ...prev, ...s }));
      })
      .catch((err) => console.error('Failed to load advanced settings:', err))
      .finally(() => setLoading(false));
    refreshEmbeddingStatus();
  }, []);

  const handleTestEndpoint = async () => {
    setTesting(true);
    try {
      setProbe(
        await searchTestEmbeddingEndpoint(
          settings.embedding_base_url || '',
          settings.embedding_model || '',
          settings.embedding_api_key || '',
        ),
      );
    } catch (err) {
      console.error('Failed to probe embedding endpoint:', err);
    } finally {
      setTesting(false);
    }
  };

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
      refreshEmbeddingStatus();
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
        <h3 className="text-sm font-medium text-text-primary">语义搜索（可选）</h3>
        <p className="text-xs text-text-secondary">
          关键词检索始终可用，不需要任何配置。填入下面的服务地址后会额外启用语义召回——两者按名次融合，
          不是二选一。地址留空即关闭语义搜索，关键词检索不受影响。
        </p>

        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div className="space-y-1.5">
            <label className="block text-sm text-text-secondary">服务地址（Base URL）</label>
            <input
              type="text"
              value={settings.embedding_base_url || ''}
              onChange={(e) => updateText('embedding_base_url', e.target.value)}
              placeholder="http://127.0.0.1:8899/v1"
              className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
            />
            <p className="text-xs text-text-secondary/70">
              留空 = 不启用。需包含 <code>/v1</code>，OpenAI 兼容服务皆可：本项目脚本
              <code>http://127.0.0.1:8899/v1</code>、Ollama <code>http://127.0.0.1:11434/v1</code>。
            </p>
          </div>
          <div className="space-y-1.5">
            <label className="block text-sm text-text-secondary">模型名</label>
            <input
              type="text"
              value={settings.embedding_model || ''}
              onChange={(e) => updateText('embedding_model', e.target.value)}
              placeholder="BAAI/bge-small-zh-v1.5"
              className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
            />
            <p className="text-xs text-text-secondary/70">
              填服务端实际的模型名：本项目脚本为 <code>BAAI/bge-small-zh-v1.5</code>，云端为
              <code>text-embedding-3-small</code> 等。改了名字，已有向量会按新模型重算。
            </p>
          </div>
        </div>

        {semanticEnabled && (
          <>
            <div className="space-y-1.5 max-w-md">
              <label className="block text-sm text-text-secondary">API Key</label>
              <input
                type="password"
                value={settings.embedding_api_key || ''}
                onChange={(e) => updateText('embedding_api_key', e.target.value)}
                placeholder="如有"
                className="w-full bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
              />
              <p className="text-xs text-text-secondary/70">本地服务不需要；云端服务填自己的 Key。</p>
            </div>

            <div className="space-y-2 rounded-lg border border-surface-hover p-3">
              <div className="flex flex-wrap items-center gap-3">
                <button
                  type="button"
                  onClick={handleTestEndpoint}
                  disabled={testing}
                  className="inline-flex items-center gap-1.5 rounded-lg border border-surface-hover px-3 py-1.5 text-sm text-text-primary hover:border-primary disabled:opacity-50"
                >
                  {testing ? <Loader2 size={14} className="animate-spin" /> : <Zap size={14} />}
                  测试连接
                </button>
                <span className="text-xs text-text-secondary/70">用上面填的值直接测，不必先保存。</span>
              </div>

              {probe &&
                (probe.ok ? (
                  <p className="text-xs text-emerald-400">
                    连接成功 · {probe.dimensions} 维 · {probe.latency_ms} ms
                    {embeddingStatus?.dimensions != null &&
                      probe.dimensions !== embeddingStatus.dimensions &&
                      ' · 与库中已存向量维度不一致，需重建索引'}
                  </p>
                ) : (
                  <p className="text-xs text-red-400">连接失败：{probe.error}</p>
                ))}
            </div>
          </>
        )}

        {embeddingStatus && (
          <div className="space-y-2 rounded-lg border border-surface-hover p-3">
            <p className="text-xs text-text-secondary">
              {embeddingStatus.leg_enabled
                ? `语义搜索：已启用 · 已生成向量 ${embeddingStatus.embedded_chunks}/${embeddingStatus.total_chunks}`
                : '语义搜索：未启用'}
              {embeddingStatus.leg_enabled &&
                embeddingStatus.dimensions != null &&
                ` · ${embeddingStatus.dimensions} 维`}
              {embeddingStatus.leg_enabled &&
                embeddingStatus.embedded_chunks > 0 &&
                ` · ${embeddingStatus.model}`}
            </p>

            {embeddingStatus.leg_enabled &&
              embeddingStatus.embedded_chunks === 0 &&
              embeddingStatus.total_chunks > 0 && (
                <p className="text-xs text-amber-400">
                  还没有向量：在图书馆对文献右键执行「重建索引」即可生成（共{' '}
                  {embeddingStatus.total_chunks} 块）。
                </p>
              )}

            {embeddingStatus.placeholder_chunks > 0 && (
              <p className="text-xs text-text-secondary/70">
                {embeddingStatus.leg_enabled ? '另有 ' : '库中 '}
                {embeddingStatus.placeholder_chunks} 块早期占位向量，不参与检索。
              </p>
            )}

            {embeddingStatus.other_models.length > 0 && (
              <p className="text-xs text-amber-400">
                另有 {embeddingStatus.other_models.reduce((sum, m) => sum + m.chunks, 0)} 块属于其它模型（
                {embeddingStatus.other_models.map((m) => m.model).join('、')}
                ），不参与检索；对文献重建索引后生效。
              </p>
            )}
          </div>
        )}
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
