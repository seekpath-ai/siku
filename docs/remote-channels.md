# 思库远程接入（飞书 + 微信ClawBot）实施方案

> 状态：阶段一实施中。方案已与用户确认：远程会话出现在主对话列表；群聊默认"仅 @响应 + 白名单"；任务触发暂不做，只做查询类。

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
| `router.rs` | 入站消息 → 会话映射 → 0.6s 防抖合并 → 按聊天串行 → 调 `run_agent_turn` → 取最新 assistant 消息回发 |
| `feishu.rs` | 飞书适配器，社区 crate `feishu-sdk`（WS 长连接、自动重连） |
| `weixin.rs` | iLink 客户端：QR 登录、`getUpdates` 长轮询、`sendMessage`、sync buf 持久化（阶段二） |

回复捕获：`run_agent_turn` 返回后查该会话最新一条 assistant 消息发回渠道，不碰 `agent:event` 流式管线。

## 3. 会话与安全模型

- **会话映射**：每个 (channel, external_chat_id) 惰性创建 `chat_sessions` 行，配置克隆用户默认对话智能体，`domain = 'remote'` 标记，`context` 存渠道信息。`RunningTurnGuard` 保证同一远程聊天串行。
- **工具裁剪**：`ToolRegistry::retain()` 白名单：`note_read`、`paper_search`、`paper_read`、`library_search`、`knowledge`、`memory_recall`、`web_search`、`web_fetch`、`tasks`（只读类）。排除 `bash`、一切写工具。
- **配对绑定**：陌生人首发消息 → 回复"请在思库设置中确认配对" → 设置页确认后入白名单。

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

| 阶段 | 内容 | 工作量 | 风险 |
|---|---|---|---|
| 一 | `remote/` 骨架 + 路由层 + 配对/去重/限流 + 飞书适配器 + 设置页 | 5~6 天 | 低 |
| 二 | 微信 iLink 客户端（登录/轮询/发送）+ 二维码登录 UI | 6~8 天 | 中：协议漂移 |
| 三 | 媒体消息（CDN+AES-128-ECB）、任务触发、飞书卡片远程审批 | 各 2~3 天 | 低-中 |

## 7. 验证方式

- 飞书：测试自建应用拉测试群，覆盖 私聊/@群聊/未配对拒绝/防抖合并/重连
- 微信：ClawBot 私聊全流程 + 重启验证 sync buf 续传不重复消费
- 回归：`cargo check`、`tsc --noEmit`、eslint；远程会话在对话列表可见且流式正常
