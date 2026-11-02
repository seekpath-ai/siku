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
- **配对绑定**：陌生人首发消息 → 回复"请在思库设置中确认配对" → 设置页确认后入白名单。配对键是 `(channel, chat_id, user_id)`，**按人配对**（已实现）：群内每位成员需单独确认；同群已确认成员共享一个远程会话（session_id 落在该群所有成员行上）。群白名单由此隐含达成：某群没有任何已确认成员时，该群 @ 机器人只会收到配对提示，无法对话；设置页删除全部成员行即等效拉黑该群。

```sql
CREATE TABLE remote_chats (           -- 设备本地表，不进 CRR 同步
  channel TEXT NOT NULL,              -- 'feishu' | 'weixin'
  chat_id TEXT NOT NULL,              -- 外部会话 id
  user_id TEXT NOT NULL,              -- 外部用户 id（配对键的一部分）
  user_name TEXT,
  session_id TEXT,                    -- 群共享：同 chat_id 各行持有同一会话
  approved INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (channel, chat_id, user_id)
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

1. **访问控制收紧**：✅ 已实现——配对键扩为 `(channel, chat_id, user_id)` 群内按人配对（旧库自动迁移重建），群白名单由"无已确认成员即不可对话"隐含达成。
2. **任务触发 + 完成推送**：远程发"帮我综述 xxx"→ 后台跑综述/分析任务 → 完成主动推 IM。需打通任务系统与渠道回发（reply 按 chat_id 主动发，不再依赖当轮上下文）。
3. **远程消息排队**：✅ 已实现——每聊天一把 tokio 异步锁（`CHAT_LOCKS`），等待锁的任务本身就是队列；回合进行中来的消息收到"已排队，完成后自动回复你"提示后挂起，前一条回复完自动接续。配对检查在锁外（陌生人立即收到配对提示不排队）。锁覆盖不到思库 UI 内同会话手动发消息的场景，此时仍回冲突提示。
4. **会话体验打磨**：对话列表加渠道图标、远程会话标识。**会话标题人性化**（两级，均认可）：
   - 第 1 级（建会话时自动解析名称）：✅ 已实现——`ReplySink::resolve_names`（默认空实现），飞书调 `im/v1/chats/{chat_id}` 拿群名、`contact/v3/users/{open_id}` 拿成员名（需 `im:chat:readonly`、`contact:user.base:readonly` 权限并重新发布，缺权限降级为 id 并记 warn 日志）；群标题用群名、私聊用对方姓名；存量裸 id 标题在下次消息时一次性回填。**微信 iLink 协议拿不到昵称**（官方插件也只用 from_user_id），微信标题维持 id。
   - 第 2 级（会话内反查群详情）：**入口仅放对话列表右键菜单**——远程会话右键加「查看群信息」「查看群文件」两项（不做对话页头部图标、不进设置页），点击开居中模态框（复用 TaskCenterDialog 交互范式）：
     - 「查看群信息」小模态框（~400px）：实时调 `/im/v1/chats/{chat_id}` + `/members`，展示群名、群主、成员数、成员列表。仅飞书可做，微信 iLink 无群概念。
     - 「查看群文件」大模态框（~640px）：调 `/im/v1/messages?container_id_type=chat&container_id={chat_id}` 筛 file/image 消息，列表展示文件名/大小/时间（倒序，封顶 200 条），每行操作「导入文献库」（PDF，走现成导入管线）或「另存为」，行内显示进度。下载走 `im/v1/messages/{id}/resources/{file_key}`，与第 5 项媒体消息共用接收管线。群文件标签页的云文档（Drive 体系）不做。**已实现**：右键菜单仅在远程飞书非私聊会话显示（context JSON 新增 `kind` 字段）；命令 `remote_chat_info/files/file_save/file_import`。
5. **媒体消息**：✅ 已实现入站侧——
   - 飞书图片：`resources` 下载 → base64 → 视觉输入（`run_agent_turn` attachments，模型不支持视觉时回明确错误）；图文混排（post）：段落标签解析，text/a 段拼文本、at 段保留"@名字"上下文（机器人自身 mention 剔除）、img 段逐张下载进视觉输入（多图支持）；文件：路由层短路回执，引导"右键会话→查看群文件"导入/另存；其它类型（语音/视频/表情）回不支持提示（仍受群@策略约束）。
   - 微信图片/文件：CDN 下载（`full_url` 优先，否则 `encrypt_query_param` 拼 `novac2c.cdn.weixin.qq.com/c2c/download`）+ AES-128-ECB 解密（密钥两种编码：base64(raw16) / base64(hex32)，图片另有 aeskey hex 优先；RustCrypto `aes`+`ecb` 纯 Rust 实现）；图片进视觉输入，文件存 `app_data/remote_downloads/` 并回执。
   - 未做（暂不评估）：出站媒体（远程工具只读，agent 不产出文件）、飞书卡片远程审批、微信语音 silk 转码。

## 7. 验证方式

- 飞书：测试自建应用拉测试群，覆盖 私聊/@群聊/未配对拒绝/防抖合并/重连
- 微信：ClawBot 私聊全流程 + 重启验证 sync buf 续传不重复消费
- 回归：`cargo check`、`tsc --noEmit`、eslint；远程会话在对话列表可见且流式正常
