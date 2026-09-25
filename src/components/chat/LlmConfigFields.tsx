import type { LlmConfigBlock } from '@/lib/types';
import { LLM_PRESETS, findPreset } from '@/lib/llm-presets';

interface Props {
  config: LlmConfigBlock;
  onChange: (partial: Partial<LlmConfigBlock>) => void;
  apiKeyOptional?: boolean;
}

export function LlmConfigFields({ config, onChange, apiKeyOptional }: Props) {
  const preset = findPreset(config.provider);

  const handleProviderChange = (provider: string) => {
    const newPreset = findPreset(provider);
    onChange({
      provider,
      model: newPreset?.models[0] || config.model,
      base_url: newPreset?.baseURL || config.base_url,
    });
  };

  return (
    <div className="space-y-3">
      <div className="space-y-1.5">
        <label className="text-xs text-codex-muted">Provider</label>
        <select
          value={config.provider}
          onChange={(e) => handleProviderChange(e.target.value)}
          className="w-full bg-codex-surface border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
        >
          {LLM_PRESETS.map((p) => (
            <option key={p.provider} value={p.provider}>
              {p.label}
            </option>
          ))}
        </select>
      </div>

      <div className="space-y-1.5">
        <label className="text-xs text-codex-muted">Model</label>
        <select
          value={config.model}
          onChange={(e) => onChange({ model: e.target.value })}
          className="w-full bg-codex-surface border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
        >
          {preset?.models.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
          {!preset?.models.includes(config.model) && (
            <option value={config.model}>{config.model}</option>
          )}
        </select>
      </div>

      <div className="space-y-1.5">
        <label className="text-xs text-codex-muted">Base URL</label>
        <input
          type="text"
          value={config.base_url}
          onChange={(e) => onChange({ base_url: e.target.value })}
          className="w-full bg-codex-surface border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
        />
      </div>

      <div className="space-y-1.5">
        <label className="text-xs text-codex-muted">回传思考内容（reasoning）</label>
        <select
          value={
            config.reasoning_passthrough === undefined || config.reasoning_passthrough === null
              ? 'auto'
              : config.reasoning_passthrough
                ? 'on'
                : 'off'
          }
          onChange={(e) =>
            onChange({
              reasoning_passthrough:
                e.target.value === 'auto' ? undefined : e.target.value === 'on',
            })
          }
          className="w-full bg-codex-surface border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
        >
          <option value="auto">自动（DeepSeek 官方端点默认开）</option>
          <option value="on">总是回传</option>
          <option value="off">从不回传</option>
        </select>
        <p className="text-[11px] text-codex-muted">
          DeepSeek 思考模型在带 tools 的请求里<strong className="font-semibold">必须</strong>把历史
          reasoning_content 回传，否则 API 返回 400；回传后会被拼进上下文，模型因此能接着之前的思考继续
          （而不是从头再想）。其他厂商一般不需要该字段，保持"自动"即可。
        </p>
      </div>

      <div className="space-y-1.5">
        <label className="text-xs text-codex-muted">
          API Key
          {apiKeyOptional && (
            <span className="ml-1 text-codex-muted">（留空使用全局默认）</span>
          )}
          {preset?.apiKeyEnv && (
            <span className="ml-1 text-codex-muted">(env: {preset.apiKeyEnv})</span>
          )}
        </label>
        <input
          type="password"
          value={config.api_key}
          onChange={(e) => onChange({ api_key: e.target.value })}
          placeholder={apiKeyOptional ? 'Leave empty to use global default' : '输入 API Key'}
          className="w-full bg-codex-surface border border-codex-border rounded-lg px-3 py-2 text-sm text-codex-primary outline-none focus:border-codex-border-light"
        />
      </div>
    </div>
  );
}
