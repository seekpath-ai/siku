import { useCallback, useEffect, useState } from 'react';
import { Check, Loader2, RefreshCw } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { settingsAppGet, settingsAppSave } from '@/lib/tauri';

const MODES = [
  { id: 'persistent', name: '持久配置（推荐）', desc: '思库管理的独立浏览器配置，站点登录态长期保留' },
  { id: 'temporary', name: '临时实例', desc: '每次干净的临时配置，适合无需登录的一次性任务' },
  { id: 'attach', name: '附加模式', desc: '连接你已用 --remote-debugging-port 启动的浏览器（高级）' },
] as const;

/** 浏览器自动化设置：browser_* 工具使用的浏览器路径与默认连接模式。 */
export function BrowserSettings() {
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [path, setPath] = useState('');
  const [mode, setMode] = useState<string>('persistent');
  const [detected, setDetected] = useState<string | null>(null);

  const detect = useCallback(() => {
    invoke<string>('browser_detect')
      .then(setDetected)
      .catch(() => setDetected(null));
  }, []);

  useEffect(() => {
    settingsAppGet()
      .then((s) => {
        setPath(s.browser_path ?? '');
        setMode(s.browser_default_mode ?? 'persistent');
      })
      .catch((err) => console.error('Failed to load browser settings:', err))
      .finally(() => setLoading(false));
    detect();
  }, [detect]);

  const persist = useCallback(async (nextPath: string, nextMode: string) => {
    setSaving(true);
    try {
      const current = await settingsAppGet();
      await settingsAppSave({
        ...current,
        browser_path: nextPath || null,
        browser_default_mode: nextMode === 'persistent' ? null : nextMode,
      });
      setSaved(true);
      window.setTimeout(() => setSaved(false), 2000);
    } catch (err) {
      console.error('Failed to save browser settings:', err);
    } finally {
      setSaving(false);
    }
  }, []);

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-sm text-text-secondary">
        <Loader2 size={14} className="animate-spin" />加载中...
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2 text-sm text-text-primary">
        <span>浏览器自动化</span>
        {saving ? (
          <Loader2 size={13} className="animate-spin text-text-secondary" />
        ) : saved ? (
          <span className="flex items-center gap-1 text-[10px] text-accent">
            <Check size={11} /> 已保存
          </span>
        ) : null}
      </div>
      <p className="text-xs text-text-secondary leading-relaxed">
        智能体的 browser_* 工具驱动本机真实的 Chrome/Edge 完成网页操作（登录态检索、文献下载等）。
        浏览器窗口需要亮屏未锁定的桌面环境。
      </p>

      <div className="px-4 py-3 bg-surface border border-surface-hover rounded-xl space-y-2">
        <div className="text-sm font-medium text-text-primary">浏览器路径</div>
        <input
          type="text"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          onBlur={() => persist(path.trim(), mode)}
          placeholder="留空自动检测"
          className="w-full px-3 py-1.5 bg-background border border-surface-hover rounded-lg text-xs text-text-primary focus:outline-none focus:border-primary/50"
        />
        <div className="flex items-center gap-2 text-[11px] text-text-secondary/70">
          {detected ? (
            <span className="truncate">自动检测：{path.trim() ? '（已被上方覆盖）' : detected}</span>
          ) : (
            <span>未检测到 Chrome/Edge，请安装或手动指定路径</span>
          )}
          <button
            onClick={detect}
            title="重新检测"
            className="p-0.5 rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover shrink-0"
          >
            <RefreshCw size={11} />
          </button>
        </div>
      </div>

      <div className="px-4 py-3 bg-surface border border-surface-hover rounded-xl space-y-2">
        <div className="text-sm font-medium text-text-primary">默认连接模式</div>
        <p className="text-[11px] text-text-secondary/70">智能体调用 browser_connect 不传 mode 时生效</p>
        <div className="space-y-1.5">
          {MODES.map((m) => (
            <label key={m.id} className="flex items-start gap-2.5 cursor-pointer group">
              <input
                type="radio"
                name="browser-mode"
                checked={mode === m.id}
                onChange={() => {
                  setMode(m.id);
                  persist(path.trim(), m.id);
                }}
                className="accent-primary mt-0.5"
              />
              <div className="flex-1 min-w-0">
                <div className="text-xs text-text-primary group-hover:text-primary transition-colors">{m.name}</div>
                <div className="text-[11px] text-text-secondary/70">{m.desc}</div>
              </div>
            </label>
          ))}
        </div>
      </div>
    </div>
  );
}
