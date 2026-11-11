import { useCallback, useEffect, useState } from 'react';
import { Check, FolderOpen, Loader2, RefreshCw, RotateCcw } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import {
  browserOpenSetup,
  browserResetProfile,
  fileBrowserOpenInSystem,
  settingsAppGet,
  settingsAppSave,
} from '@/lib/tauri';
import { useDialog } from '@/hooks/useDialog';

interface ProfileInfo {
  default_profile_dir: string;
  effective_profile_dir: string;
  effective_profile_exists: boolean;
}

/** Base name of a Windows/Unix path, for the status line. */
function baseName(p: string): string {
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? p;
}

/**
 * 浏览器自动化设置。
 *
 * 只做一件事：让用户把一个能登录网站的浏览器准备好。思库使用独立的持久
 * 配置，用户在里面登录一次即长期有效——不需要理解连接模式，也不需要从日常
 * 浏览器"导入登录态"（Chrome 127+ 会删掉复制来的 Cookie）。
 */
export function BrowserSettings() {
  const { alert, confirm } = useDialog();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [path, setPath] = useState('');
  const [launchArgs, setLaunchArgs] = useState('');
  const [profileDir, setProfileDir] = useState('');
  const [info, setInfo] = useState<ProfileInfo | null>(null);
  const [detected, setDetected] = useState<string | null>(null);
  const [opening, setOpening] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [advancedOpen, setAdvancedOpen] = useState(false);

  const reloadInfo = useCallback(() => {
    invoke<ProfileInfo>('browser_profile_info')
      .then(setInfo)
      .catch((err) => console.error('browser profile info:', err));
  }, []);

  const detect = useCallback(() => {
    invoke<string>('browser_detect')
      .then(setDetected)
      .catch(() => setDetected(null));
  }, []);

  useEffect(() => {
    settingsAppGet()
      .then((s) => {
        setPath(s.browser_path ?? '');
        setLaunchArgs(s.browser_launch_args ?? '');
        setProfileDir(s.browser_profile_dir ?? '');
      })
      .catch((err) => console.error('Failed to load browser settings:', err))
      .finally(() => setLoading(false));
    detect();
    reloadInfo();
  }, [detect, reloadInfo]);

  /** Save the advanced fields (the primary action needs no saving). */
  const persist = useCallback(
    async (next: { path?: string; args?: string; dir?: string }) => {
      setSaving(true);
      try {
        const current = await settingsAppGet();
        await settingsAppSave({
          ...current,
          browser_path: (next.path ?? path).trim() || null,
          browser_launch_args: (next.args ?? launchArgs).trim() || null,
          browser_profile_dir: (next.dir ?? profileDir).trim() || null,
        });
        setSaved(true);
        window.setTimeout(() => setSaved(false), 2000);
        reloadInfo();
      } catch (err) {
        console.error('Failed to save browser settings:', err);
      } finally {
        setSaving(false);
      }
    },
    [path, launchArgs, profileDir, reloadInfo],
  );

  const openSetup = useCallback(async () => {
    setOpening(true);
    setMessage(null);
    try {
      await browserOpenSetup();
      reloadInfo();
      setMessage('浏览器已打开。登录常用网站后直接关闭即可，登录态会长期保留。');
    } catch (e) {
      setMessage(`打开失败：${String(e).replace(/^Error:\s*/, '')}`);
    } finally {
      setOpening(false);
    }
  }, [reloadInfo]);

  const resetProfile = useCallback(async () => {
    const ok = await confirm(
      '将删除自动化浏览器的全部数据（登录态、扩展、缓存），下次使用需要重新登录。这个操作不可撤销。',
      { title: '重置浏览器配置', danger: true },
    );
    if (!ok) return;
    try {
      await browserResetProfile();
      reloadInfo();
      setMessage('已重置。下次打开浏览器是全新的配置。');
    } catch (e) {
      alert(String(e), { title: '重置失败', kind: 'error' });
    }
  }, [alert, confirm, reloadInfo]);

  const pickProfileDir = useCallback(async () => {
    const { open } = await import('@tauri-apps/plugin-dialog');
    const selected = await open({ directory: true, title: '选择浏览器配置目录' });
    if (typeof selected === 'string' && selected) {
      setProfileDir(selected);
      void persist({ dir: selected });
    }
  }, [persist]);

  const effectiveDir = profileDir.trim() || info?.effective_profile_dir || '';

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-sm text-text-secondary/60 py-6">
        <Loader2 size={14} className="animate-spin" />加载中...
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-base font-semibold text-text-primary">浏览器</h2>
        <p className="text-xs text-text-secondary/60 mt-1">
          智能体的 browser_* 工具驱动本机真实的 Chrome/Edge 完成网页操作（检索、下载、填表）。
          浏览器窗口必须是可见且未锁屏的。
        </p>
      </div>

      {/* One card, one decision: are we signed in or not. */}
      <div className="px-4 py-4 bg-surface border border-surface-hover rounded-xl space-y-3">
        <div className="text-sm font-medium text-text-primary">自动化浏览器</div>
        <p className="text-[11px] text-text-secondary/70 leading-relaxed">
          思库使用一个独立的浏览器配置。在这里登录一次，之后智能体就能直接使用这些登录；
          它长期有效，不会影响你日常的 Chrome。
        </p>
        <div className="flex flex-wrap items-center gap-2">
          <button
            onClick={() => void openSetup()}
            disabled={opening}
            className="h-9 px-4 bg-primary text-white rounded-lg text-xs disabled:opacity-40 hover:bg-primary/90 transition-colors flex items-center gap-1.5"
          >
            {opening ? <Loader2 size={13} className="animate-spin" /> : null}
            {opening ? '正在启动…' : '打开浏览器登录'}
          </button>
          <button
            onClick={() => void resetProfile()}
            className="h-9 px-3 rounded-lg border border-surface-hover text-xs text-text-secondary hover:text-text-primary hover:bg-surface-hover/50 transition-colors flex items-center gap-1.5"
          >
            <RotateCcw size={13} />
            重置浏览器配置
          </button>
        </div>
        <div className="text-[11px] text-text-secondary/60 space-y-0.5">
          <div>
            状态：
            {info?.effective_profile_exists ? (
              <span className="text-accent">已就绪</span>
            ) : (
              <span>尚未创建（首次打开浏览器时自动创建）</span>
            )}
            {detected && (
              <>
                <span className="mx-1.5">·</span>
                使用 {baseName(detected)}
              </>
            )}
          </div>
          <div className="font-mono break-all text-text-secondary/50">{effectiveDir}</div>
        </div>
        {message && (
          <p className="text-[11px] text-text-secondary/80 leading-relaxed break-all">{message}</p>
        )}
      </div>

      {/* Advanced: everything a normal user never needs to touch. */}
      <div className="rounded-xl border border-surface-hover bg-surface">
        <button
          onClick={() => setAdvancedOpen((v) => !v)}
          className="w-full flex items-center justify-between px-4 py-3 text-left"
        >
          <span className="text-sm font-medium text-text-primary">高级</span>
          <span className="text-[11px] text-text-secondary/60">
            {advancedOpen ? '收起' : '展开'}
          </span>
        </button>

        {advancedOpen && (
          <div className="px-4 pb-4 space-y-4 border-t border-surface-hover pt-3">
            <div className="space-y-1">
              <div className="text-[11px] text-text-secondary/70">浏览器程序（留空自动检测）</div>
              <div className="flex items-center gap-2">
                <input
                  value={path}
                  onChange={(e) => setPath(e.target.value)}
                  onBlur={() => void persist({ path })}
                  placeholder={detected ?? '自动检测 Chrome / Edge'}
                  className="flex-1 h-8 rounded-lg bg-background border border-surface-hover text-xs text-text-primary px-2 font-mono focus:outline-none focus:border-primary/50"
                />
                <button
                  onClick={() => {
                    detect();
                    void persist({ path });
                  }}
                  className="h-8 px-2 rounded-lg border border-surface-hover text-[11px] text-text-secondary hover:text-text-primary flex items-center gap-1 shrink-0"
                >
                  <RefreshCw size={11} />
                  检测
                </button>
                {saving && <Loader2 size={12} className="animate-spin text-text-secondary" />}
                {saved && <Check size={12} className="text-accent" />}
              </div>
            </div>

            <div className="space-y-1">
              <div className="text-[11px] text-text-secondary/70">配置目录</div>
              <div className="flex items-center gap-2">
                <div
                  className="flex-1 min-w-0 h-8 flex items-center rounded-lg bg-background border border-surface-hover px-2 text-[11px] font-mono text-text-secondary truncate"
                  title={effectiveDir}
                >
                  {effectiveDir || '默认'}
                </div>
                <button
                  onClick={() => pickProfileDir()}
                  className="h-8 px-2 rounded-lg border border-surface-hover text-[11px] text-text-secondary hover:text-text-primary shrink-0"
                >
                  选择…
                </button>
                <button
                  onClick={() => {
                    fileBrowserOpenInSystem(effectiveDir).catch((e) => setMessage(String(e)));
                  }}
                  className="h-8 px-2 rounded-lg border border-surface-hover text-[11px] text-text-secondary hover:text-text-primary shrink-0"
                  title="在文件管理器中打开"
                >
                  <FolderOpen size={12} />
                </button>
                {profileDir.trim() && (
                  <button
                    onClick={() => {
                      setProfileDir('');
                      void persist({ dir: '' });
                    }}
                    className="h-8 px-2 rounded-lg text-[11px] text-text-secondary hover:text-text-primary shrink-0"
                  >
                    恢复默认
                  </button>
                )}
              </div>
            </div>

            <div className="space-y-1">
              <div className="text-[11px] text-text-secondary/70">自定义启动参数（留空即可）</div>
              <input
                value={launchArgs}
                onChange={(e) => setLaunchArgs(e.target.value)}
                onBlur={() => void persist({ args: launchArgs })}
                placeholder="例如 --proxy-server=http://127.0.0.1:7890"
                className="w-full h-8 rounded-lg bg-background border border-surface-hover text-xs text-text-primary px-2 font-mono focus:outline-none focus:border-primary/50"
              />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
