import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import QRCode from 'qrcode';
import { Check, HelpCircle, Loader2, Pencil, Plus, Power, Trash2 } from 'lucide-react';
import {
  remoteFeishuBotDelete,
  remoteFeishuBotSave,
  remoteFeishuBots,
  remotePairingDelete,
  remotePairingSet,
  remotePairingSetGrant,
  remotePairings,
  remoteStatus,
  remoteTakeover,
  remoteWeixinConfig,
  remoteWeixinLoginStart,
  remoteWeixinLoginVerify,
  remoteWeixinLogout,
} from '@/lib/tauri';
import type { FeishuBotConfig, RemoteChannelStatus, RemotePairing, WeixinConfigInfo, WeixinQrEvent } from '@/lib/tauri';

const CHANNEL_LABEL: Record<string, string> = { feishu: '飞书', weixin: '微信' };

function channelLabel(channel: string): string {
  return CHANNEL_LABEL[channel.split(':')[0]] ?? channel;
}

/** 配对时间短格式：MM-DD HH:mm；解析失败原样返回。 */
function fmtPairTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** Unified status chip: 未启用 灰 / 待机或异常 琥珀 / 已连接 绿. */
function StatusChip({ st }: { st?: RemoteChannelStatus }) {
  if (!st) return null;
  const cls = st.connected
    ? 'bg-green-500/10 text-green-500'
    : st.enabled || st.role === 'standby'
      ? 'bg-amber-500/10 text-amber-500'
      : 'bg-surface-hover text-text-secondary/60';
  // 未连接时优先显示渠道上报的详情（待机接管、连接失效重试等），
  // 其次按启用状态兜底——待机角色的 enabled=false 不应显示成"未启用"。
  const text = st.connected
    ? st.detail || '已连接'
    : st.detail || (st.enabled ? '未连接' : '未启用');
  return <span className={`text-xs px-2 py-0.5 rounded-full shrink-0 ${cls}`}>{text}</span>;
}

/** Small "?" icon with a hover tooltip panel for channel setup guidance.
 *  The gap between icon and panel is bridged with padding (not margin) so
 *  the mouse can travel into the panel without losing hover — required for
 *  selecting/copying text like event names. */
function HintIcon({ children }: { children: React.ReactNode }) {
  return (
    <span className="relative group inline-flex items-center align-middle ml-1">
      <HelpCircle size={14} className="text-text-secondary/40 group-hover:text-primary cursor-help transition-colors" />
      <span className="hidden group-hover:block absolute left-0 top-full pt-1.5 z-50 w-80 max-w-[80vw] select-text cursor-default">
        <span className="block rounded-lg border border-surface-hover bg-surface shadow-xl px-3 py-2.5 text-xs leading-relaxed text-text-secondary font-normal">
          {children}
        </span>
      </span>
    </span>
  );
}

