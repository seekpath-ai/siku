import { useCallback, useEffect, useState } from 'react';
import { Loader2, LogOut, RefreshCw, Smartphone, Trash2, User, KeyRound } from 'lucide-react';
import {
  authLogin,
  authLogout,
  authRegister,
  authStatus,
  deviceList,
  deviceRename,
  deviceRevoke,
  suggestDeviceName,
  type AccountDeviceRow,
  type AuthInfo,
} from '@/lib/tauri';
import { useDialog } from '@/hooks/useDialog';

const SERVER_URL_KEY = 'siku.sync.serverUrl';
const LEGACY_HOST_KEY = 'siku.sync.serverHost';
const LEGACY_PORT_KEY = 'siku.sync.serverPort';

/** 读取服务器地址：优先新 key，其次迁移旧的 host+port 组合。 */
function loadServerUrl(): string {
  const saved = localStorage.getItem(SERVER_URL_KEY);
  if (saved) return saved;
  const host = localStorage.getItem(LEGACY_HOST_KEY);
  const port = localStorage.getItem(LEGACY_PORT_KEY);
  if (host) return `http://${host}${port ? `:${port}` : ''}`;
  return 'http://192.168.21.100:8080';
}

interface Props {
  onLoggedIn: (info: AuthInfo) => void;
}

