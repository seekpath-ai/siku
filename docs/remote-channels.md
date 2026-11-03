# 思库远程接入（飞书 + 微信ClawBot）实施方案

> 状态：阶段一（飞书）、阶段二（微信ClawBot）已实现并真机实测通过（d35dd95：rustls CryptoProvider、飞书事件字段全拼 `message_type`/`message_id`、回复抢跑、微信回发容错）。方案已与用户确认：远程会话出现在主对话列表；群聊默认"仅 @响应 + 白名单"；任务触发暂不做，只做查询类。

## 1. 目标与边界

思库运行期间，用户通过微信ClawBot私聊或飞书机器人私聊/群聊：

- 查询笔记、文献（"我那篇 transformer 的笔记怎么说"）
- 发起任务处理（阶段三，暂缓）
- 远程会话在思库对话列表中可见可回溯（复用 `chat_sessions`/`chat_messages`）

远程会话复用现有"对话"智能体，**工具集与本地对话一致（全量内置工具）**——远程是用户自己驱动自己的电脑；审批强制 Auto（远程没有审批卡 UI）。安全边界只保留在配对层：群内每位成员需单独确认。

## 2. 总体架构

```
微信ClawBot ─ iLink长轮询(出站) ─┐
                                 ├─ ChannelManager ─ 路由层 ─→ run_agent_turn ─→ 回流回复
飞书机器人 ─ WS长连接(出站) ────┘   (去重/防抖/配对/限流)      (复用现有对话智能体)
```

全出站连接，不依赖 relay、不需要公网 IP。新增模块 `src-tauri/src/remote/`：

| 文件 | 职责 |
|---|---|
| `mod.rs` | `RemoteChannelManager`：渠道启停（飞书按机器人实例多槽位、微信单槽位）、单活仲裁、配置热加载、状态事件 `remote:status` |
| `router.rs` | 入站消息 → 会话映射 → 0.6s 防抖合并 → 按聊天串行 → 调 `run_agent_turn` → 基线轮询等本轮回复落库后回发 |
| `feishu.rs` | 飞书适配器，社区 crate `feishu-sdk`（WS 长连接、自动重连） |
| `weixin.rs` | iLink 客户端：QR 登录、`getUpdates` 长轮询、`sendMessage`、sync buf 持久化（阶段二） |

回复捕获：`run_agent_turn` spawn 后立即返回，不能回头发最新 assistant 消息（会拿到上一轮的回复——实测踩过）。改为回合前记录 assistant 行 max(rowid) 基线，轮询等待本轮新行落库（助手消息只在回合结束持久化，含失败 ❌ 行），超时 10 分钟。不碰 `agent:event` 流式管线。

## 3. 会话与安全模型

- **会话映射**：每个 (channel, external_chat_id) 惰性创建 `chat_sessions` 行，配置克隆用户默认对话智能体，`domain = 'remote'` 标记，`context` 存渠道信息。`RunningTurnGuard` 保证同一远程聊天串行。
- **工具与审批**：会话 `tools_enabled = NULL`（全量内置工具，与本地对话一致，含 bash/写工具）；`build_agent_config` 识别 `context.remote` 后强制 `ApprovalMode::Auto`（远程无审批卡 UI，避免回合卡审批超时），同时无视历史会话上遗留的只读 tools 白名单。**按成员授权收窄**：`remote_chats.grant_level`（`'full' | 'readonly'`），readonly 成员的回合由路由层向 `run_agent_turn` 传 `tools_override`（只读工具集）——群会话多名成员共享一行会话，限制必须按回合而非按会话。新配对默认值：群成员 readonly、私聊 full；存量行迁移默认 full。设置页配对确认时可二选一（允许·全部/允许·只读），已配对成员随时切换。
- **配对绑定**：陌生人首发消息 → 回复"请在思库设置中确认配对" → 设置页确认后入白名单。配对键是 `(channel, chat_id, user_id)`，**按人配对**（已实现）：群内每位成员需单独确认；同群已确认成员共享一个远程会话（session_id 落在该群所有成员行上）。群白名单由此隐含达成：某群没有任何已确认成员时，该群 @ 机器人只会收到配对提示，无法对话；设置页删除全部成员行即等效拉黑该群。

