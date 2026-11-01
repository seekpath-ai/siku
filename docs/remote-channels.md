# 思库远程接入（飞书 + 微信ClawBot）实施方案

> 状态：阶段一（飞书）、阶段二（微信ClawBot）已实现并真机实测通过（d35dd95：rustls CryptoProvider、飞书事件字段全拼 `message_type`/`message_id`、回复抢跑、微信回发容错）。方案已与用户确认：远程会话出现在主对话列表；群聊默认"仅 @响应 + 白名单"；任务触发暂不做，只做查询类。

## 1. 目标与边界

思库运行期间，用户通过微信ClawBot私聊或飞书机器人私聊/群聊：

- 查询笔记、文献（"我那篇 transformer 的笔记怎么说"）
- 发起任务处理（阶段三，暂缓）
- 远程会话在思库对话列表中可见可回溯（复用 `chat_sessions`/`chat_messages`）

远程会话复用现有"对话"智能体，但工具集裁剪为只读——远程没有审批卡 UI，安全模型前置。

## 2. 总体架构

```
微信ClawBot ─ iLink长轮询(出站) ─┐
                                 ├─ ChannelManager ─ 路由层 ─→ run_agent_turn ─→ 回流回复
飞书机器人 ─ WS长连接(出站) ────┘   (去重/防抖/配对/限流)      (复用现有对话智能体)
```

全出站连接，不依赖 relay、不需要公网 IP。新增模块 `src-tauri/src/remote/`：

| 文件 | 职责 |
|---|---|
| `mod.rs` | `RemoteChannelManager`：渠道启停、配置热加载、状态事件 `remote:status` |
| `router.rs` | 入站消息 → 会话映射 → 0.6s 防抖合并 → 按聊天串行 → 调 `run_agent_turn` → 基线轮询等本轮回复落库后回发 |
| `feishu.rs` | 飞书适配器，社区 crate `feishu-sdk`（WS 长连接、自动重连） |
| `weixin.rs` | iLink 客户端：QR 登录、`getUpdates` 长轮询、`sendMessage`、sync buf 持久化（阶段二） |

回复捕获：`run_agent_turn` spawn 后立即返回，不能回头发最新 assistant 消息（会拿到上一轮的回复——实测踩过）。改为回合前记录 assistant 行 max(rowid) 基线，轮询等待本轮新行落库（助手消息只在回合结束持久化，含失败 ❌ 行），超时 10 分钟。不碰 `agent:event` 流式管线。

## 3. 会话与安全模型

- **会话映射**：每个 (channel, external_chat_id) 惰性创建 `chat_sessions` 行，配置克隆用户默认对话智能体，`domain = 'remote'` 标记，`context` 存渠道信息。`RunningTurnGuard` 保证同一远程聊天串行。
- **工具裁剪**：`ToolRegistry::retain()` 白名单：`note_read`、`paper_search`、`paper_read`、`library_search`、`knowledge`、`memory_recall`、`web_search`、`web_fetch`、`tasks`（只读类）。排除 `bash`、一切写工具。
- **配对绑定**：陌生人首发消息 → 回复"请在思库设置中确认配对" → 设置页确认后入白名单。配对键是 `(channel, chat_id)`，**按会话不按人**：群聊配对一次全群生效，全群共享一个远程会话（与 OpenClaw 默认一致），限流按发送者 open_id。

**访问控制收紧（阶段三候选，两方向均已认可）**：

1. **群白名单**：设置页维护"允许响应的群"列表，不在列表的群 @ 了也不响应（现配对 UI 即雏形，但批准一次全群生效，需显式化）。
2. **群内按人配对**：配对键改为 `(channel, chat_id, user_id)`，每个成员单独确认；会话仍按群共享，只挡未授权发言者。防陌生大群任意成员调用文献库工具。

```sql
CREATE TABLE remote_chats (           -- 设备本地表，不进 CRR 同步
  channel TEXT NOT NULL,              -- 'feishu' | 'weixin'
  chat_id TEXT NOT NULL,              -- 外部会话 id
  user_id TEXT NOT NULL,              -- 外部用户 id（白名单键）
  user_name TEXT,
  session_id TEXT,                    -- 惰性创建的 chat_sessions.id
  approved INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (channel, chat_id)
);
```