export function AccountSettings({ onLoggedIn }: Props) {
  // 服务器地址为完整 URL（支持 https://、wss://、ws://、http://），
  // 由后端 normalize_http_base / normalize_ws_url 统一归一化：
  //   wss://relay.example.com        → https://relay.example.com/api/login
  //                                 → wss://relay.example.com/v1/signaling
  //   https://relay.example.com/x    → 自动剥路径、补 /v1/signaling
  const [serverUrl, setServerUrl] = useState(() => loadServerUrl());
  const [auth, setAuth] = useState<AuthInfo | null>(null);
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [deviceName, setDeviceName] = useState('');
  const [suggestedName, setSuggestedName] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [devices, setDevices] = useState<AccountDeviceRow[]>([]);
  const [loadingDevices, setLoadingDevices] = useState(false);

  const httpBase = serverUrl.trim();

  const { prompt } = useDialog();

  // Persist server config.
  useEffect(() => {
    localStorage.setItem(SERVER_URL_KEY, serverUrl);
  }, [serverUrl]);

  useEffect(() => {
    authStatus()
      .then((info) => {
        if (info.access_token && info.user_id) {
          setAuth(info);
          setEmail(info.email);
          onLoggedIn(info);
        }
      })
      .catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Default device name: hostname + first 4 chars of the device id, so two
  // machines never both show up as "我的设备".
  useEffect(() => {
    suggestDeviceName()
      .then(setSuggestedName)
      .catch(() => {});
  }, []);

  const isAuthError = (e: unknown) => {
    const msg = String(e);
    return msg.includes('401') || msg.includes('Unauthorized') || msg.includes('登录已过期');
  };

  const refreshDevices = useCallback(
    async (showLoading = true) => {
      if (!auth?.access_token) return;
      if (showLoading) setLoadingDevices(true);
      try {
        setDevices(await deviceList(httpBase));
      } catch (e) {
        if (isAuthError(e)) {
          await authLogout().catch(() => {});
          setAuth(null);
          setDevices([]);
          setError('登录已过期，请重新登录');
        } else {
          setError(`加载设备列表失败: ${e}`);
        }
      } finally {
        if (showLoading) setLoadingDevices(false);
      }
    },
    [auth, httpBase]
  );

  // 设备列表自动刷新：在线状态与设备名称（改名后）会变化，轮询保持最新
  useEffect(() => {
    if (!auth?.access_token) return;
    refreshDevices();
    const id = window.setInterval(() => refreshDevices(false), 5000);
    return () => window.clearInterval(id);
  }, [auth, refreshDevices]);

  const handleLogin = async () => {
    setBusy(true);
    setError('');
    try {
      // Default device name is suggested as hostname + id suffix so two
      // devices don't both show up as "我的设备".
      const name = deviceName.trim() || suggestedName || '我的设备';
      const info = await authLogin(httpBase, email, password, name);
      setAuth(info);
      onLoggedIn(info);
      refreshDevices();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleRegister = async () => {
    setBusy(true);
    setError('');
    try {
      await authRegister(httpBase, email, password);
      const name = deviceName.trim() || suggestedName || '我的设备';
      const info = await authLogin(httpBase, email, password, name);
      setAuth(info);
      onLoggedIn(info);
      refreshDevices();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleRename = async (deviceId: string, currentName: string) => {
    const next = await prompt('输入新的设备名称：', {
      title: '重命名设备',
      defaultValue: currentName || '我的设备',
      placeholder: '设备名称',
    });
    if (!next || next.trim() === currentName) return;
    setBusy(true);
    try {
      await deviceRename(httpBase, deviceId, next.trim());
      await refreshDevices();
    } catch (e) {
      if (isAuthError(e)) {
        await authLogout().catch(() => {});
        setAuth(null);
        setDevices([]);
        setError('登录已过期，请重新登录');
      } else {
        setError(`改名失败: ${e}`);
      }
    } finally {
      setBusy(false);
    }
  };

  const handleLogout = async () => {
    await authLogout().catch(() => {});
    setAuth(null);
    setDevices([]);
  };

  const handleRevoke = async (deviceId: string) => {
    if (!auth) return;
    setBusy(true);
    try {
      await deviceRevoke(httpBase, deviceId);
      await refreshDevices();
    } catch (e) {
      if (isAuthError(e)) {
        await authLogout().catch(() => {});
        setAuth(null);
        setDevices([]);
        setError('登录已过期，请重新登录');
      } else {
        setError(`吊销失败: ${e}`);
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-3">
      {/* 服务器配置 */}
      <div className="text-sm font-medium text-text-primary">服务器</div>
      <div className="space-y-2">
        <input
          type="text"
          value={serverUrl}
          onChange={(e) => setServerUrl(e.target.value)}
          placeholder="服务器地址，如 wss://relay.example.com"
          className="w-full px-3 py-2 bg-surface border border-surface-hover rounded-lg text-sm text-text-primary outline-none focus:border-primary"
        />
        <div className="text-[11px] text-text-secondary/60 space-y-1">
          <div>
            示例：
            <code className="text-text-secondary">wss://relay.seekpath.com.cn</code>（官方）、
            <code className="text-text-secondary">https://relay.example.com</code>（官方）、
            <code className="text-text-secondary">ws://192.168.21.100:8080</code>（本地/内网）
          </div>
          <div>生产环境用 <code className="text-text-secondary">wss://</code> 或 <code className="text-text-secondary">https://</code> 开头，端口可不填（默认 443）。</div>
        </div>
      </div>

      {!auth ? (
        <div className="space-y-3">
          <div className="text-sm font-medium text-text-primary pt-1">账号登录</div>
          <input
            type="email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            placeholder="邮箱"
            className="w-full px-3 py-2 bg-surface border border-surface-hover rounded-lg text-sm text-text-primary outline-none focus:border-primary"
          />
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder="密码"
            className="w-full px-3 py-2 bg-surface border border-surface-hover rounded-lg text-sm text-text-primary outline-none focus:border-primary"
          />
          <input
            type="text"
            value={deviceName}
            onChange={(e) => setDeviceName(e.target.value)}
            placeholder={`设备名称（默认 ${suggestedName || '…'}）`}
            className="w-full px-3 py-2 bg-surface border border-surface-hover rounded-lg text-sm text-text-primary outline-none focus:border-primary"
          />
          <div className="flex gap-2">
            <button
              onClick={handleLogin}
              disabled={busy || !email || !password || !serverUrl.trim()}
              className="flex items-center gap-2 px-4 py-2 bg-primary text-white text-sm rounded-lg hover:opacity-90 disabled:opacity-50 transition-opacity"
            >
              {busy ? <Loader2 size={14} className="animate-spin" /> : <User size={14} />}
              登录
            </button>
            <button
              onClick={handleRegister}
              disabled={busy || !email || !password || !serverUrl.trim()}
              className="flex items-center gap-2 px-4 py-2 bg-surface border border-surface-hover text-text-primary text-sm rounded-lg hover:bg-surface-hover disabled:opacity-50 transition-colors"
            >
              <KeyRound size={14} />
              注册并登录
            </button>
          </div>
          {error && <div className="text-xs text-red-400">{error}</div>}
        </div>
      ) : (
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <div className="text-sm font-medium text-text-primary">账号</div>
            <button
              onClick={handleLogout}
              className="flex items-center gap-1 text-xs text-text-secondary hover:text-red-400 transition-colors"
            >
              <LogOut size={12} /> 退出登录
            </button>
          </div>
          <div className="px-3 py-2 bg-surface border border-surface-hover rounded-lg text-xs text-text-secondary">
            <div className="flex items-center gap-2">
              <User size={12} className="text-primary" />
              <span className="text-text-primary">{auth.email}</span>
            </div>
            <div className="flex items-center gap-2 mt-1">
              <Smartphone size={12} className="text-primary" />
              <span className="font-mono break-all">{auth.device_id}</span>
            </div>
          </div>

          <div className="text-xs font-medium text-text-secondary">设备列表</div>
          <div className="space-y-2">
            {devices.map((d) => {
              const isSelf = d.device_id === auth.device_id;
              return (
                <div
                  key={d.device_id}
                  className="flex items-center justify-between px-3 py-2 bg-surface border border-surface-hover rounded-lg"
                >
                  <div className="min-w-0">
                    <div className="flex items-center gap-1.5">
                      <span className="text-sm text-text-primary truncate">{d.name || '未命名设备'}</span>
                      {isSelf && (
                        <span className="text-[10px] px-1.5 py-0.5 rounded bg-primary/20 text-primary shrink-0">本机</span>
                      )}
                      {!d.revoked && (
                        <span
                          className={`text-[10px] px-1.5 py-0.5 rounded shrink-0 ${
                            d.online
                              ? 'bg-emerald-500/15 text-emerald-400'
                              : 'bg-surface-hover text-text-secondary/50'
                          }`}
                        >
                          {d.online ? '在线' : '离线'}
                        </span>
                      )}
                    </div>
                    <div className="text-[10px] text-text-secondary/60 font-mono truncate">{d.device_id}</div>
                  </div>
                  <div className="flex items-center gap-2 shrink-0">
                    {d.revoked ? (
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-red-500/10 text-red-400">已吊销</span>
                    ) : (
                      <>
                        <button
                          onClick={() => handleRename(d.device_id, d.name)}
                          disabled={busy}
                          title="重命名设备"
                          className="flex items-center gap-1 text-[10px] text-text-secondary hover:text-primary disabled:opacity-30 transition-colors"
                        >
                          <RefreshCw size={11} /> 改名
                        </button>
                        {!isSelf && (
                          <button
                            onClick={() => handleRevoke(d.device_id)}
                            disabled={busy}
                            title="吊销该设备的登录"
                            className="flex items-center gap-1 text-[10px] text-text-secondary hover:text-red-400 disabled:opacity-30 transition-colors"
                          >
                            <Trash2 size={11} /> 吊销
                          </button>
                        )}
                      </>
                    )}
                  </div>
                </div>
              );
            })}
            {devices.length === 0 && !loadingDevices && (
              <div className="text-xs text-text-secondary/60">暂无设备，登录后将自动注册</div>
            )}
            {loadingDevices && (
              <div className="flex items-center gap-2 text-xs text-text-secondary/60">
                <Loader2 size={12} className="animate-spin" /> 加载中…
              </div>
            )}
          </div>
          {error && <div className="text-xs text-red-400">{error}</div>}
        </div>
      )}
    </div>
  );
}
