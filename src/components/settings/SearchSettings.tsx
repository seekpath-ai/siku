import { useCallback, useEffect, useState } from 'react';
import { ArrowDown, ArrowUp, Check, Loader2 } from 'lucide-react';
import { settingsAppGet, settingsAppSave, type AppSettings, type SearchEngineConfig } from '@/lib/tauri';

interface EngineMeta {
  name: string;
  desc: string;
  /** Requires an API key input. */
  key?: boolean;
  /** Requires a base URL input. */
  url?: boolean;
}

const ENGINE_META: Record<string, EngineMeta> = {
  bing: { name: 'Bing', desc: '国内可直接访问，无需密钥（网页抓取）' },
  duckduckgo: { name: 'DuckDuckGo', desc: '需代理或海外网络（网页抓取）' },
  tavily: { name: 'Tavily', desc: '面向 AI 的搜索 API，需 API Key', key: true },
  brave: { name: 'Brave Search', desc: '需 API Key', key: true },
  searxng: { name: 'SearXNG', desc: '自托管搜索聚合实例，需地址', url: true },
};

const ENGINE_ORDER = ['bing', 'duckduckgo', 'tavily', 'brave', 'searxng'];

/** 网络搜索设置：web_search 工具的引擎链——启用、排序（自上而下回退）、
 *  密钥/地址。一次工具调用内按序尝试，首个出结果的引擎生效。 */
export function SearchSettings() {
  const [engines, setEngines] = useState<SearchEngineConfig[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const settingsRef = useCallback(async (next: SearchEngineConfig[]) => {
    setEngines(next);
    setSaving(true);
    try {
      const current = await settingsAppGet();
      await settingsAppSave({ ...current, search_engines: next });
      setSaved(true);
      window.setTimeout(() => setSaved(false), 2000);
    } catch (err) {
      console.error('Failed to save search engines:', err);
    } finally {
      setSaving(false);
    }
  }, []);

  useEffect(() => {
    settingsAppGet()
      .then((s: AppSettings) => {
        // Merge stored order with any engines the build added since.
        const stored = s.search_engines ?? [];
        const merged = [...stored];
        for (const id of ENGINE_ORDER) {
          if (!merged.some((e) => e.id === id)) merged.push({ id, enabled: false });
        }
        setEngines(merged.filter((e) => ENGINE_META[e.id]));
      })
      .catch((err) => console.error('Failed to load search settings:', err))
      .finally(() => setLoading(false));
  }, []);

  const toggle = (id: string) =>
    settingsRef(engines.map((e) => (e.id === id ? { ...e, enabled: !e.enabled } : e)));

  const move = (id: string, dir: -1 | 1) => {
    const i = engines.findIndex((e) => e.id === id);
    const j = i + dir;
    if (i < 0 || j < 0 || j >= engines.length) return;
    const next = [...engines];
    [next[i], next[j]] = [next[j], next[i]];
    settingsRef(next);
  };

  const patch = (id: string, field: 'apiKey' | 'baseUrl', value: string) =>
    settingsRef(engines.map((e) => (e.id === id ? { ...e, [field]: value || null } : e)));

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2 text-sm text-text-primary">
        <span>搜索引擎</span>
        {saving ? (
          <Loader2 size={13} className="animate-spin text-text-secondary" />
        ) : saved ? (
          <span className="flex items-center gap-1 text-[10px] text-accent">
            <Check size={11} /> 已保存
          </span>
        ) : null}
      </div>
      <p className="text-xs text-text-secondary leading-relaxed">
        智能体的 web_search 工具按下列顺序依次尝试已启用的引擎，首个返回结果的引擎生效（一次调用内自动回退，不会浪费对话轮次）。
        网页抓取类引擎走系统网络；需要密钥的引擎配置后才会被尝试。
      </p>
      {loading ? (
        <div className="flex items-center gap-2 text-sm text-text-secondary">
          <Loader2 size={14} className="animate-spin" />加载中...
        </div>
      ) : (
        <div className="space-y-2">
          {engines.map((e, i) => {
            const meta = ENGINE_META[e.id];
            return (
              <div
                key={e.id}
                className={`px-4 py-3 bg-surface border rounded-xl transition-colors ${
                  e.enabled ? 'border-primary/30' : 'border-surface-hover'
                }`}
              >
                <div className="flex items-center gap-3">
                  <input
                    type="checkbox"
                    checked={e.enabled}
                    onChange={() => toggle(e.id)}
                    className="accent-primary"
                  />
                  <div className="flex-1 min-w-0">
                    <div className="text-sm font-medium text-text-primary">{meta.name}</div>
                    <p className="text-[11px] text-text-secondary/70">{meta.desc}</p>
                  </div>
                  <div className="flex items-center gap-0.5 shrink-0">
                    <button
                      onClick={() => move(e.id, -1)}
                      disabled={i === 0}
                      title="上移（优先尝试）"
                      className="p-1 rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover disabled:opacity-30"
                    >
                      <ArrowUp size={13} />
                    </button>
                    <button
                      onClick={() => move(e.id, 1)}
                      disabled={i === engines.length - 1}
                      title="下移"
                      className="p-1 rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover disabled:opacity-30"
                    >
                      <ArrowDown size={13} />
                    </button>
                  </div>
                </div>
                {e.enabled && meta.key && (
                  <input
                    type="password"
                    value={e.apiKey ?? ''}
                    onChange={(ev) => patch(e.id, 'apiKey', ev.target.value)}
                    placeholder={`${meta.name} API Key`}
                    className="mt-2 w-full px-3 py-1.5 bg-background border border-surface-hover rounded-lg text-xs text-text-primary focus:outline-none focus:border-primary/50"
                  />
                )}
                {e.enabled && meta.url && (
                  <input
                    type="text"
                    value={e.baseUrl ?? ''}
                    onChange={(ev) => patch(e.id, 'baseUrl', ev.target.value)}
                    placeholder="实例地址，如 https://searx.example.com"
                    className="mt-2 w-full px-3 py-1.5 bg-background border border-surface-hover rounded-lg text-xs text-text-primary focus:outline-none focus:border-primary/50"
                  />
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