- **渠道配置**（AppID/Secret、iLink token、开关）存 `DeviceAppSettings`（设备本地、不同步）。
- **限流**：每用户 10 条/分钟滑动窗口；消息 ID 24h 去重（内存 LRU + app_data json 持久化）。

## 4. 渠道协议要点

**飞书**（官方稳定 API）

- 用户在 open.feishu.cn 建自建应用拿 AppID/Secret；订阅 `im.message.receive_v1`，权限 `im:message`、`im:message:send`。
- WS 长连接由 SDK 维护，断线自动重连；群聊默认仅响应 @；Markdown 回复自动转 post 富文本。

**微信 iLink**（官方通道，已完整放量；阶段二）

- 登录：请求 QR → 设置页显示二维码 → 微信扫码确认 → 轮询登录态拿 token/contextToken → 持久化。
- 收消息：`getUpdates` 长轮询 + sync buf 持久化断点续传；发消息：`sendMessage` REST。
- 参考：腾讯官方 `@tencent-weixin/openclaw-weixin`、第三方复刻 weixin-ClawBot-API 的协议文档。
- 协议非正式规范，版本常量集中一处，登录失败/协议变更时降级提示"重新扫码"。

## 5. 前端

`SettingsLayout` 新增"远程接入"页（`RemoteSettings.tsx`）：渠道卡片（开关、状态、凭证、二维码登录）、待配对列表、白名单管理。远程活跃状态接入 `pet_report`。

## 6. 阶段拆分

| 阶段 | 内容 | 状态 | 风险 |
|---|---|---|---|
| 一 | `remote/` 骨架 + 路由层 + 配对/去重/限流 + 飞书适配器 + 设置页 | ✅ ec6be33 | 低 |
| 二 | 微信 iLink 客户端（登录/轮询/发送）+ 二维码登录 UI | ✅ 445f2ab | 中：协议漂移 |
| 实测修复 | rustls CryptoProvider、飞书字段全拼、回复抢跑、微信回发容错 | ✅ d35dd95 | — |

**阶段三 backlog（优先级从高到低）**：

1. **访问控制收紧**：群白名单 + 群内按人配对（见 §3）。先做，挡住陌生群风险。
2. **任务触发 + 完成推送**：远程发"帮我综述 xxx"→ 后台跑综述/分析任务 → 完成主动推 IM。需打通任务系统与渠道回发（reply 按 chat_id 主动发，不再依赖当轮上下文）。
3. **远程消息排队**：生成中再发的消息目前吃"稍后再发"；改为每聊天一个队列，回合结束自动接续下一条。
4. **会话体验打磨**：对话列表加渠道图标、远程会话标识。**会话标题人性化**（两级，均认可）：
   - 第 1 级（建会话时自动解析名称）：飞书调 `GET /open-apis/im/v1/chats/{chat_id}`（需 `im:chat` 权限）拿群名做标题（"飞书 · 课题三组"），私聊调用户信息接口拿姓名；微信用 getupdates 里的发送者昵称。解决现状标题是 chat_id 裸串的问题。
   - 第 2 级（会话内反查群详情）：对话页头部/右键加"查看群信息"，实时调 `/im/v1/chats/{chat_id}` + `/members` 展示群名、群主、成员列表。仅飞书可做，微信 iLink 无群概念、无等价接口。
5. **媒体消息**：微信图片/文件收（CDN 下载 + AES-128-ECB 解密）与发（上传）；飞书图片/文件收发；飞书卡片远程审批。微信侧最绕，放最后。

## 7. 验证方式

- 飞书：测试自建应用拉测试群，覆盖 私聊/@群聊/未配对拒绝/防抖合并/重连
- 微信：ClawBot 私聊全流程 + 重启验证 sync buf 续传不重复消费
- 回归：`cargo check`、`tsc --noEmit`、eslint；远程会话在对话列表可见且流式正常
