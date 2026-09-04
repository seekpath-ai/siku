import { useState, useEffect, useCallback, useRef } from 'react';
import {
  Loader2, RefreshCw, Download, Upload, Link2,
  ScanLine, X, Wifi, Globe, Network, Archive, CheckCircle2, AlertCircle,
} from 'lucide-react';
import {
  getDeviceId,
  startLocalHost,
  stopLocalHost,
  connectLocalHost,
  syncOnce,
  exportEncryptedSeed,
  importEncryptedSeed,
  startLanBeacon,
  stopLanBeacon,
  startLanDiscovery,
  stopLanDiscovery,
  getLanPeers,
  getSyncStatus,
  getLanHostActive,
  stopLocalSession,
  getSyncConfig,
  flushSyncOutbox,
  getSyncOutboxCount,
  setSyncConfig as setRemoteSyncConfig,
  type LanPeerInfo,
  type SyncStatus,
  type SyncConfig,
} from '@/lib/tauri';
import { AccountSettings } from './AccountSettings';
import { useDialog } from '@/hooks/useDialog';

type SyncTab = 'cloud' | 'lan' | 'offline';

export function SyncSettings() {
  const [tab, setTab] = useState<SyncTab>('cloud');

  const [syncing, setSyncing] = useState(false);

  // LAN
  const [lanRole, setLanRole] = useState<'host' | 'guest'>(() => {
    const v = localStorage.getItem('siku.lan.role');
    return v === 'guest' ? 'guest' : 'host';
  });
  // 等待状态持久化：离开页面再回来时保持 host 等待/已连接状态，
  // 不需要重新连接（后端连接持续存在）
  const [lanHosting, setLanHosting] = useState(() => localStorage.getItem('siku.lan.hosting') === '1');
  const [localPairCode, setLocalPairCode] = useState(() => localStorage.getItem('siku.lan.paircode') ?? '');
  const [lanDiscovering, setLanDiscovering] = useState(false);
  const [lanPeers, setLanPeers] = useState<LanPeerInfo[]>([]);

  // Offline seed
  const [seedPassword, setSeedPassword] = useState('');
  const [seedAction, setSeedAction] = useState<'idle' | 'exporting' | 'importing'>('idle');
  const [cloudResult, setCloudResult] = useState<string>('');
  const [offlineResult, setOfflineResult] = useState<string>('');

  // Status
  const [syncStatus, setSyncStatus] = useState<SyncStatus>({ connected: false });
  const [syncConfig, setSyncConfig] = useState<SyncConfig>({
    sync_optional_data: true,
    allow_plaintext_relay: false,
  });
  const [configSaving, setConfigSaving] = useState(false);
  const [outboxCount, setOutboxCount] = useState(0);
  const [flushingOutbox, setFlushingOutbox] = useState(false);

  const { prompt } = useDialog();

  // LAN guest: 扫描到设备后点击即连接。配对码由用户在弹窗中手动输入
  // （以提供方设备上显示的为准），不再直接展示/自动使用 beacon 里的码。
  // 配对码错误时 host 协议层会 Reject（"配对码不一致"），这里展示报错。
  const handlePeerConnect = useCallback(async (peer: LanPeerInfo) => {
    const code = await prompt(
      `设备 ${peer.addr} 上显示的配对码（请在提供方设备上查看），输入后连接`,
      { title: '连接设备', placeholder: '6 位配对码' }
    );
    if (!code) return; // 用户取消
    try {
      const ip = peer.addr.split(':')[0];
      await connectLocalHost(ip, peer.device_id, code.trim());
      setLanDiscovering(false);
    } catch (err) {
      console.error('Failed to connect local host:', err);
      alert(`连接失败: ${err}`);
    }
  }, [prompt]);

  useEffect(() => {
    getSyncConfig()
      .then(setSyncConfig)
      .catch((err) => console.error('Failed to load sync config:', err));
    // Resume LAN hosting if the UI state says so but the backend loop is gone
    // (e.g. after an app restart). Page navigation keeps the loop alive, so
    // nothing needs to be done on a plain return to the page.
    if (localStorage.getItem('siku.lan.hosting') === '1') {
      getLanHostActive()
        .then(async (active) => {
          if (active) return false;
          const code = localStorage.getItem('siku.lan.paircode') ?? '';
          if (!code) return false;
          const deviceId = await getDeviceId();
          await Promise.all([
            startLocalHost(code),
            startLanBeacon({ device_id: deviceId, pairing_payload: '' }),
          ]);
          return true;
        })
        .catch((err) => console.error('Failed to resume LAN hosting:', err));
    }
    return () => {
      // Keep the beacon alive when leaving the page while hosting — the host
      // loop keeps running in the background, so stopping the broadcast here
      // would leave the UI state (and the pairing code) inconsistent with
      // reality. Only the transient scan is torn down.
      stopLanDiscovery().catch(() => {});
    };
  }, []);

  // Poll sync status while on the page, scoped to the active tab's session
  // kind: LAN and cloud sessions are independent engines, so each tab only
  // sees its own status.
  const prevLanConnected = useRef(false);
  useEffect(() => {
    const refresh = () => {
      const kind = tab === 'lan' ? 'lan' : tab === 'cloud' ? 'cloud' : undefined;
      getSyncStatus(kind)
        .then((status) => {
          setSyncStatus(status);
          if (tab === 'lan') {
            const connectedNow = !!(status.connected && status.kind === 'lan');
            if (prevLanConnected.current && !connectedNow) {
              // 会话断开（对端断开/停止等待/连接被关闭）：清掉旧设备列表，
              // 避免发现区域残留上一次连接的设备误导用户。
              setLanPeers([]);
            }
            prevLanConnected.current = connectedNow;
          }
        })
        .catch((err) => console.error('Failed to load sync status:', err));
      if (tab === 'cloud') {
        getSyncOutboxCount()
          .then(setOutboxCount)
          .catch(() => {});
      }
    };
    refresh();
    const id = window.setInterval(refresh, 2000);
    return () => window.clearInterval(id);
  }, [tab]);





  const handleSyncOnce = useCallback(async () => {
    setSyncing(true);
    try {
      const msg = await syncOnce('cloud');
      setCloudResult(msg);
      const status = await getSyncStatus('cloud');
      setSyncStatus(status);
    } catch (err) {
      console.error('Failed to sync:', err);
      alert(`同步失败: ${err}`);
    } finally {
      setSyncing(false);
    }
  }, []);

  const handleFlushOutbox = useCallback(async () => {
    setFlushingOutbox(true);
    try {
      await flushSyncOutbox();
      const count = await getSyncOutboxCount();
      setOutboxCount(count);
    } catch (err) {
      console.error('Failed to flush outbox:', err);
      alert(`邮箱重试失败: ${err}`);
    } finally {
      setFlushingOutbox(false);
    }
  }, []);

  const handleToggleOptionalSync = useCallback(async () => {
    const next = {
      ...syncConfig,
      sync_optional_data: !syncConfig.sync_optional_data,
    };
    setSyncConfig(next);
    setConfigSaving(true);
    try {
      await setRemoteSyncConfig(next);
    } catch (err) {
      console.error('Failed to save sync config:', err);
      alert(`保存同步设置失败: ${err}`);
      setSyncConfig({
        ...syncConfig,
        sync_optional_data: !next.sync_optional_data,
      });
    } finally {
      setConfigSaving(false);
    }
  }, [syncConfig]);

  const handleTogglePlaintextRelay = useCallback(async () => {
    const next = {
      ...syncConfig,
      allow_plaintext_relay: !syncConfig.allow_plaintext_relay,
    };
    setSyncConfig(next);
    setConfigSaving(true);
    try {
      await setRemoteSyncConfig(next);
    } catch (err) {
      console.error('Failed to save sync config:', err);
      alert(`保存同步设置失败: ${err}`);
      setSyncConfig({
        ...syncConfig,
        allow_plaintext_relay: !next.allow_plaintext_relay,
      });
    } finally {
      setConfigSaving(false);
    }
  }, [syncConfig]);

  const handleExportSeed = useCallback(async () => {
    if (!seedPassword) return;
    setSeedAction('exporting');
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const path = await save({
        filters: [{ name: 'Encrypted seed', extensions: ['zip'] }],
        defaultPath: 'siku-seed.zip',
      });
      if (!path) {
        setSeedAction('idle');
        return;
      }
      await exportEncryptedSeed({ archive_path: path, password: seedPassword });
      setOfflineResult(`已导出到 ${path}`);
    } catch (err) {
      console.error('Failed to export seed:', err);
      alert(`导出失败: ${err}`);
    } finally {
      setSeedAction('idle');
    }
  }, [seedPassword]);

  const handleImportSeed = useCallback(async () => {
    if (!seedPassword) return;
    setSeedAction('importing');
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({
        filters: [{ name: 'Encrypted seed', extensions: ['zip'] }],
      });
      if (!selected || Array.isArray(selected)) {
        setSeedAction('idle');
        return;
      }
      const target = await importEncryptedSeed({
        archive_path: selected,
        password: seedPassword,
      });
      setOfflineResult(`已导入到 ${target}，请重启应用以生效`);
    } catch (err) {
      console.error('Failed to import seed:', err);
      alert(`导入失败: ${err}`);
    } finally {
      setSeedAction('idle');
    }
  }, [seedPassword]);

  // ── User-facing sync status (cloud: 五态；LAN: 见 lanStateView) ──
  const fmtSyncTime = (iso?: string) => {
    if (!iso) return '';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    const pad = (n: number) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  };
  // 云端「已同步」行只显示时刻（HH:MM），日期由上下文自明。
  const fmtSyncClock = (iso?: string) => {
    if (!iso) return '';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    const pad = (n: number) => String(n).padStart(2, '0');
    return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  };
  // Cloud-state helpers: 「已同步」= 本机已获取其它所有设备当前可见（云端存档
  // 或对端直连）的全部变更，与传输方式无关；云端连接状态看进程级
  // relay_connected（auto-sync proxy 的 discovery 连接），不看 engine 会话
  // （无 P2P 会话时纯邮箱路径也在同步）。LAN 会话存活期间 relay_connected
  // 同样可能为 true，两个 tab 的状态互不冲突。
  const isCloudConnected = !!syncStatus.relay_connected;
  const isLanConnected = syncStatus.connected && syncStatus.kind === 'lan';

  const syncStateView = (() => {
    if (syncing) {
      return {
        icon: <Loader2 size={14} className="animate-spin text-primary" />,
        text: '同步中…',
        cls: 'text-primary',
      };
    }
    if (syncStatus.last_error?.includes('quota_exceeded')) {
      return {
        icon: <AlertCircle size={14} className="text-amber-400" />,
        text: '云端存储已满，新更改暂存本地，扩容后自动恢复',
        cls: 'text-amber-400',
      };
    }
    if (syncStatus.last_error) {
      return {
        icon: <AlertCircle size={14} className="text-red-400" />,
        text: '同步失败，点击重试',
        cls: 'text-red-400',
      };
    }
    if (isCloudConnected && syncStatus.last_sync_at) {
      const counters: string[] = [];
      if (syncStatus.pushed) counters.push(`推 ${syncStatus.pushed}`);
      if (syncStatus.pulled) counters.push(`拉 ${syncStatus.pulled}`);
      const tail = counters.length ? ` · ${counters.join(' · ')}` : '';
      return {
        icon: <CheckCircle2 size={14} className="text-accent" />,
        text: `已同步 ${fmtSyncClock(syncStatus.last_sync_at)}${tail}`,
        cls: 'text-accent',
      };
    }
    if (isCloudConnected) {
      return {
        icon: <Loader2 size={14} className="animate-spin text-accent" />,
        text: '已连接，同步中…',
        cls: 'text-accent',
      };
    }
    return {
      icon: <span className="w-2.5 h-2.5 rounded-full bg-text-secondary/40" />,
      text: '未连接云端',
      cls: 'text-text-secondary',
    };
  })();

  // LAN 同步状态视图：单一状态行覆盖全部状态（等待连接 / 已连接 /
  // 已同步 / 失败 / 未同步），避免页面里多处重复显示同步状态。
  const lanStateView = (() => {
    if (syncStatus.last_error) {
      return {
        icon: <AlertCircle size={14} className="text-red-400" />,
        text: `同步失败：${syncStatus.last_error}`,
        cls: 'text-red-400',
      };
    }
    if (isLanConnected && syncStatus.last_sync_at) {
      const counters: string[] = [];
      if (syncStatus.pushed) counters.push(`推 ${syncStatus.pushed}`);
      if (syncStatus.pulled) counters.push(`拉 ${syncStatus.pulled}`);
      const tail = counters.length ? ` · ${counters.join(' · ')}` : '';
      return {
        icon: <CheckCircle2 size={14} className="text-accent" />,
        text: `已同步 ${fmtSyncTime(syncStatus.last_sync_at)}${tail}`,
        cls: 'text-accent',
      };
    }
    if (isLanConnected) {
      return {
        icon: <CheckCircle2 size={14} className="text-accent" />,
        text: '已连接，正在同步…',
        cls: 'text-accent',
      };
    }
    if (lanRole === 'host' && lanHosting) {
      return {
        icon: <Wifi size={14} className="animate-pulse" />,
        text: '等待局域网设备连接…',
        cls: 'text-text-secondary',
      };
    }
    return {
      icon: <span className="w-2.5 h-2.5 rounded-full bg-text-secondary/40" />,
      text: '未同步',
      cls: 'text-text-secondary',
    };
  })();

  const tabBtn = (t: SyncTab, label: string, icon: React.ReactNode, desc: string) => (
    <button
      onClick={() => setTab(t)}
      className={`flex-1 flex flex-col items-center gap-0.5 py-3 rounded-lg transition-colors ${
        tab === t ? 'bg-primary/15 text-primary' : 'text-text-secondary hover:bg-surface-hover'
      }`}
    >
      <span className="flex items-center gap-1.5 text-sm font-medium">
        {icon}
        {label}
      </span>
      <span className={`text-[10px] ${tab === t ? 'text-primary/70' : 'text-text-secondary/50'}`}>
        {desc}
      </span>
    </button>
  );

  const roleTabBtn = (active: boolean) =>
    `flex-1 py-2 text-sm rounded-lg transition-colors ${
      active ? 'bg-primary/20 text-primary font-medium' : 'text-text-secondary hover:bg-surface-hover'
    }`;

  return (
    <div className="space-y-6">
      <h2 className="text-lg font-semibold text-text-primary">同步</h2>

      {/* 三种同步方式 */}
      <div className="flex gap-1 p-1 bg-surface border border-surface-hover rounded-xl">
        {tabBtn('cloud', '公网同步', <Globe size={14} />, '跨网络 · 需登录')}
        {tabBtn('lan', '局域网同步', <Network size={14} />, '同网络 · 无需登录')}
        {tabBtn('offline', '离线同步', <Archive size={14} />, '物理搬运 · 无需网络')}
      </div>

      {/* 同步范围：全局设置，对公网与局域网同步均生效 */}
      <div className="space-y-3">
        <div className="text-sm font-medium text-text-primary">同步范围</div>
        <div className="flex items-center justify-between px-4 py-3 bg-surface border border-surface-hover rounded-xl">
          <div>
            <div className="text-sm text-text-primary">同步聊天记录</div>
            <div className="text-xs text-text-secondary">
              启用后同步 chat_sessions、chat_messages 等可选表；含密钥的全局设置不参与同步
            </div>
          </div>
          <button
            role="switch"
            aria-checked={syncConfig.sync_optional_data}
            disabled={configSaving}
            onClick={handleToggleOptionalSync}
            className={`w-9 h-5 rounded-full transition-colors shrink-0 disabled:opacity-50 ${
              syncConfig.sync_optional_data ? 'bg-primary' : 'bg-surface-hover'
            }`}
          >
            <span
              className={`block w-4 h-4 rounded-full bg-white shadow transition-transform ${
                syncConfig.sync_optional_data ? 'translate-x-[18px]' : 'translate-x-0.5'
              }`}
            />
          </button>
        </div>

        <div className="flex items-center justify-between px-4 py-3 bg-surface border border-surface-hover rounded-xl">
          <div>
            <div className="text-sm text-text-primary">允许不加密的中继连接（ws://）</div>
            <div className="text-xs text-text-secondary">
              中继连接默认要求加密（wss://）；公网明文传输会暴露账号令牌，仅自建局域网中继调试时开启
            </div>
          </div>
          <button
            role="switch"
            aria-checked={syncConfig.allow_plaintext_relay}
            disabled={configSaving}
            onClick={handleTogglePlaintextRelay}
            className={`w-9 h-5 rounded-full transition-colors shrink-0 disabled:opacity-50 ${
              syncConfig.allow_plaintext_relay ? 'bg-red-500/80' : 'bg-surface-hover'
            }`}
          >
            <span
              className={`block w-4 h-4 rounded-full bg-white shadow transition-transform ${
                syncConfig.allow_plaintext_relay ? 'translate-x-[18px]' : 'translate-x-0.5'
              }`}
            />
          </button>
        </div>
      </div>

      {/* ═══════════ 公网同步 ═══════════ */}
      {tab === 'cloud' && (
        <div className="space-y-6">
          <AccountSettings onLoggedIn={() => {}} />

          {/* 同步状态（云端五态：存储已满 / 失败 / 已同步 / 同步中 / 未连接） */}
          <div className="space-y-3">
            <div className="text-sm font-medium text-text-primary">同步状态</div>
            <div className="flex items-center justify-between px-4 py-3 bg-surface border border-surface-hover rounded-xl">
              <div className="flex items-center gap-2">
                {syncStateView.icon}
                <span className={`text-sm ${syncStateView.cls}`}>{syncStateView.text}</span>
              </div>
              <div className="flex items-center gap-2">
                <button
                  onClick={handleSyncOnce}
                  disabled={syncing}
                  className="flex items-center gap-1.5 px-3 py-1.5 bg-surface border border-surface-hover text-text-primary text-xs rounded-lg hover:bg-surface-hover disabled:opacity-50 transition-colors"
                >
                  <RefreshCw size={12} />
                  立即同步
                </button>
              </div>
            </div>
            {cloudResult && <div className="text-xs text-accent">{cloudResult}</div>}

            {/* 邮箱待投递（对端不在线时的离线通道） */}
            {outboxCount > 0 && (
              <div className="flex items-center justify-between px-4 py-2 bg-surface/60 border border-surface-hover rounded-lg">
                <span className="text-xs text-text-secondary">
                  本地有 {outboxCount} 条待同步变更（对端上线后自动投递）
                </span>
                <button
                  onClick={handleFlushOutbox}
                  disabled={flushingOutbox}
                  className="flex items-center gap-1.5 px-2.5 py-1 bg-surface border border-surface-hover text-text-primary text-xs rounded-lg hover:bg-surface-hover disabled:opacity-50 transition-colors"
                >
                  {flushingOutbox ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />}
                  立即重试
                </button>
              </div>
            )}
          </div>
        </div>
      )}

      {/* ═══════════ 局域网同步 ═══════════ */}
      {tab === 'lan' && (
        <div className="space-y-4">
          {/* 局域网同步状态：单一状态行（等待连接 / 已连接 / 已同步 /
              失败 / 未同步），连接时附带断开操作按钮。只认 lan 会话——
              公网会话连接中不得在此显示断开按钮 */}
          <div className="flex items-center justify-between px-4 py-3 bg-surface border border-surface-hover rounded-xl">
            <div className="flex items-center gap-2">
              {lanStateView.icon}
              <span className={`text-sm ${lanStateView.cls}`}>{lanStateView.text}</span>
            </div>
            {isLanConnected && (
              <button
                onClick={async () => {
                  await stopLocalSession().catch(() => {});
                  setLanHosting(false);
                  localStorage.removeItem('siku.lan.hosting');
                  localStorage.removeItem('siku.lan.paircode');
                  setLanPeers([]);
                }}
                className="flex items-center gap-1.5 px-3 py-1.5 bg-surface border border-surface-hover text-red-400 text-xs rounded-lg hover:bg-red-500/10 transition-colors"
              >
                <X size={12} />
                断开连接
              </button>
            )}
          </div>

          <div className="flex gap-1 p-1 bg-surface border border-surface-hover rounded-lg">
            <button
              className={roleTabBtn(lanRole === 'host')}
              onClick={() => {
                setLanRole('host');
                localStorage.setItem('siku.lan.role', 'host');
              }}
            >
              我是提供方（共享数据）
            </button>
            <button
              className={roleTabBtn(lanRole === 'guest')}
              onClick={() => {
                setLanRole('guest');
                localStorage.setItem('siku.lan.role', 'guest');
              }}
            >
              我是接收方（获取数据）
            </button>
          </div>

          {lanRole === 'host' ? (
            <div className="space-y-3">
              <button
                onClick={async () => {
                  if (lanHosting) {
                    // 停止等待：停掉 beacon 和局域网 host 循环（不影响公网自动同步）
                    await stopLanBeacon().catch(() => {});
                    await stopLocalHost().catch(() => {});
                    setLanHosting(false);
                    localStorage.removeItem('siku.lan.hosting');
                    localStorage.removeItem('siku.lan.paircode');
                    setLanPeers([]);
                    return;
                  }
                  // 生成 6 位配对码并广播（配对码仅本机显示，不随 beacon 广播）
                  const code = String(Math.floor(100000 + Math.random() * 900000));
                  const deviceId = await getDeviceId();
                  setLocalPairCode(code);
                  localStorage.setItem('siku.lan.paircode', code);
                  setLanHosting(true);
                  localStorage.setItem('siku.lan.hosting', '1');
                  await startLocalHost(code);
                  await startLanBeacon({
                    device_id: deviceId,
                    pairing_payload: '',
                  });
                }}
                className="flex items-center gap-2 px-4 py-2 bg-primary text-white text-sm rounded-lg hover:opacity-90 transition-opacity"
              >
                {lanHosting ? <X size={14} /> : <Network size={14} />}
                {lanHosting ? '停止等待' : '开始等待连接'}
              </button>
              {lanHosting && (
                <div className="p-4 bg-surface border border-surface-hover rounded-xl text-center space-y-2">
                  <div className="text-xs text-text-secondary">本机配对码（请在另一台设备上核对一致）</div>
                  <div className="text-3xl font-mono font-semibold text-text-primary tracking-widest">
                    {localPairCode}
                  </div>
                  <div className="text-[11px] text-text-secondary/60">等待对方连接后自动开始同步</div>
                </div>
              )}
            </div>
          ) : (
            <div className="space-y-3">
              <button
                onClick={async () => {
                  if (lanDiscovering) {
                    await stopLanDiscovery().catch(() => {});
                    setLanDiscovering(false);
                    setLanPeers([]);
                    return;
                  }
                  setLanDiscovering(true);
                  try {
                    await startLanDiscovery();
                  } catch (err) {
                    console.error('Failed to start LAN discovery:', err);
                    alert(`启动局域网发现失败: ${err}`);
                    setLanDiscovering(false);
                  }
                }}
                className="flex items-center gap-2 px-4 py-2 bg-surface border border-surface-hover text-text-primary text-sm rounded-lg hover:bg-surface-hover transition-colors"
              >
                {lanDiscovering ? <X size={14} /> : <ScanLine size={14} />}
                {lanDiscovering ? '停止扫描' : '扫描附近设备'}
              </button>

              {lanDiscovering && lanPeers.length === 0 && (
                <div className="flex items-center gap-2 text-xs text-text-secondary">
                  <Wifi size={13} className="animate-pulse" />
                  正在扫描局域网中的设备…
                </div>
              )}

              {lanDiscovering && (
                <LanPeerList
                  peers={lanPeers}
                  onRefresh={async () => {
                    try {
                      const peers = await getLanPeers();
                      setLanPeers(peers);
                    } catch (err) {
                      console.error('Failed to refresh LAN peers:', err);
                    }
                  }}
                  onConnect={handlePeerConnect}
                />
              )}
            </div>
          )}

        </div>
      )}
      {tab === 'offline' && (
        <div className="space-y-4">
          <input
            type="text"
            value={seedPassword}
            onChange={(e) => setSeedPassword(e.target.value)}
            placeholder="设置/输入加密密码"
            className="w-full px-3 py-2 bg-surface border border-surface-hover rounded-lg text-sm text-text-primary outline-none focus:border-primary"
          />
          <div className="flex gap-2">
            <button
              onClick={handleExportSeed}
              disabled={seedAction !== 'idle' || !seedPassword}
              className="flex items-center gap-2 px-4 py-2 bg-surface border border-surface-hover text-text-primary text-sm rounded-lg hover:bg-surface-hover disabled:opacity-50 transition-colors"
            >
              {seedAction === 'exporting' ? <Loader2 size={14} className="animate-spin" /> : <Download size={14} />}
              导出数据（当前设备）
            </button>
            <button
              onClick={handleImportSeed}
              disabled={seedAction !== 'idle' || !seedPassword}
              className="flex items-center gap-2 px-4 py-2 bg-surface border border-surface-hover text-text-primary text-sm rounded-lg hover:bg-surface-hover disabled:opacity-50 transition-colors"
            >
              {seedAction === 'importing' ? <Loader2 size={14} className="animate-spin" /> : <Upload size={14} />}
              导入数据（本设备）
            </button>
          </div>
          {offlineResult && <div className="text-xs text-accent">{offlineResult}</div>}
        </div>
      )}
    </div>
  );
}

interface LanPeerListProps {
  peers: LanPeerInfo[];
  onRefresh: () => Promise<void>;
  onConnect: (peer: LanPeerInfo) => void;
}

function LanPeerList({ peers, onRefresh, onConnect }: LanPeerListProps) {
  useEffect(() => {
    onRefresh();
    const id = window.setInterval(() => {
      onRefresh().catch(() => {});
    }, 1000);
    return () => window.clearInterval(id);
  }, [onRefresh]);

  return (
    <div className="space-y-2">
      {peers.map((peer) => (
        <button
          key={peer.device_id}
          onClick={() => onConnect(peer)}
          className="w-full flex items-center justify-between px-4 py-3 bg-surface border border-surface-hover rounded-lg hover:border-primary transition-colors text-left"
        >
          <div>
            <div className="text-sm text-text-primary font-medium">{peer.device_id.slice(0, 8)}…</div>
            <div className="text-xs text-text-secondary">{peer.addr}</div>
          </div>
          <span className="flex items-center gap-1 text-xs text-primary">
            <Link2 size={12} />
            连接
          </span>
        </button>
      ))}
    </div>
  );
}
