import { useCallback, useEffect, useState } from 'react';
import {
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Cloud,
  Loader2,
  LogOut,
  RefreshCw,
  Smartphone,
  Trash2,
  User,
  KeyRound,
  X,
  Zap,
} from 'lucide-react';
import {
  authLogin,
  authLogout,
  authRegister,
  authStatus,
  deviceList,
  deviceRename,
  deviceRemove,
  storageOrderCreate,
  storageOrderList,
  storagePlans,
  storageStatus,
  suggestDeviceName,
  type AccountDeviceRow,
  type AuthInfo,
  type StorageOrder,
  type StorageOrderCreateResult,
  type StoragePlan,
  type StorageStatus,
} from '@/lib/tauri';
import { useDialog } from '@/hooks/useDialog';

/** 用量字节数转人类可读单位（GB/MB/KB）。 */
function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '0 B';
  if (bytes >= 1 << 30) return `${(bytes / (1 << 30)).toFixed(1)} GB`;
  if (bytes >= 1 << 20) return `${(bytes / (1 << 20)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${bytes} B`;
}

/** RFC3339 时间转本地日期时间；解析失败时原样返回。 */
function formatDateTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

const ORDER_STATUS_VIEW: Record<StorageOrder['status'], { label: string; cls: string }> = {
  pending: { label: '审核中', cls: 'bg-amber-500/15 text-amber-400' },
  paid: { label: '已开通', cls: 'bg-emerald-500/15 text-emerald-400' },
  rejected: { label: '已拒绝', cls: 'bg-red-500/15 text-red-400' },
  cancelled: { label: '已取消', cls: 'bg-surface-hover text-text-secondary/50' },
};

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

  // 云端存储（配额 / 套餐 / 扩容订单）
  const [storage, setStorage] = useState<StorageStatus | null>(null);
  const [storageOrders, setStorageOrders] = useState<StorageOrder[]>([]);
  const [loadingStorage, setLoadingStorage] = useState(false);
  const [planModalOpen, setPlanModalOpen] = useState(false);
  const [plans, setPlans] = useState<StoragePlan[]>([]);
  const [loadingPlans, setLoadingPlans] = useState(false);
  const [selectedPlanId, setSelectedPlanId] = useState('');
  const [orderPeriod, setOrderPeriod] = useState<'month' | 'year'>('year');
  const [ordering, setOrdering] = useState(false);
  const [orderResult, setOrderResult] = useState<StorageOrderCreateResult | null>(null);
  // 待支付订单的付款方式弹窗 / 历史订单折叠
  const [paymentOrder, setPaymentOrder] = useState<StorageOrder | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);

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

  const refreshStorage = useCallback(
    async (showLoading = true) => {
      if (!auth?.access_token) return;
      if (showLoading) setLoadingStorage(true);
      try {
        const [status, orders] = await Promise.all([
          storageStatus(httpBase),
          storageOrderList(httpBase),
        ]);
        setStorage(status);
        setStorageOrders(orders);
      } catch (e) {
        if (isAuthError(e)) {
          await authLogout().catch(() => {});
          setAuth(null);
          setDevices([]);
          setError('登录已过期，请重新登录');
        } else {
          setError(`加载云端存储信息失败: ${e}`);
        }
      } finally {
        if (showLoading) setLoadingStorage(false);
      }
    },
    [auth, httpBase]
  );

  // 进入设置页 / 登录状态变化时加载云端存储用量与订单；退出登录后清空
  useEffect(() => {
    if (!auth?.access_token) {
      setStorage(null);
      setStorageOrders([]);
      setPlanModalOpen(false);
      setOrderResult(null);
      setPaymentOrder(null);
      return;
    }
    refreshStorage();
  }, [auth, refreshStorage]);

  const openPlanModal = async () => {
    setPlanModalOpen(true);
    setOrderResult(null);
    setSelectedPlanId('');
    setLoadingPlans(true);
    try {
      const list = await storagePlans(httpBase);
      // 只展示付费套餐（free 免费档不参与扩容选择）
      setPlans(list.filter((p) => p.id !== 'free'));
    } catch (e) {
      setError(`加载套餐失败: ${e}`);
      setPlanModalOpen(false);
    } finally {
      setLoadingPlans(false);
    }
  };

  const handleCreateOrder = async () => {
    if (!selectedPlanId) return;
    setOrdering(true);
    try {
      const result = await storageOrderCreate(httpBase, selectedPlanId, orderPeriod);
      setOrderResult(result);
      await refreshStorage(false);
    } catch (e) {
      setError(`提交扩容申请失败: ${e}`);
    } finally {
      setOrdering(false);
    }
  };

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

  const handleRemove = async (deviceId: string) => {
    if (!auth) return;
    setBusy(true);
    try {
      await deviceRemove(httpBase, deviceId);
      await refreshDevices();
    } catch (e) {
      if (isAuthError(e)) {
        await authLogout().catch(() => {});
        setAuth(null);
        setDevices([]);
        setError('登录已过期，请重新登录');
      } else {
        setError(`移除失败: ${e}`);
      }
    } finally {
      setBusy(false);
    }
  };

  // 套餐显示名：优先用已加载的套餐表，未加载时回退到 plan_id
  const planNameOf = (planId: string | null | undefined) => {
    if (!planId || planId === 'free') return '免费版';
    return plans.find((p) => p.id === planId)?.name ?? planId;
  };

  const storagePct =
    storage && storage.quota_bytes > 0
      ? Math.min(100, Math.round((storage.used_bytes / storage.quota_bytes) * 100))
      : 0;
  // 使用率 >90% 黄色预警，已满红色
  const storageBarCls = storagePct >= 100 ? 'bg-red-500' : storagePct > 90 ? 'bg-amber-400' : 'bg-primary';
  const storagePctCls =
    storagePct >= 100 ? 'text-red-400' : storagePct > 90 ? 'text-amber-400' : 'text-text-secondary';

  // 待支付订单单独置顶展示；已完结的订单折叠进「历史订单」。
  const pendingOrders = storageOrders
    .filter((o) => o.status === 'pending')
    .sort((a, b) => b.created_at.localeCompare(a.created_at));
  const historyOrders = storageOrders
    .filter((o) => o.status !== 'pending')
    .sort((a, b) => b.created_at.localeCompare(a.created_at));

  const selectedPlan = plans.find((p) => p.id === selectedPlanId) ?? null;

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
                      <span
                        className={`text-[10px] px-1.5 py-0.5 rounded shrink-0 ${
                          d.online
                            ? 'bg-emerald-500/15 text-emerald-400'
                            : 'bg-surface-hover text-text-secondary/50'
                        }`}
                      >
                        {d.online ? '在线' : '离线'}
                      </span>
                    </div>
                    <div className="text-[10px] text-text-secondary/60 font-mono truncate">{d.device_id}</div>
                  </div>
                  <div className="flex items-center gap-2 shrink-0">
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
                        onClick={() => handleRemove(d.device_id)}
                        disabled={busy}
                        title="移除该设备，其登录立即失效"
                        className="flex items-center gap-1 text-[10px] text-text-secondary hover:text-red-400 disabled:opacity-30 transition-colors"
                      >
                        <Trash2 size={11} /> 移除
                      </button>
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

          {/* 云端存储 */}
          <div className="flex items-center justify-between pt-1">
            <div className="text-xs font-medium text-text-secondary">云端存储</div>
            <div className="flex items-center gap-2">
              <button
                onClick={() => refreshStorage()}
                disabled={loadingStorage}
                title="刷新用量与订单"
                className="flex items-center gap-1 text-[10px] text-text-secondary hover:text-primary disabled:opacity-30 transition-colors"
              >
                {loadingStorage ? <Loader2 size={11} className="animate-spin" /> : <RefreshCw size={11} />} 刷新
              </button>
              <button
                onClick={openPlanModal}
                className="flex items-center gap-1 text-[10px] text-text-secondary hover:text-primary transition-colors"
              >
                <Zap size={11} /> 扩容
              </button>
            </div>
          </div>
          {storage ? (
            <div className="px-3 py-2 bg-surface border border-surface-hover rounded-lg space-y-1.5">
              <div className="flex items-center justify-between text-xs">
                <span className="flex items-center gap-1.5 text-text-primary">
                  <Cloud size={12} className="text-primary" />
                  已用 {formatBytes(storage.used_bytes)} / {formatBytes(storage.quota_bytes)}
                </span>
                <span className={storagePctCls}>{storagePct}%</span>
              </div>
              <div className="h-1.5 rounded-full bg-surface-hover overflow-hidden">
                <div className={`h-full rounded-full ${storageBarCls}`} style={{ width: `${storagePct}%` }} />
              </div>
              <div className="text-[11px] text-text-secondary/60">
                当前套餐：{planNameOf(storage.plan_id)}
                {storage.expires_at
                  ? ` · ${formatDateTime(storage.expires_at)} 到期`
                  : storage.plan_id && storage.plan_id !== 'free'
                    ? ' · 永久'
                    : ''}
              </div>
            </div>
          ) : (
            loadingStorage && (
              <div className="flex items-center gap-2 text-xs text-text-secondary/60">
                <Loader2 size={12} className="animate-spin" /> 加载中…
              </div>
            )
          )}
          {pendingOrders.map((o) => (
            <div
              key={o.id}
              className="flex items-center justify-between gap-2 px-3 py-2 bg-amber-500/10 border border-amber-500/40 rounded-lg"
            >
              <div className="min-w-0">
                <div className="text-xs text-text-primary font-medium">
                  {planNameOf(o.plan_id)} · ¥{o.amount_cny}
                </div>
                <div className="text-[10px] text-text-secondary/60">
                  <span className="font-mono">{o.id.slice(0, 8)}…</span> · {formatDateTime(o.created_at)}
                </div>
              </div>
              <button
                onClick={() => setPaymentOrder(o)}
                className="shrink-0 px-2.5 py-1 text-[11px] rounded-md bg-amber-500/20 text-amber-300 hover:bg-amber-500/30 transition-colors"
              >
                查看付款方式
              </button>
            </div>
          ))}
          {historyOrders.length > 0 && (
            <div className="space-y-1.5">
              <button
                onClick={() => setHistoryOpen((v) => !v)}
                className="flex items-center gap-1 text-[11px] text-text-secondary hover:text-text-primary transition-colors"
              >
                {historyOpen ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                历史订单（{historyOrders.length}）
              </button>
              {historyOpen &&
                historyOrders.map((o) => {
                  const view = ORDER_STATUS_VIEW[o.status] ?? ORDER_STATUS_VIEW.cancelled;
                  return (
                    <div
                      key={o.id}
                      className="flex items-center justify-between px-3 py-1.5 bg-surface/60 border border-surface-hover rounded-lg"
                    >
                      <div className="min-w-0">
                        <div className="text-xs text-text-primary">
                          {planNameOf(o.plan_id)} · ¥{o.amount_cny}
                        </div>
                        <div className="text-[10px] text-text-secondary/60">
                          <span className="font-mono">{o.id.slice(0, 8)}…</span> · {formatDateTime(o.created_at)}
                        </div>
                      </div>
                      <span className={`text-[10px] px-1.5 py-0.5 rounded shrink-0 ${view.cls}`}>{view.label}</span>
                    </div>
                  );
                })}
            </div>
          )}
          {error && <div className="text-xs text-red-400">{error}</div>}
        </div>
      )}

      {/* 扩容套餐选择弹窗（项目无通用卡片弹窗组件，按本文件风格条件渲染） */}
      {planModalOpen && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
          onClick={() => !ordering && setPlanModalOpen(false)}
        >
          <div
            className="w-full max-w-md max-h-[85vh] overflow-y-auto bg-surface border border-surface-hover rounded-xl p-4 space-y-3"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center justify-between">
              <div className="text-sm font-medium text-text-primary">扩容云端存储</div>
              <button
                onClick={() => setPlanModalOpen(false)}
                disabled={ordering}
                className="text-text-secondary hover:text-text-primary disabled:opacity-30 transition-colors"
              >
                <X size={14} />
              </button>
            </div>

            {orderResult ? (
              <div className="space-y-3">
                <div className="flex items-center gap-2 text-sm text-emerald-400">
                  <CheckCircle2 size={14} /> 申请已提交，等待管理员确认收款
                </div>
                <div className="px-3 py-2 border border-surface-hover rounded-lg space-y-1.5">
                  <div className="text-xs text-text-secondary">
                    订单号：<span className="font-mono text-text-primary break-all">{orderResult.order_id}</span>
                  </div>
                  <div className="text-xs text-text-secondary">
                    金额：<span className="text-text-primary">¥{orderResult.amount_cny}</span>
                  </div>
                  <div className="text-xs text-text-primary whitespace-pre-wrap">{orderResult.payment_info}</div>
                </div>
                <div className="text-[11px] text-amber-400">
                  转账时请备注订单号，管理员确认收款后将自动开通对应套餐。
                </div>
                <button
                  onClick={() => setPlanModalOpen(false)}
                  className="w-full px-4 py-2 bg-primary text-white text-sm rounded-lg hover:opacity-90 transition-opacity"
                >
                  完成
                </button>
              </div>
            ) : (
              <>
                {loadingPlans ? (
                  <div className="flex items-center gap-2 text-xs text-text-secondary/60">
                    <Loader2 size={12} className="animate-spin" /> 加载套餐…
                  </div>
                ) : (
                  <div className="space-y-2">
                    {plans.map((p) => {
                      const selected = p.id === selectedPlanId;
                      return (
                        <button
                          key={p.id}
                          onClick={() => setSelectedPlanId(p.id)}
                          className={`w-full flex items-center justify-between px-3 py-2.5 border rounded-lg text-left transition-colors ${
                            selected
                              ? 'border-primary bg-primary/10'
                              : 'border-surface-hover hover:border-primary/50'
                          }`}
                        >
                          <div className="min-w-0">
                            <div className="text-sm text-text-primary font-medium">{p.name}</div>
                            <div className="text-[11px] text-text-secondary/60">{formatBytes(p.quota_bytes)}</div>
                          </div>
                          <div className="text-right shrink-0">
                            <div className="text-xs text-text-primary">¥{p.monthly_cny}/月</div>
                            <div className="text-[11px] text-text-secondary/60">¥{p.yearly_cny}/年</div>
                          </div>
                        </button>
                      );
                    })}
                    {plans.length === 0 && (
                      <div className="text-xs text-text-secondary/60">暂无可选套餐</div>
                    )}
                  </div>
                )}

                <div className="flex gap-1 p-1 border border-surface-hover rounded-lg">
                  {(['month', 'year'] as const).map((per) => (
                    <button
                      key={per}
                      onClick={() => setOrderPeriod(per)}
                      className={`flex-1 py-1.5 text-xs rounded-md transition-colors ${
                        orderPeriod === per
                          ? 'bg-primary/20 text-primary font-medium'
                          : 'text-text-secondary hover:bg-surface-hover'
                      }`}
                    >
                      {per === 'month' ? '月付' : '年付（约 8.3 折）'}
                    </button>
                  ))}
                </div>

                <button
                  onClick={handleCreateOrder}
                  disabled={!selectedPlan || ordering}
                  className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-primary text-white text-sm rounded-lg hover:opacity-90 disabled:opacity-50 transition-opacity"
                >
                  {ordering ? <Loader2 size={14} className="animate-spin" /> : <Zap size={14} />}
                  {selectedPlan
                    ? `提交申请（¥${orderPeriod === 'month' ? selectedPlan.monthly_cny : selectedPlan.yearly_cny}/${
                        orderPeriod === 'month' ? '月' : '年'
                      }）`
                    : '请选择套餐'}
                </button>
                <div className="text-[11px] text-text-secondary/60">
                  提交后按展示的收款信息线下转账，转账时请备注订单号。
                </div>
              </>
            )}
          </div>
        </div>
      )}

      {/* 待支付订单的付款方式弹窗（与扩容弹窗同风格条件渲染） */}
      {paymentOrder && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
          onClick={() => setPaymentOrder(null)}
        >
          <div
            className="w-full max-w-md bg-surface border border-surface-hover rounded-xl p-4 space-y-3"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center justify-between">
              <div className="text-sm font-medium text-text-primary">付款方式</div>
              <button
                onClick={() => setPaymentOrder(null)}
                className="text-text-secondary hover:text-text-primary transition-colors"
              >
                <X size={14} />
              </button>
            </div>
            <div className="px-3 py-2 border border-surface-hover rounded-lg space-y-1.5">
              <div className="text-xs text-text-secondary">
                订单号：<span className="font-mono text-text-primary break-all select-all">{paymentOrder.id}</span>
              </div>
              <div className="text-xs text-text-secondary">
                金额：<span className="text-text-primary">¥{paymentOrder.amount_cny}</span>
              </div>
              <div className="text-xs text-text-primary whitespace-pre-wrap">
                {paymentOrder.payment_info || '（收款信息未配置，请联系管理员）'}
              </div>
            </div>
            <div className="text-[11px] text-amber-400">
              转账时请备注订单号，管理员确认收款后将自动开通对应套餐。
            </div>
            <button
              onClick={() => setPaymentOrder(null)}
              className="w-full px-4 py-2 bg-primary text-white text-sm rounded-lg hover:opacity-90 transition-opacity"
            >
              完成
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