```sql
CREATE TABLE remote_chats (           -- 同步表（OPTIONAL_SYNC_TABLES）：配对状态随网关漫游
  channel TEXT NOT NULL DEFAULT '',   -- 'weixin' | 'feishu:<app_id>'（实例编码见下）
  chat_id TEXT NOT NULL DEFAULT '',   -- 外部会话 id
  user_id TEXT NOT NULL DEFAULT '',   -- 外部用户 id（配对键的一部分）
  user_name TEXT NOT NULL DEFAULT '',
  session_id TEXT,                    -- 群共享：同 chat_id 各行持有同一会话
  approved INTEGER NOT NULL DEFAULT 0,
  grant_level TEXT NOT NULL DEFAULT 'full',  -- 'full' 全量工具 | 'readonly' 只读工具集（按回合生效）
  created_at TEXT NOT NULL DEFAULT '',
  updated_at TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (channel, chat_id, user_id)
);
```

- **渠道配置**（飞书 bots AppID/Secret、iLink token、开关）存**独立同步行 `remote_config`**：账号下设备漫游，微信扫码一次全账号共享（重扫会替换旧绑定，漫游是硬需求）。注意不能放 `app_settings.remote`——`app_settings` 行因含 LLM API key 被同步层（changeset/快照/statement 三处）永久排除，漫游会静默失效（2026-11 踩过：配对表和微信游标漫游正常，唯独配置不到达对端）。旧位置（`app_settings.remote`、设备本地 `device_settings.remote`）由 `migrate_remote_settings` 按"填隙合并"（飞书按 app_id 并集、微信取非空）一次性迁入并清空。安全性依据：同步通道本身 E2E 加密（relay 只存密文），bot token 与对话内容同级保护；与"账号凭证绝不进同步表"不冲突——账号 token/同步密钥是信任根，bot token 是被保护的内容。
- **飞书多机器人**（多租户/多应用同支持）：`feishu_bots: Vec<FeishuBotConfig>`，每个实例 = 一组 app_id/app_secret，app_id 即实例 id。实例维度编码进 channel 字符串（`feishu:<app_id>`），配对主键、会话 context、限流/串行锁键、状态槽位全部按实例隔离——跨租户 chat_id 撞车不会串；同租户两机器人同群时飞书 message_id 全局唯一，入站去重天然丢弃第二条事件。旧单配置 `feishu` 字段在 remote_config 读写与迁移时归一化进列表；旧配对行由 `migrate_feishu_instance_channels` 在恰好一个机器人时改写为实例 channel。微信维持单实例（漫游单活模型一对一）。
- **微信运行时态**（长轮询游标、context_tokens）单独存同步行 `remote_weixin_state`：游标每个轮询周期都可能前进，若埋在 app_settings 整 blob 里，LWW 会冲掉其它设备对无关设置的编辑。checkpoint 15s 节流 + 退出时终写。
- **单活仲裁**（`remote/mod.rs`）：连接同一时刻只允许一台设备持有。holder 确定性选举——relay 房间 presence（`api/devices` 返回 online 标记）中 device_id 最小者，所有设备独立计算同一结果，无锁无心跳；同步行 `remote_holder` 提供手动接管覆盖（设置页「在本机接管」），override 设备在线时优先。仲裁器每 45s 重估，presence 查询失败保持原角色（宁停勿双）；未登录同步账号时按单设备旧行为直连。渠道任务按 (want, config指纹) 跳过不变重启，健康连接不被 45s 周期打断。
- **限流**：每用户 10 条/分钟滑动窗口；消息 ID 24h 去重（内存 LRU + app_data json 持久化）。
- **接管重投加固**（2026-11-03，修复"备用机接管后服务器重投积压消息被当新消息批量处理"）：三层纵深——① 跨设备去重表 `remote_msg_ids`（同步行，id=`channel:msg_id`，INSERT OR IGNORE 原子占坑，>5000 行才修剪）替代纯本地 seen 文件；② 微信游标采纳：轮询循环启动时给同步 10s 宽限窗口、之后每轮 poll 前重读同步行，凡与本机内存值及本机最后持久化值都不同的游标判定为前持有者写入并采纳；③ 消息年龄护栏：超过 2 小时的入站消息（飞书 `create_time`、微信 `create_time_ms/create_time` 存在时）直接丢弃不触发回合。

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
   - 飞书图片：`resources` 下载 → base64 → 视觉输入（`run_agent_turn` attachments，模型不支持视觉时回明确错误）；图文混排（post）：段落标签解析，text/a 段拼文本、at 段保留"@名字"上下文（机器人自身 mention 剔除）、img 段逐张下载进视觉输入（多图支持）；**文件**：`resolve_file_media` 落盘并统一归置 `remote_downloads/` → 走正常 agent 回合，回合文本携带保存路径 + `read_document_text` 提取的内容（与本地对话附件同一管线，超限自动缓存并分页）；**不自动导入**——用户要求入库时 agent 自行用 `paper_import`（仅文献 PDF）；落盘失败回执引导手动处理；其它类型（语音/视频/表情）回不支持提示（仍受群@策略约束）。
   - 微信图片/文件：CDN 下载（`full_url` 优先，否则 `encrypt_query_param` 拼 `novac2c.cdn.weixin.qq.com/c2c/download`）+ AES-128-ECB 解密（密钥两种编码：base64(raw16) / base64(hex32)，图片另有 aeskey hex 优先；RustCrypto `aes`+`ecb` 纯 Rust 实现）；图片进视觉输入，文件落盘 `remote_downloads/`（路径存 MediaRef.file_key）后**PDF 自动导入文献库**（同飞书管线），非 PDF 保留手动处理回执。**语音**：直接读 `voice_item.text`（微信服务端自动转写，官方插件同路径），无需 silk 解码；无转写结果时回执引导发文字。
   - 未做（暂不评估）：出站媒体（远程工具只读，agent 不产出文件）、飞书卡片远程审批。
