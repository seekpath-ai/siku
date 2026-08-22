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
