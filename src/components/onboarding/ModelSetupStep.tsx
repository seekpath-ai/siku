import { useEffect, useState } from 'react';
import { Check, ExternalLink, Loader2 } from 'lucide-react';
import { open as shellOpen } from '@tauri-apps/plugin-shell';
import { LLM_PRESETS, type LlmPreset } from '@/lib/llm-presets';
import {
  llmProviderCreate,
  llmProviderList,
  llmProviderSetDefault,
  llmProviderUpdate,
  settingsListModels,
  settingsValidateLlm,
} from '@/lib/tauri';

/** Domestic providers offered front-and-center in onboarding, in display order. */
const DOMESTIC_PROVIDERS = ['deepseek', 'kimi', 'qwen', 'zhipu'] as const;

const SHORT_NAMES: Record<string, string> = {
  deepseek: 'DeepSeek',
  kimi: 'Kimi',
  qwen: '通义千问',
  zhipu: '智谱 z.ai',
};

/** Official brand marks (Simple Icons, CC0) — rendered on a white chip
 *  because Kimi/Z.ai marks are near-black and vanish on the dark theme. */
const LOGO_FILES: Record<string, string> = {
  deepseek: '/llm-logos/deepseek.svg',
  kimi: '/llm-logos/kimi.svg',
  qwen: '/llm-logos/qwen.svg',
  zhipu: '/llm-logos/zhipu.svg',
};

function ProviderBadge({ preset, size = 28 }: { preset: LlmPreset; size?: number }) {
  const logo = LOGO_FILES[preset.provider];
  if (logo) {
    return (
      <span
        className="inline-flex items-center justify-center rounded-lg bg-white shrink-0"
        style={{ width: size, height: size }}
      >
        <img
          src={logo}
          alt={preset.label}
          style={{ width: size * 0.72, height: size * 0.72 }}
        />
      </span>
    );
  }
  return (
    <span
      className="inline-flex items-center justify-center rounded-lg text-white font-bold shrink-0"
      style={{
        width: size,
        height: size,
        background: preset.accent ?? '#666',
        fontSize: size * 0.5,
      }}
    >
      {preset.label[0]}
    </span>
  );
}

/** Onboarding step 2: configure the default LLM without leaving the wizard.
 *  Provider cards carry a link to the official console; once the key is
 *  pasted, the live model catalog is fetched via GET /models so the user
 *  picks from what the account can actually use. */