6. **工作状态展示**：✅ 已实现——
   - **共用进度管线**（`router.rs`）：回合开始挂 `app.listen("agent:event")` 按 session_id 过滤，聚合 `RemoteProgress`（工具行动态含耗时、流式正文/推理摘要、计时），2s 节流推 `ReplySink::progress_update`；`progress_start`/`progress_finish` 负责建/收，finish 返回 true 时跳过纯文本补发。
   - **飞书活卡片**：interactive 卡片 + `PATCH /im/v1/messages/{id}` 原地更新。回合开始发卡（问题摘要+计时）→ 工具行实时追加（中文化标签：阅读文献/检索文献库/联网搜索…，完成打勾+耗时）→ 流式正文预览 → 完成 PATCH 为 ✅ 绿卡（正文+用时+工具数）/失败红卡；>2500 字降级"分条发送"。卡片创建失败自动回退纯文本。
   - **微信延迟回执**：协议无消息编辑/typing 指示，回执只能是普通消息——4s 未回发"收到，正在思考…"、15s 未回发"还在处理中…"，回合结束取消定时器；快速回合零打扰。
   - 二期候选：思考过程折叠展示。✅ 已实现卡片按钮回调（`card.action.trigger` 回调，需在控制台「回调订阅」添加并长连接接收）：运行中卡片带「停止生成」（取消 agent 回合）、完成/失败卡片带「重新生成/重试」（原提示词经 enqueue 重跑一轮，旧卡标记"已重试 ↻"，`retried` 标志防连点/重投重复开回合）、有推理内容时带「思考过程」展开/收起（按钮 value 带目标态幂等设置，防 at-least-once 重投双翻转）；**三个按钮全部走"应答内返回卡片"**（`EventResp` body 携带 `{"card":{"type":"raw","data":...}}`，平台收到立即渲染，零额外 PATCH、无应答超时重发窗口、状态与界面不脱节）；旧卡点击按 message_id 防串。

## 7. 验证方式

- 飞书：测试自建应用拉测试群，覆盖 私聊/@群聊/未配对拒绝/防抖合并/重连
- 微信：ClawBot 私聊全流程 + 重启验证 sync buf 续传不重复消费
- 回归：`cargo check`、`tsc --noEmit`、eslint；远程会话在对话列表可见且流式正常