/** Settings page section: remote IM channels (feishu bots, weixin, pairing). */
export function RemoteSettings() {
  const [loading, setLoading] = useState(true);
  const [bots, setBots] = useState<FeishuBotConfig[]>([]);
  const [status, setStatus] = useState<RemoteChannelStatus[]>([]);
  const [pairings, setPairings] = useState<RemotePairing[]>([]);
  const [weixinCfg, setWeixinCfg] = useState<WeixinConfigInfo | null>(null);
  const [qrDataUrl, setQrDataUrl] = useState<string | null>(null);
  const [qrMessage, setQrMessage] = useState<string | null>(null);
  const [needVerify, setNeedVerify] = useState(false);
  const [verifyCode, setVerifyCode] = useState('');
  const [loginBusy, setLoginBusy] = useState(false);
  const [takingOver, setTakingOver] = useState(false);
  // Bot edit form: 'new' for the add form, an app_id when editing, null closed.
  const [editingId, setEditingId] = useState<string | null>(null);
  const [formName, setFormName] = useState('');
  const [formAppId, setFormAppId] = useState('');
  const [formSecret, setFormSecret] = useState('');
  const [saving, setSaving] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);

  const loadPairings = useCallback(async () => {
    try {
      setPairings(await remotePairings());
    } catch (e) {
      console.error('load remote pairings:', e);
    }
  }, []);

  const loadBots = useCallback(async () => {
    try {
      setBots(await remoteFeishuBots());
    } catch (e) {
      console.error('load feishu bots:', e);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [st, wx] = await Promise.all([remoteStatus(), remoteWeixinConfig()]);
        if (!cancelled) {
          setStatus(st);
          setWeixinCfg(wx);
        }
      } catch (e) {
        console.error('load remote settings:', e);
      }
      if (!cancelled) setLoading(false);
    })();
    loadBots();
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
  }, [loadPairings, loadBots]);

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

  const openNewBotForm = () => {
    setEditingId('new');
    setFormName('');
    setFormAppId('');
    setFormSecret('');
    setFormError(null);
  };

  const openEditBotForm = (bot: FeishuBotConfig) => {
    setEditingId(bot.app_id);
    setFormName(bot.name);
    setFormAppId(bot.app_id);
    setFormSecret('');
    setFormError(null);
  };

  const saveBot = async () => {
    setSaving(true);
    setFormError(null);
    try {
      const existing = bots.find((b) => b.app_id === formAppId.trim());
      await remoteFeishuBotSave(formName, formAppId, formSecret, existing?.enabled ?? true);
      setEditingId(null);
      await loadBots();
    } catch (e) {
      setFormError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const toggleBot = async (bot: FeishuBotConfig) => {
    try {
      // Empty secret = keep the stored one (backend upsert semantics).
      await remoteFeishuBotSave(bot.name, bot.app_id, '', !bot.enabled);
      await loadBots();
    } catch (e) {
      console.error('toggle feishu bot:', e);
    }
  };

  const deleteBot = async (bot: FeishuBotConfig) => {
    if (confirmDeleteId !== bot.app_id) {
      setConfirmDeleteId(bot.app_id);
      window.setTimeout(() => setConfirmDeleteId((cur) => (cur === bot.app_id ? null : cur)), 3000);
      return;
    }
    setConfirmDeleteId(null);
    try {
      await remoteFeishuBotDelete(bot.app_id);
      await loadBots();
    } catch (e) {
      console.error('delete feishu bot:', e);
    }
  };

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
          思库运行期间，通过飞书/微信机器人远程对话：查询笔记、文献，或让智能体处理任务。配置与配对随账号同步漫游，同时只有一台在线设备持有连接，其它设备自动待机。远程会话显示在主对话列表中，能力与本机对话一致，按配对成员授予「全部 / 只读」权限。
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

      {/* Feishu bots */}
      <div className="rounded-lg border border-surface-hover bg-surface px-4 py-3 space-y-3">
        <div className="flex items-center gap-3">
          <div className="flex-1 min-w-0">
            <div className="text-sm font-medium text-text-primary">
              飞书机器人
              <HintIcon>
                <div className="space-y-2">
                  <p>
                    在 <a className="text-primary" href="https://open.feishu.cn/app" target="_blank" rel="noreferrer">飞书开放平台</a> 创建企业自建应用，启用机器人能力；「事件订阅」选择使用长连接接收事件，添加 <code className="px-1 rounded bg-background text-text-primary font-mono text-[11px]">im.message.receive_v1</code>；权限需包含 <code className="px-1 rounded bg-background text-text-primary font-mono text-[11px]">im:message</code> 与 <code className="px-1 rounded bg-background text-text-primary font-mono text-[11px]">im:message:send</code>；另需 <code className="px-1 rounded bg-background text-text-primary font-mono text-[11px]">contact:user.base:readonly</code>（获取用户基本信息，用于成员昵称解析）与 <code className="px-1 rounded bg-background text-text-primary font-mono text-[11px]">im:chat</code>（获取与更新群组信息，用于群名/群信息/群文件）。可添加多个机器人（跨租户或同租户多应用）。
                  </p>
                  <p>卡片按钮（停止 / 重新生成 / 思考过程）需额外订阅「卡片回传交互」回调：</p>
                  <ol className="list-decimal pl-4 space-y-1">
                    <li>「事件与回调 → <b>回调订阅</b>」页签（注意不是「事件订阅」）；</li>
                    <li>订阅方式选「使用长连接接收回调」（与消息事件同通道，无需公网 URL）；</li>
                    <li>添加回调 → 搜索「卡片回传交互」（<code className="px-1 rounded bg-background text-text-primary font-mono text-[11px]">card.action.trigger</code>）；</li>
                    <li>创建版本并发布后生效（开发版可先用「测试企业与人员」验证）。</li>
                  </ol>
                </div>
              </HintIcon>
            </div>
          </div>
          <button
            onClick={openNewBotForm}
            className="h-8 px-3 rounded-lg border border-surface-hover text-sm text-text-secondary hover:text-text-primary flex items-center gap-1.5 shrink-0"
          >
            <Plus size={13} />
            添加机器人
          </button>
        </div>

        {bots.length === 0 && editingId !== 'new' && (
          <div className="text-xs text-text-secondary/50 py-1">尚未配置机器人。</div>
        )}

        {bots.map((bot) => {
          const st = status.find((s) => s.channel === `feishu:${bot.app_id}`);
          const editing = editingId === bot.app_id;
          return (
            <div key={bot.app_id} className="rounded-lg border border-surface-hover/60 bg-background/40 px-3 py-2.5">
              <div className="flex items-center gap-3">
                <div className="flex-1 min-w-0">
                  <div className="text-sm text-text-primary truncate">{bot.name || bot.app_id}</div>
                  {bot.name && <div className="text-[11px] text-text-secondary/50 truncate">{bot.app_id}</div>}
                </div>
                <StatusChip st={st} />
                <button
                  onClick={() => toggleBot(bot)}
                  title={bot.enabled ? '停用' : '启用'}
                  className={`h-7 w-7 rounded-lg border flex items-center justify-center ${
                    bot.enabled
                      ? 'border-primary/40 text-primary'
                      : 'border-surface-hover text-text-secondary hover:text-text-primary'
                  }`}
                >
                  <Power size={13} />
                </button>
                <button
                  onClick={() => openEditBotForm(bot)}
                  title="编辑"
                  className="h-7 w-7 rounded-lg border border-surface-hover text-text-secondary hover:text-text-primary flex items-center justify-center"
                >
                  <Pencil size={13} />
                </button>
                <button
                  onClick={() => deleteBot(bot)}
                  title={confirmDeleteId === bot.app_id ? '再点一次确认删除' : '删除'}
                  className={`h-7 w-7 rounded-lg border flex items-center justify-center ${
                    confirmDeleteId === bot.app_id
                      ? 'border-red-500/50 text-red-400'
                      : 'border-surface-hover text-text-secondary hover:text-red-400'
                  }`}
                >
                  <Trash2 size={13} />
                </button>
              </div>
              {editing && (
                <BotForm
                  name={formName}
                  appId={formAppId}
                  secret={formSecret}
                  appIdLocked
                  error={formError}
                  saving={saving}
                  onName={setFormName}
                  onAppId={setFormAppId}
                  onSecret={setFormSecret}
                  onCancel={() => setEditingId(null)}
                  onSave={saveBot}
                />
              )}
            </div>
          );
        })}

        {editingId === 'new' && (
          <div className="rounded-lg border border-primary/30 bg-background/40 px-3 py-2.5">
            <BotForm
              name={formName}
              appId={formAppId}
              secret={formSecret}
              appIdLocked={false}
              error={formError}
              saving={saving}
              onName={setFormName}
              onAppId={setFormAppId}
              onSecret={setFormSecret}
              onCancel={() => setEditingId(null)}
              onSave={saveBot}
            />
          </div>
        )}
      </div>

      {/* WeChat ClawBot */}
      <div className="rounded-lg border border-surface-hover bg-surface px-4 py-3 space-y-3">
        <div className="flex items-center gap-3">
          <div className="flex-1 min-w-0">
            <div className="text-sm font-medium text-text-primary">
              微信 ClawBot
              <HintIcon>
                <p>
                  扫码将微信绑定到思库账号，账号下各设备共享该绑定，无需重复扫码（重复扫码会替换旧绑定）。之后在微信 ClawBot 里直接发消息对话。需微信 8.0.70+。
                </p>
              </HintIcon>
            </div>
          </div>
          <StatusChip st={weixinStatus} />
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
                <div className="flex-1 min-w-0 flex items-baseline gap-2 whitespace-nowrap overflow-hidden">
                  <span className="shrink-0 text-text-secondary">{channelLabel(p.channel)}</span>
                  <span className="truncate font-mono text-xs text-text-primary" title={p.user_id}>
                    {p.user_name || p.user_id}
                  </span>
                  {p.chat_id !== p.user_id && (
                    <span className="truncate font-mono text-[11px] text-text-secondary/60" title={p.chat_id}>
                      会话 {p.chat_id}
                    </span>
                  )}
                  <span className="shrink-0 text-[11px] text-text-secondary/50" title={`配对请求时间 ${p.created_at}`}>
                    {fmtPairTime(p.created_at)}
                  </span>
                </div>
                <button
                  onClick={() =>
                    remotePairingSetGrant(p.channel, p.chat_id, p.user_id, 'full')
                      .then(() => remotePairingSet(p.channel, p.chat_id, p.user_id, true))
                      .then(loadPairings)
                  }
                  className="h-7 px-2.5 rounded-lg bg-primary text-white text-xs"
                >
                  允许·全部
                </button>
                <button
                  onClick={() =>
                    remotePairingSetGrant(p.channel, p.chat_id, p.user_id, 'readonly')
                      .then(() => remotePairingSet(p.channel, p.chat_id, p.user_id, true))
                      .then(loadPairings)
                  }
                  className="h-7 px-2.5 rounded-lg border border-primary/50 text-primary text-xs hover:bg-primary/10"
                >
                  允许·只读
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
          <div className="mt-2 text-[11px] text-text-secondary/60">
            群聊按成员逐个确认；同群已确认成员共享一个远程会话。「全部」可使用包括命令执行在内的全部工具，「只读」仅能查询文献/笔记/知识库。
          </div>
        </div>
      )}

      {/* Approved chats */}
      {approved.length > 0 && (
        <div className="rounded-lg border border-surface-hover bg-surface px-4 py-3">
          <div className="text-sm font-medium text-text-primary mb-2">已配对成员（{approved.length}）</div>
          <div className="space-y-2">
            {approved.map((p) => (
              <div key={`${p.channel}:${p.chat_id}:${p.user_id}`} className="flex items-center gap-3 text-sm">
                <div className="flex-1 min-w-0 flex items-baseline gap-2 whitespace-nowrap overflow-hidden">
                  <span className="shrink-0 text-text-secondary">{channelLabel(p.channel)}</span>
                  <span className="truncate font-mono text-xs text-text-primary" title={p.user_id}>
                    {p.user_name || p.user_id}
                  </span>
                  {p.chat_id !== p.user_id && (
                    <span className="truncate font-mono text-[11px] text-text-secondary/60" title={p.chat_id}>
                      会话 {p.chat_id}
                    </span>
                  )}
                  <span className="shrink-0 text-[11px] text-text-secondary/50" title={`配对时间 ${p.created_at}`}>
                    配对于 {fmtPairTime(p.created_at)}
                  </span>
                </div>
                <div className="flex rounded-lg border border-surface-hover overflow-hidden text-xs">
                  {(['full', 'readonly'] as const).map((g) => (
                    <button
                      key={g}
                      onClick={() =>
                        remotePairingSetGrant(p.channel, p.chat_id, p.user_id, g).then(loadPairings)
                      }
                      className={`h-7 px-2.5 ${
                        (p.grant_level || 'full') === g
                          ? 'bg-primary text-white'
                          : 'text-text-secondary hover:text-text-primary'
                      }`}
                    >
                      {g === 'full' ? '全部' : '只读'}
                    </button>
                  ))}
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

/** Shared add/edit form for a feishu bot. */
function BotForm({
  name,
  appId,
  secret,
  appIdLocked,
  error,
  saving,
  onName,
  onAppId,
  onSecret,
  onCancel,
  onSave,
}: {
  name: string;
  appId: string;
  secret: string;
  appIdLocked: boolean;
  error: string | null;
  saving: boolean;
  onName: (v: string) => void;
  onAppId: (v: string) => void;
  onSecret: (v: string) => void;
  onCancel: () => void;
  onSave: () => void;
}) {
  return (
    <div className="mt-2.5 pt-2.5 border-t border-surface-hover/60 space-y-2">
      <input
        value={name}
        onChange={(e) => onName(e.target.value)}
        placeholder="名称（可选，便于区分多个机器人）"
        className="w-full h-8 rounded-lg bg-background border border-surface-hover text-sm text-text-primary px-3 focus:outline-none focus:border-primary/50"
      />
      <input
        value={appId}
        onChange={(e) => onAppId(e.target.value)}
        placeholder="App ID（cli_ 开头）"
        disabled={appIdLocked}
        className="w-full h-8 rounded-lg bg-background border border-surface-hover text-sm text-text-primary px-3 focus:outline-none focus:border-primary/50 disabled:opacity-50"
      />
      <input
        type="password"
        value={secret}
        onChange={(e) => onSecret(e.target.value)}
        placeholder={appIdLocked ? 'App Secret（留空保持不变）' : 'App Secret'}
        className="w-full h-8 rounded-lg bg-background border border-surface-hover text-sm text-text-primary px-3 focus:outline-none focus:border-primary/50"
      />
      {error && <div className="text-xs text-red-400">{error}</div>}
      <div className="flex items-center gap-2">
        <button
          onClick={onSave}
          disabled={saving || !appId.trim() || (!appIdLocked && !secret.trim())}
          className="h-8 px-3 rounded-lg bg-primary text-white text-sm disabled:opacity-40 flex items-center gap-1.5"
        >
          {saving ? <Loader2 size={13} className="animate-spin" /> : <Check size={13} />}
          保存
        </button>
        <button
          onClick={onCancel}
          disabled={saving}
          className="h-8 px-3 rounded-lg border border-surface-hover text-sm text-text-secondary hover:text-text-primary disabled:opacity-40"
        >
          取消
        </button>
      </div>
    </div>
  );
}