export function ModelSetupStep() {
  /** null = still checking existing config */
  const [configured, setConfigured] = useState<{ label: string; model: string } | null | false>(null);
  const [editing, setEditing] = useState(false);
  const [selected, setSelected] = useState<string>('deepseek');
  const [apiKey, setApiKey] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [models, setModels] = useState<string[] | null>(null);
  const [model, setModel] = useState('');
  const [saved, setSaved] = useState(false);
  const [fallbackNote, setFallbackNote] = useState(false);

  useEffect(() => {
    llmProviderList()
      .then((list) => {
        const def = list.find((p) => p.is_default) ?? list.find((p) => p.api_key);
        if (def && (def.api_key || def.provider === 'ollama')) {
          setConfigured({ label: SHORT_NAMES[def.provider] ?? def.name, model: def.model });
        } else {
          setConfigured(false);
        }
      })
      .catch(() => setConfigured(false));
  }, []);

  const preset = LLM_PRESETS.find((p) => p.provider === selected)!;

  const resetFormFor = (provider: string) => {
    setSelected(provider);
    setModels(null);
    setModel('');
    setError(null);
    setSaved(false);
    setFallbackNote(false);
  };

  const handleValidate = async () => {
    if (!apiKey.trim() || busy) return;
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      await settingsValidateLlm(preset.provider, apiKey.trim(), preset.baseURL, preset.models[0]);
      let live: string[];
      try {
        live = await settingsListModels(apiKey.trim(), preset.baseURL);
        setFallbackNote(false);
      } catch {
        // The key works but the catalog call failed — fall back to the
        // preset's known models rather than blocking the user.
        live = [...preset.models];
        setFallbackNote(true);
      }
      setModels(live);
      // Prefer the preset's recommended order when those models are live.
      const recommended = preset.models.find((m) => live.includes(m));
      setModel(recommended ?? live[0]);
    } catch (e) {
      setModels(null);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleSave = async () => {
    if (!models || !model || busy) return;
    setBusy(true);
    setError(null);
    try {
      const list = await llmProviderList();
      const existing = list.find((p) => p.provider === preset.provider);
      const input = {
        name: SHORT_NAMES[preset.provider] ?? preset.label,
        provider: preset.provider,
        model,
        api_key: apiKey.trim(),
        base_url: preset.baseURL,
        is_default: true,
      };
      if (existing) {
        await llmProviderUpdate(existing.id, input);
        await llmProviderSetDefault(existing.id);
      } else {
        await llmProviderCreate(input);
      }
      setSaved(true);
      setConfigured({ label: input.name, model });
      setEditing(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  // Already configured: show the state, offer reconfiguration.
  if (configured && !editing) {
    return (
      <div className="flex flex-col items-center gap-3">
        <div className="flex items-center gap-2 px-4 py-3 rounded-xl bg-primary/10 border border-primary/30 text-sm text-text-primary">
          <Check size={15} className="text-primary shrink-0" />
          已配置默认模型：{configured.label} · {configured.model}
        </div>
        <button
          onClick={() => setEditing(true)}
          className="text-xs text-primary underline underline-offset-4 hover:opacity-80"
        >
          重新配置
        </button>
      </div>
    );
  }

  if (configured === null) {
    return (
      <div className="flex items-center justify-center gap-2 py-6 text-sm text-text-secondary">
        <Loader2 size={14} className="animate-spin" />检查现有配置…
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3 text-left">
      {/* Provider cards */}
      <div className="grid grid-cols-4 gap-2">
        {DOMESTIC_PROVIDERS.map((key) => {
          const p = LLM_PRESETS.find((x) => x.provider === key)!;
          const active = selected === key;
          return (
            <button
              key={key}
              onClick={() => resetFormFor(key)}
              className={`flex flex-col items-center gap-1.5 px-1 py-2.5 rounded-xl border transition-colors ${
                active
                  ? 'border-primary/60 bg-primary/10'
                  : 'border-surface-hover bg-background hover:bg-surface-hover'
              }`}
            >
              <ProviderBadge preset={p} />
              <span className="text-[11px] text-text-primary leading-none">
                {SHORT_NAMES[key]}
              </span>
            </button>
          );
        })}
      </div>

      {/* Console link */}
      {preset.platformUrl && (
        <button
          onClick={() => shellOpen(preset.platformUrl!).catch(() => window.open(preset.platformUrl!, '_blank'))}
          className="flex items-center justify-center gap-1 text-xs text-primary hover:opacity-80"
        >
          <ExternalLink size={11} />
          没有 Key？前往{SHORT_NAMES[selected]}开放平台免费获取
        </button>
      )}

      {/* Key input + validate */}
      <div className="flex gap-2">
        <input
          type="password"
          value={apiKey}
          onChange={(e) => {
            setApiKey(e.target.value);
            setModels(null);
            setSaved(false);
          }}
          placeholder="粘贴 API Key"
          spellCheck={false}
          className="flex-1 min-w-0 px-3 py-2 rounded-lg bg-background border border-surface-hover text-xs text-text-primary outline-none focus:border-primary/50 placeholder:text-text-secondary/50"
        />
        <button
          onClick={handleValidate}
          disabled={!apiKey.trim() || busy}
          className="px-3 py-2 rounded-lg bg-primary text-white text-xs font-medium hover:opacity-90 disabled:opacity-40 shrink-0"
        >
          {busy && !models ? <Loader2 size={13} className="animate-spin" /> : '验证并获取模型'}
        </button>
      </div>

      {error && <div className="text-xs text-red-400 break-words">{error}</div>}

      {/* Model picker + save */}
      {models && (
        <div className="flex flex-col gap-2">
          {fallbackNote && (
            <div className="text-[11px] text-text-secondary">
              模型列表拉取失败，已回退到内置推荐列表。
            </div>
          )}
          <div className="flex gap-2">
            <select
              value={model}
              onChange={(e) => setModel(e.target.value)}
              className="flex-1 min-w-0 px-3 py-2 rounded-lg bg-background border border-surface-hover text-xs text-text-primary focus:outline-none focus:border-primary/50"
            >
              {models.map((m) => (
                <option key={m} value={m}>
                  {m}
                  {preset.models.includes(m) ? '（推荐）' : ''}
                </option>
              ))}
            </select>
            <button
              onClick={handleSave}
              disabled={!model || busy}
              className="px-3 py-2 rounded-lg bg-primary text-white text-xs font-medium hover:opacity-90 disabled:opacity-40 shrink-0"
            >
              {saved ? '✓ 已保存' : '保存为默认模型'}
            </button>
          </div>
        </div>
      )}

      <div className="text-[11px] text-text-secondary text-center">
        用其它厂商（OpenAI / Claude / 本地 Ollama）？稍后在 设置 → 模型 里添加。
      </div>
    </div>
  );
}
