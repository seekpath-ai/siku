import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import QRCode from 'qrcode';
import { Check, Loader2, Trash2 } from 'lucide-react';
import {
  remoteFeishuConfig,
  remoteFeishuSave,
  remotePairingDelete,
  remotePairingSet,
  remotePairings,
  remoteStatus,
  remoteTakeover,
  remoteWeixinConfig,
  remoteWeixinLoginStart,
  remoteWeixinLoginVerify,
  remoteWeixinLogout,
} from '@/lib/tauri';
import type { RemoteChannelStatus, RemotePairing, WeixinConfigInfo, WeixinQrEvent } from '@/lib/tauri';

const CHANNEL_LABEL: Record<string, string> = { feishu: '飞书', weixin: '微信' };

/** Settings page section: remote IM channels (feishu bot, pairing). */
export function RemoteSettings() {
  const [loading, setLoading] = useState(true);
  const [enabled, setEnabled] = useState(false);
  const [appId, setAppId] = useState('');
  const [appSecret, setAppSecret] = useState('');
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [status, setStatus] = useState<RemoteChannelStatus[]>([]);
  const [pairings, setPairings] = useState<RemotePairing[]>([]);
  const [weixinCfg, setWeixinCfg] = useState<WeixinConfigInfo | null>(null);
  const [qrDataUrl, setQrDataUrl] = useState<string | null>(null);
  const [qrMessage, setQrMessage] = useState<string | null>(null);
  const [needVerify, setNeedVerify] = useState(false);
  const [verifyCode, setVerifyCode] = useState('');
  const [loginBusy, setLoginBusy] = useState(false);
  const [takingOver, setTakingOver] = useState(false);

  const loadPairings = useCallback(async () => {
    try {
      setPairings(await remotePairings());
    } catch (e) {
      console.error('load remote pairings:', e);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [cfg, st, wx] = await Promise.all([remoteFeishuConfig(), remoteStatus(), remoteWeixinConfig()]);
        if (!cancelled) {
          setEnabled(cfg.enabled);
          setAppId(cfg.app_id);
          setAppSecret(cfg.app_secret);
          setStatus(st);
          setWeixinCfg(wx);
        }
      } catch (e) {
        console.error('load remote settings:', e);
      }
      if (!cancelled) setLoading(false);
    })();
    loadPairings();

    const un1 = listen<RemoteChannelStatus[]>('remote:status', (e) => setStatus(e.payload));
    const un2 = listen('remote:pairing', () => loadPairings());
    const un3 = listen<WeixinQrEvent>('remote:weixin-qr', async (e) => {
      const { url, message, need_verify } = e.payload;
      setNeedVerify(need_verify);
      setQrMessage(message);
      if (url) {
        try {
          setQrDataUrl(await QRCode.toDataURL(url, { width: 220, margin: 1 }));
        } catch (err) {
          console.error('render qr:', err);
        }
      } else {
        setQrDataUrl(null);
      }
      if (message === '已连接微信') {
        setLoginBusy(false);
        remoteWeixinConfig().then(setWeixinCfg).catch(() => {});
      }
    });
    return () => {
      cancelled = true;
      un1.then((u) => u());
      un2.then((u) => u());
      un3.then((u) => u());
    };
  }, [loadPairings]);

  const startWeixinLogin = async () => {
    setLoginBusy(true);
    setQrMessage(null);
    setQrDataUrl(null);
    setNeedVerify(false);
    try {
      await remoteWeixinLoginStart();
    } catch (e) {
      setQrMessage(String(e));
      setLoginBusy(false);
    }
  };

  const save = async (nextEnabled: boolean) => {
    setSaving(true);
    try {
      await remoteFeishuSave(appId, appSecret, nextEnabled);
      setEnabled(nextEnabled);
      setSaved(true);
      window.setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      console.error('save feishu config:', e);
    } finally {
      setSaving(false);
    }
  };

  const feishuStatus = status.find((s) => s.channel === 'feishu');
  const weixinStatus = status.find((s) => s.channel === 'weixin');
  const standbyHolder = status.find((s) => s.role === 'standby')?.holder;
  const pending = pairings.filter((p) => !p.approved);
  const approved = pairings.filter((p) => p.approved);

  const takeover = async () => {
    setTakingOver(true);
    try {
      await remoteTakeover();
    } catch (e) {
      console.error('remote takeover:', e);
    } finally {
      setTakingOver(false);
    }
  };

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
        <h2 className="text-base font-semibold text-text-primary">远程接入</h2>
        <p className="text-xs text-text-secondary/60 mt-1">
          思库运行期间，通过飞书/微信机器人远程对话：查询笔记、文献，或让智能体处理任务。配置与配对随账号同步漫游，同时只有一台在线设备持有连接，其它设备自动待机。远程会话使用只读工具集，并显示在主对话列表中。
        </p>
      </div>

      {/* Standby banner: another device holds the connection */}
      {standbyHolder && (
        <div className="rounded-lg border border-amber-500/40 bg-amber-500/5 px-4 py-3 flex items-center gap-3">
          <div className="flex-1 min-w-0 text-sm text-text-secondary">
            远程接入当前由 <span className="text-text-primary font-medium">{standbyHolder}</span> 接管，本机处于待机。
          </div>
          <button
            onClick={takeover}
            disabled={takingOver}
            className="h-8 px-3 rounded-lg bg-primary text-white text-sm disabled:opacity-40 flex items-center gap-1.5 shrink-0"
          >
            {takingOver && <Loader2 size={13} className="animate-spin" />}
            在本机接管
          </button>
        </div>
      )}

      {/* Feishu channel */}
      <div className="rounded-lg border border-surface-hover bg-surface px-4 py-3 space-y-3">
        <div className="flex items-center gap-3">
          <div className="flex-1 min-w-0">
            <div className="text-sm font-medium text-text-primary">飞书机器人</div>
            <div className="text-xs text-text-secondary/60 mt-0.5">
              在 <a className="text-primary" href="https://open.feishu.cn/app" target="_blank" rel="noreferrer">飞书开放平台</a> 创建企业自建应用，启用机器人能力并发布版本；
              事件订阅选择「使用长连接接收事件」，添加 im.message.receive_v1；权限需包含 im:message 与 im:message:send。
            </div>
          </div>
          {feishuStatus && (
            <span
              className={`text-xs px-2 py-0.5 rounded-full shrink-0 ${
                feishuStatus.connected
                  ? 'bg-green-500/10 text-green-500'
                  : feishuStatus.enabled
                    ? 'bg-amber-500/10 text-amber-500'
                    : 'bg-surface-hover text-text-secondary/60'
              }`}
            >
              {feishuStatus.connected ? feishuStatus.detail || '已连接' : feishuStatus.enabled ? feishuStatus.detail || '未连接' : '未启用'}
            </span>
          )}
        </div>
        <div className="space-y-2">
          <input
            value={appId}
            onChange={(e) => setAppId(e.target.value)}
            placeholder="App ID（cli_ 开头）"
            className="w-full h-8 rounded-lg bg-background border border-surface-hover text-sm text-text-primary px-3 focus:outline-none focus:border-primary/50"
          />
          <input
            type="password"
            value={appSecret}
            onChange={(e) => setAppSecret(e.target.value)}
            placeholder="App Secret"
            className="w-full h-8 rounded-lg bg-background border border-surface-hover text-sm text-text-primary px-3 focus:outline-none focus:border-primary/50"
          />
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => save(true)}
            disabled={saving || !appId.trim() || !appSecret.trim()}
            className="h-8 px-3 rounded-lg bg-primary text-white text-sm disabled:opacity-40 flex items-center gap-1.5"
          >
            {saving ? <Loader2 size={13} className="animate-spin" /> : saved ? <Check size={13} /> : null}
            保存并启用
          </button>
          {enabled && (
            <button
              onClick={() => save(false)}
              disabled={saving}
              className="h-8 px-3 rounded-lg border border-surface-hover text-sm text-text-secondary hover:text-text-primary disabled:opacity-40"
            >
              停用
            </button>
          )}
        </div>
      </div>

      {/* WeChat ClawBot */}
      <div className="rounded-lg border border-surface-hover bg-surface px-4 py-3 space-y-3">
        <div className="flex items-center gap-3">
          <div className="flex-1 min-w-0">
            <div className="text-sm font-medium text-text-primary">微信 ClawBot</div>
            <div className="text-xs text-text-secondary/60 mt-0.5">
              扫码将微信绑定到思库账号，账号下各设备共享该绑定，无需重复扫码。之后在微信 ClawBot 里直接发消息对话。需微信 8.0.70+。
            </div>
          </div>
          {weixinStatus && (
            <span
              className={`text-xs px-2 py-0.5 rounded-full shrink-0 ${
                weixinStatus.connected
                  ? 'bg-green-500/10 text-green-500'
                  : weixinStatus.enabled
                    ? 'bg-amber-500/10 text-amber-500'
                    : 'bg-surface-hover text-text-secondary/60'
              }`}
            >
              {weixinStatus.connected ? weixinStatus.detail || '已连接' : weixinStatus.enabled ? weixinStatus.detail || '未连接' : '未连接'}
            </span>
          )}
        </div>

        {weixinCfg?.enabled ? (
          <div className="flex items-center gap-3">
            <div className="flex-1 min-w-0 text-xs text-text-secondary/60">
              已连接微信账号 {weixinCfg.ilink_user_id || '—'}
            </div>
            <button
              onClick={() =>
                remoteWeixinLogout()
                  .then(() => remoteWeixinConfig())
                  .then(setWeixinCfg)
                  .catch((e) => console.error('weixin logout:', e))
              }
              className="h-8 px-3 rounded-lg border border-surface-hover text-sm text-text-secondary hover:text-text-primary"
            >
              断开连接
            </button>
          </div>
        ) : (
          <div className="space-y-3">
            {!loginBusy && (
              <button
                onClick={startWeixinLogin}
                className="h-8 px-3 rounded-lg bg-primary text-white text-sm"
              >
                扫码连接微信
              </button>
            )}
            {qrDataUrl && (
              <div className="flex flex-col items-start gap-2">
                <img src={qrDataUrl} alt="微信登录二维码" className="rounded-lg border border-surface-hover" />
              </div>
            )}
            {needVerify && (
              <div className="flex items-center gap-2">
                <input
                  value={verifyCode}
                  onChange={(e) => setVerifyCode(e.target.value)}
                  placeholder="微信中显示的数字"
                  className="w-40 h-8 rounded-lg bg-background border border-surface-hover text-sm text-text-primary px-3 focus:outline-none focus:border-primary/50"
                />
                <button
                  onClick={() => {
                    remoteWeixinLoginVerify(verifyCode.trim()).catch((e) => setQrMessage(String(e)));
                    setVerifyCode('');
                  }}
                  disabled={!verifyCode.trim()}
                  className="h-8 px-3 rounded-lg bg-primary text-white text-sm disabled:opacity-40"
                >
                  提交
                </button>
              </div>
            )}
            {qrMessage && <div className="text-xs text-text-secondary/70">{qrMessage}</div>}
          </div>
        )}
      </div>

      {/* Pairing requests */}
      {pending.length > 0 && (
        <div className="rounded-lg border border-amber-500/40 bg-surface px-4 py-3">
          <div className="text-sm font-medium text-text-primary mb-2">待确认配对（{pending.length}）</div>
          <div className="space-y-2">
            {pending.map((p) => (
              <div key={`${p.channel}:${p.chat_id}:${p.user_id}`} className="flex items-center gap-3 text-sm">
                <div className="flex-1 min-w-0 text-text-secondary">
                  {CHANNEL_LABEL[p.channel] ?? p.channel}{' '}
                  <span className="text-text-primary">{p.user_name || p.user_id}</span>
                  <span className="text-text-secondary/60">（会话 {p.chat_id}）</span>
                </div>
                <button
                  onClick={() => remotePairingSet(p.channel, p.chat_id, p.user_id, true).then(loadPairings)}
                  className="h-7 px-2.5 rounded-lg bg-primary text-white text-xs"
                >
                  允许
                </button>
                <button
                  onClick={() => remotePairingDelete(p.channel, p.chat_id, p.user_id).then(loadPairings)}
                  className="h-7 px-2.5 rounded-lg border border-surface-hover text-xs text-text-secondary hover:text-text-primary"
                >
                  忽略
                </button>
              </div>
            ))}
          </div>
          <div className="mt-2 text-[11px] text-text-secondary/60">群聊按成员逐个确认；同群已确认成员共享一个远程会话。</div>
        </div>
      )}

      {/* Approved chats */}
      {approved.length > 0 && (
        <div className="rounded-lg border border-surface-hover bg-surface px-4 py-3">
          <div className="text-sm font-medium text-text-primary mb-2">已配对成员（{approved.length}）</div>
          <div className="space-y-2">
            {approved.map((p) => (
              <div key={`${p.channel}:${p.chat_id}:${p.user_id}`} className="flex items-center gap-3 text-sm">
                <div className="flex-1 min-w-0 text-text-secondary">
                  {CHANNEL_LABEL[p.channel] ?? p.channel}{' '}
                  <span className="text-text-primary">{p.user_name || p.user_id}</span>
                  <span className="text-text-secondary/60">（会话 {p.chat_id}）</span>
                </div>
                <button
                  onClick={() => remotePairingDelete(p.channel, p.chat_id, p.user_id).then(loadPairings)}
                  className="h-7 w-7 rounded-lg border border-surface-hover text-text-secondary hover:text-red-400 flex items-center justify-center"
                  title="解除配对"
                >
                  <Trash2 size={13} />
                </button>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
