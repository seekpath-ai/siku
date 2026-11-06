# 思库远程接入（飞书 + 微信ClawBot）实施方案

> 状态：阶段一（飞书）、阶段二（微信ClawBot）已实现并真机实测通过（d35dd95：rustls CryptoProvider、飞书事件字段全拼 `message_type`/`message_id`、回复抢跑、微信回发容错）。方案已与用户确认：远程会话出现在主对话列表；群聊默认"仅 @响应 + 白名单"；任务触发暂不做，只做查询类。**阶段四（出站文件回传）P0/P1 已真机实测通过（微信私聊）；P2（飞书上传）/P3（边界场景）已实现、待真机，见 §7。微信工作状态改用原生 typing（已真机验证，见 §9）；微信工具进度行明确不做。**

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

- 用户在 open.feishu.cn 建自建应用拿 AppID/Secret；订阅 `im.message.receive_v1`，权限 `im:message`、`im:message:send`、`contact:user.base:readonly`（成员昵称解析）、`im:chat`（群名/群信息/群文件）。
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
| 三 | 按人配对、消息排队、入站媒体、群信息/群文件、进度面（详见下方 backlog） | ✅ 见 backlog | — |
| 四 | 出站文件回传（微信 CDN + 飞书上传 + 附件账本 + 思库侧渲染） | ✅ P0/P1 真机通过；P2/P3 已实现待真机 | 低：飞书上传为固定形状 multipart |

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
   - 微信图片/文件：CDN 下载（`full_url` 优先，否则 `encrypt_query_param` 拼 `novac2c.cdn.weixin.qq.com/c2c/download`）+ AES-128-ECB 解密（密钥两种编码：base64(raw16) / base64(hex32)，图片另有 aeskey hex 优先；RustCrypto `aes` 纯 Rust 实现）；图片进视觉输入，文件落盘 `remote_downloads/`（路径存 MediaRef.file_key）后同样只提文本注入回合（d05fd90 起**不再自动导入文献库**，`weixin.rs` 里"router auto-imports PDFs"的注释是旧描述）。**语音**：直接读 `voice_item.text`（微信服务端自动转写，官方插件同路径），无需 silk 解码；无转写结果时回执引导发文字。
   - 出站媒体（电脑 → 手机）：✅ 微信私聊真机通过，飞书上传已实现（待真机），方案见 §7。另未做：飞书卡片远程审批。
6. **工作状态展示**：✅ 已实现——
   - **共用进度管线**（`router.rs`）：回合开始挂 `app.listen("agent:event")` 按 session_id 过滤，聚合 `RemoteProgress`（工具行动态含耗时、流式正文/推理摘要、计时），2s 节流推 `ReplySink::progress_update`；`progress_start`/`progress_finish` 负责建/收，finish 返回 true 时跳过纯文本补发。
   - **飞书活卡片**：interactive 卡片 + `PATCH /im/v1/messages/{id}` 原地更新。回合开始发卡（问题摘要+计时）→ 工具行实时追加（中文化标签：阅读文献/检索文献库/联网搜索…，完成打勾+耗时）→ 流式正文预览 → 完成 PATCH 为 ✅ 绿卡（正文+用时+工具数）/失败红卡；>2500 字降级"分条发送"。卡片创建失败自动回退纯文本。
   - **微信进度面 = 原生 typing**：✅ 已实现——4s「收到，正在思考…」/ 15s「还在处理中…」两条延迟文本回执已下线（iLink 无消息编辑端点，每条回执都是独立气泡，长回合刷屏）。改为 `POST /ilink/bot/sendtyping`（status 1/2），`typing_ticket` 由 `/ilink/bot/getconfig` 下发并按 chat 内存缓存 12h；回合开始立刻置位 + **5s 心跳**续期，`progress_finish` 发 CANCEL。取不到 ticket / 发送失败一律**静默**，不回退文本。细节见 §9。
   - 二期候选：思考过程折叠展示。✅ 已实现卡片按钮回调（`card.action.trigger` 回调，需在控制台「回调订阅」添加并长连接接收）：运行中卡片带「停止生成」（取消 agent 回合）、完成/失败卡片带「重新生成/重试」（原提示词经 enqueue 重跑一轮，旧卡标记"已重试 ↻"，`retried` 标志防连点/重投重复开回合）、有推理内容时带「思考过程」展开/收起（按钮 value 带目标态幂等设置，防 at-least-once 重投双翻转）；**三个按钮全部走"应答内返回卡片"**（`EventResp` body 携带 `{"card":{"type":"raw","data":...}}`，平台收到立即渲染，零额外 PATCH、无应答超时重发窗口、状态与界面不脱节）；旧卡点击按 message_id 防串。
7. **出站文件回传（电脑 → 手机）**：✅ 微信私聊真机通过；飞书上传与边界场景已实现（待真机）。实施细节见 §7。

## 7. 出站文件回传（电脑 → 手机）实施方案

> 状态：**P0/P1 已真机实测通过（微信私聊）**：图片/PDF 发送正常，对话侧附件卡与手机端一致。**P2（飞书上传）与 P3（边界场景）已实现**，代码通过 `cargo check` / `tsc --noEmit` / eslint 与单元测试，飞书侧待真机（需要一个自建应用 + 测试群）。
>
> 目标：把"思库所在电脑上的文件发到用户微信/飞书"做成第一类能力，并让思库对话与 IM 侧**语义一致**（不追求像素一致——同一个人在手机上是文件气泡、在思库是本机文件卡，这不叫不一致）。

### 7.1 设计原则：同源投递

一条回合只产出一条账本记录；文件不是"另一条消息"，而是**这条回复的附件**。

```
agent 调 send_file_to_user(path, caption)
        │ 只登记，不发送
        ▼
Arc<Mutex<Vec<OutboundFile>>>（router 创建，逐层传入）
        ├─▶ 引擎写 assistant 行时序列化进 attachments    ← 账本（权威）
        └─▶ 回合结束后 router 逐个 sink.send_media()      ← 投递
                失败 → 用 assistant 行 id UPDATE status=failed
```

三条硬约束：

1. **工具不发文件**。直接在工具里发会绕过 router（唯一的回发出口 `send_text`/`progress_finish`），出现"IM 已收到但对话里查不到"。
2. **队列不是全局静态表**。由 `router::enqueue` 创建 `Arc`，作为新参数传入 `run_agent_turn`（与 `tools_override` 并列），工具登记与落库共享同一份内存；全局 map 会在取消/异常路径泄漏，多会话并发还可能串。
3. **attachments 只存元数据，绝不存 base64**。`chat_messages` 在 `OPTIONAL_SYNC_TABLES` 里，MB 级文件会把同步流量打爆（用户图片附件现在就是内联 base64，那是 Kb 级截图，不可套用到 PDF）。

落库者是**引擎**（`run_agent_turn` 收尾处 `chat::save_chat_message(..., "assistant", ...)` 写 assistant 行，attachments 实参当前恒为 `None`），不是 router——router 只有 rowid 基线（`wait_for_new_reply`），拿不到行 id，所以"router 落库"不成立；正确接法就是上面的队列透传。

### 7.2 微信 iLink 上行协议（官方插件还原）

官方插件 `@tencent-weixin/openclaw-weixin` 自 1.0.3 起就有完整上行实现，2.4.10 仍在维护（思库 `MIRROR_VERSION` 已镜像到 2.4.10，正好与带媒体能力的版本一致）。参考 `src/cdn/upload.ts`、`src/cdn/cdn-upload.ts`、`src/messaging/send-media.ts`、`src/api/api.ts#getUploadUrl`。

**① 申请上传凭据**

```
POST {baseUrl}/ilink/bot/getuploadurl     headers 同现有 common_headers()
{
  "filekey": "<32 位随机 hex>",
  "media_type": 1|2|3|4,          // 1 图 2 视频 3 文件 4 语音
  "to_user_id": "<会话对方 id>",
  "rawsize": 明文字节数,
  "rawfilemd5": 明文 md5（hex）,
  "filesize": 密文字节数 = ceil((rawsize+1)/16)*16,
  "no_need_thumb": true,           // 不上传缩略图（官方即如此）
  "aeskey": "<16 字节 hex>",
  "base_info": {"channel_version": "2.4.10", "bot_agent": "siku/<version>"}
}
→ { "upload_full_url"?: "...", "upload_param"?: "..." }   // 二选一，full_url 优先
```

**② 上传密文（与入站下载同一个 CDN 域名）**

```
POST {upload_full_url}
或  POST https://novac2c.cdn.weixin.qq.com/c2c/upload?encrypted_query_param=<urlenc(upload_param)>&filekey=<filekey>
Content-Type: application/octet-stream
body = AES-128-ECB + PKCS7(明文, aeskey)
→ 200：响应头 x-encrypted-param 即下载参数
→ 4xx：立即失败；5xx：退避重试 3 次
```

复用现状：`CDN_BASE_URL`（`weixin.rs`）、`aes` crate（加密方向换 `BlockEncrypt`）、`hex`/`base64`/`rand` 已有。**唯一新依赖**是 md5：`md-5 = "0.10"`（`Cargo.lock` 已有 0.10.6 的传递依赖，不新增下载）。

**③ 发消息**（`message_type: 2`、`message_state: 2`；**每次 `item_list` 只放一个 item**，说明文字单独发一条）

```jsonc
// 文件
{"type":4,"file_item":{"media":{"encrypt_query_param":"<x-encrypted-param>",
  "aes_key":"<base64(32 位 hex 字符串)>","encrypt_type":1},
  "file_name":"论文.pdf","len":"<明文字节数字符串>"}}
// 图片   {"type":2,"image_item":{"media":{...},"mid_size":<密文字节数>}}
// 视频   {"type":5,"video_item":{"media":{...},"video_size":<密文字节数>}}
// 说明   {"type":1,"text_item":{"text":"…"}}
```

四个坑（都踩过官方实现的注释里）：

- `media.aes_key` 是 **base64(32 字符 hex 字符串的 ASCII)**，不是 base64(16 原始字节)——与现有 `parse_aes_key` 的 base64(hex32) 分支兼容，回环自测天然对得上。
- `file_item.len` 是明文字节数的**字符串**；`mid_size`/`video_size` 是密文长度的**整数**。
- `filesize` 算错（未按 PKCS7 填充）会被服务端拒或对方解密失败。
- 单文件上限：插件默认 25MB（`maxSingleMediaBytes`），服务端真实上限待实测。

### 7.2b 飞书上行（已实现）

两个上传端点 + 一条消息，均为 `Authorization: Bearer <tenant_access_token>`：

| 类型 | 上传 | 发送 |
|---|---|---|
| 图片 | `POST /open-apis/im/v1/images`，multipart：`image_type=message` + `image` | `msg_type=image`，`content={"image_key":…}` |
| 文件 | `POST /open-apis/im/v1/files`，multipart：`file_type` + `file_name` + `file` | `msg_type=file`，`content={"file_key":…}` |

- `file_type` 是飞书的封闭枚举：`opus/video→mp4/pdf/doc/xls/ppt/stream`（`feishu_file_type()`）。
- multipart **手工拼装**：reqwest 的 `multipart` feature 会引入额外依赖，而这里只有固定两个字段 + 一个文件段，不值得。
- part 头的 `filename` 用 ASCII 兜底（`ascii_filename()`），真实中文名走 `file_name` 表单字段——飞书以该字段为准。
- 上限：文件 30MB、图片 10MB（飞书侧）；工具侧统一 25MB 先拦一道。
- 传输错误/5xx 重试 2 次，4xx 直接失败；错误码经 `feishu_data()` 转成可读信息（"请确认权限已开通并已发布版本"）。

### 7.3 数据结构（attachments 判别联合）

```jsonc
// 用户侧（现状，不改）：内联 base64 图片，仅 user 行渲染
[{"mime":"image/png","base64":"…","name":"截图.png"}]
// 助手侧（新增）：只元数据，无 base64
[{"kind":"file","name":"论文.pdf","size":1245184,"mime":"application/pdf",
  "path":"…/remote_outbox/xxx.pdf","sha256":"…",
  "status":"pending|ok|failed","error":null}]
```

- 无 `kind` 字段的老行按"用户侧内联图片"处理，`parseAttachments` 保持向后兼容。
- `path` 是设备本地路径，**发送的是原文件、不复制**（`remote_outbox/` 只是允许读取的根目录之一，供 agent 自己产出的文件放置）；跨设备同步后文件不存在 → 卡片灰化 + "文件不在此设备"。
- 不存 base64 → 其它设备不承担文件体积；也意味着**siku 侧只能看到元数据**，这是刻意的取舍。

### 7.4 状态机与取消/失败策略

```
queued(工具登记，仅内存) → pending(assistant 行落库) → ok(上传+发送成功)
                                                   → failed(错误码写回行)
```

- 回写 `failed` 需要 assistant 行 id：`wait_for_new_reply` 改成返回 `(id, content)`（现在只 `SELECT content`）。这是对同步表的第二次写，会多一份 CRDT 流量，可接受。
- **取消**（飞书「停止生成」）：不发送文件；行已存在则标 `failed: 已取消`。用户刚点停止，再发文件是噪音。
- **回合错误**：不发送，标 `failed: <错误>`。
- **assistant 行写失败**（db error）：不发送，`send_text` 补一句"附件未能随回复发出"。
- **回写后必须广播** `chat:message_updated {session_id, message_id, attachments}`：前端在 `done` 时就重载过历史（那时行里还是 `pending`），不推这条事件的话气泡会一直停在「发送中」，直到用户切标签再回来。前端在 `ChatPanel` 里按 message_id 就地补丁；行尚未加载时兜底重读一次会话。
- 幂等：`path + size + sha256` 去重；工具被重复调用不重复发送。
- **投递顺序与 typing**：`progress_finish` 不再停 typing；router 在"回复文本 + 文件全部投递完"之后才调 `progress_settle`（`ReplySink` 新方法）停心跳并发 CANCEL。否则 20MB 上传的数秒内手机侧完全静默——typing 正是那段等待的唯一反馈。心跳另设 900s 硬上限，防止 settle 丢失时残留在后台。
- **失败可重试**：`remote_attachment_retry(session_id, message_id, index)` 按账本索引重发单个附件（不重跑回合），成功后 `patch_attachment_status` 改回 `ok`、清掉 `error` 并广播 `chat:message_updated`。守卫与原发送一致：仅私聊 + 总开关开启；群聊/只读/开关关闭直接报错。

### 7.5 权限与限额（安全边界）

- **工具始终注册，能力在调用期判定**（`run_agent_turn`）：凡是远程回合（router 传入出站队列）就注册 `send_file_to_user`，把"不可用原因"带进工具；命中时 `execute` 返回**具体原因 + 替代方案**，而不是让工具消失。这条是 2026-11 的实测修正：工具不注册时模型看不到任何信号，会以为"能力一致"（旧提示词原话），于是用 `bash`/文件工具反复拼凑。
- 四种拒绝情形：群聊 / 只读授权 / 管理员关闭总开关 / 非远程会话。**群聊一律拒发**——远程会话按 chat 共享（同群已配对成员共用一个 `session_id`），发文件等于广播给全群；且成员授权可被单独升到 full（`RemoteSettings.tsx`），所以授权级别兜不住这一层。
- **总开关**「允许远程回传文件」存 `remote_config.allow_outbound_files`（随账号同步，`RemoteSettings` 手写 `Default` 保证缺省为开，历史 JSON 无该字段也按开处理）。关闭后工具**根本不注册**，模型看不到、也调不到。
- `readonly` 授权不注册该工具（`READONLY_TOOLS_JSON` 不含它）。
- 路径白名单：会话工作区、`remote_downloads/`、`remote_outbox/`、桌面/下载/文档目录；拒绝 `~/.ssh`、浏览器 profile、应用数据目录等敏感路径。
- 限额：单文件 ≤25MB；每回合 ≤5 个、合计 ≤50MB。
- 文件发送是数据出口：以上四条缺一不可，远程侧没有审批弹窗（`ApprovalMode::Auto`），这些就是全部边界。

### 7.6 思库侧渲染

- assistant 气泡下方挂附件卡：文件显示图标 + 文件名 + 大小 + 状态；点击走 `file_browser_open_in_system` 用系统程序打开（命令已存在）。
- 图片（`mime: image/*` 且本机存在）：内联缩略图 + 点击放大——`assetProtocol` 已开（`tauri.conf.json`，scope `**`）且 CSP 允许 `asset:`，走 `convertFileSrc`（参考 `MarkdownImage` 的本地路径解析）。**零成本**。
- 需改的前端：`ChatAttachment` 扩判别联合；`MessageBubble` 增加 assistant 附件分支（现在只在 `isUser` 渲染）；新增文件卡片组件；`AttachmentImage` 写死 `data:${mime};base64,` 必须加分支，否则文件型附件渲染成裂图。
- 失败态卡片带「重新发送」：卡片拿到 `sessionId/messageId/index`（`MessageBubble` 按**原始数组下标**映射，保证与账本索引一致），点击调 `remote_attachment_retry`；成功由 `chat:message_updated` 事件把卡片翻成「已发送到手机」，不依赖手动刷新。
- 工具行仍留在 `agent_steps` 作审计线（`send_file_to_user · 已完成`），与飞书卡片的工具行天然对应；附件卡是"发了什么"的权威记录，二者互补不冗余。

### 7.7 形态差异（接受，不追求抹平）

- **飞书**：文件只能是卡片之外的独立消息，且必然在 `progress_finish`（卡片定稿）之后到达，视觉上脱离"本轮"。可选：带文件的回合不发卡片，直接走文本 + 文件。
- **微信**：上传期间 typing 一直亮着（CANCEL 挪到投递完成后），所以"发大文件时手机没反应"的窗口被消掉。
- **微信**：正文 2000 字符分条 + 文件气泡交错，siku 侧仍是"一段正文 + 一张卡"。
- IM 侧原生气泡可转发/收藏/"用其他应用打开"，siku 侧是本机文件卡——形态不同是必然，也是合理的：siku 的文件就在本机磁盘，直接打开比下载更顺。
- 投递结果单向：CDN 失败只体现在附件卡状态与工具结果，IM 侧补一句说明；siku 不知道用户是否点开。

### 7.8 改动清单

> P0/P1 已落地的部分标 ✅，其余为待办。

Rust：

1. ✅ `remote/weixin.rs`：`get_upload_url()` + `cdn_upload()`（AES-128-ECB 加密 + md5 + `x-encrypted-param`）+ `send_file()` / `send_message_item()`；`WeixinSink::send_media` 接上；`send_file_now()` 供 P0 命令使用。
2. ✅ `remote/router.rs`：`OutboundFile` / `OutboundQueue` / 附件 JSON 三件套；`ReplySink::send_media`（默认"渠道不支持"）；`enqueue` 建队列并透传；`wait_for_new_reply` 返回 `id/content/attachments`；`deliver_outbound_files()` 投递并回写状态。
3. ✅ `commands/agent.rs`：`run_agent_turn` 增 `outbound` 参数（4 处调用点同步）；assistant 行按回合终态写 `pending` / `failed`；错误分支同样记录。
4. ✅ `ai/agent/tools/send_file_to_user.rs`（新）：白名单（工作区/remote_downloads/remote_outbox/桌面/下载/文档）、25MB/5 个/50MB 限额、sha256 去重、只 push 队列；3 个单测。
5. ✅ `Cargo.toml`：`md-5 = "0.10"`（lock 已有，无新下载）。
6. ✅ `remote/feishu.rs`：`send_media`（`im/v1/files` / `im/v1/images` 上传 → 发 `file`/`image` 消息），手工 multipart + 2 次重试；3 个单测（file_type 映射 / ASCII 文件名 / multipart 字节形状）。

前端：

7. ✅ `lib/types.ts`：attachments 判别联合（`kind`、`size`、`path`、`status`、`error`；`base64` 转可选）。
8. ✅ `MessageBubble.tsx`：assistant 分支渲染附件卡（用户侧仍只渲染内联图片，`AttachmentImage` 兼容无 base64 的行）。
9. ✅ `components/chat/FileAttachmentCard.tsx`：图标/大小/状态（发送中·已发送到手机·发送失败+原因）、图片缩略图（asset 协议）、点击用系统程序打开、跨设备缺文件时提示"文件不在此设备"。
10. ✅ `RemoteSettings.tsx`：P0 冒烟按钮「发送文件到微信」+「允许远程回传文件」总开关（命令 `remote_outbound_files_enabled/set`，随账号同步）；`FileAttachmentCard` 挂载时用 `file_browser_get_info` 探测文件是否存在，跨设备直接显示「文件不在此设备」。

### 7.9 分阶段与验收

| 阶段 | 内容 | 状态 | 验收 |
|---|---|---|---|
| P0 | 微信协议冒烟：设置页扫描件直发（`remote_weixin_send_file`，不走 agent） | ✅ 真机通过 | `getuploadurl` 对现有 bot token 放行 |
| P1 | `send_file_to_user` + 队列 + 附件账本 + 思库侧渲染 | ✅ 真机通过 | 手机收到文件，对话出现附件卡且状态 `ok` |
| P2 | 飞书 `send_media`（图片 `im/v1/images`、文件 `im/v1/files`） | ✅ 代码就绪，⬜ 待真机 | 飞书私聊能收到图片与 PDF；群聊被拒（工具不注册） |
| P3 | 超限/上传中断/停止生成/群聊拒发/跨设备灰化/总开关 | ✅ 已实现，⬜ 待真机 | 断网、超限、点停止、切设备四种场景状态正确 |

风险：

- **服务端能力开关未知**：官方插件能发不代表任意 bot token 都能拿到 upload url（可能看 client version 或机器人类型）——P0 一天内可判定，是最大不确定项。
- 协议非正式规范，版本常量必须集中一处（现已在 `weixin.rs` 顶部）；升级镜像版本时同步复核本节的字段。
- 实现完成后需同步更新 `docs/agent-tools.md`（新增 `send_file_to_user` 一行）与本节状态。

## 8. 验证方式

- 飞书：测试自建应用拉测试群，覆盖 私聊/@群聊/未配对拒绝/防抖合并/重连
- 微信：ClawBot 私聊全流程 + 重启验证 sync buf 续传不重复消费
- 出站文件回传：✅ 微信私聊图片/PDF 真机通过（2026-11）。剩余待补：20MB+ 超限 / 上传中断 / 点「停止生成」/ 群聊调用被拒 / 跨设备打开灰化
- 回归：`cargo check`、`tsc --noEmit`、eslint；远程会话在对话列表可见且流式正常

## 9. 微信工作状态（typing / 工具进度）backlog

> 背景：4s/15s 两条延迟文本回执已下线（§6 第 6 项）。**P1（typing 心跳）已实现**；P2（工具进度行）待评估，下面是协议能力与取舍。

### 9.1 协议能力（官方插件 2.4.10 源码核实）

| 能力 | 端点 / 字段 | 说明 |
|---|---|---|
| 打字指示 | `POST /ilink/bot/sendtyping` `{ilink_user_id, typing_ticket, status}` | `status` 1=正在输入、2=取消；官方按 5s 心跳续期（`keepaliveIntervalMs`） |
| 取 ticket | `POST /ilink/bot/getconfig` `{ilink_user_id, context_token}` → `typing_ticket` | 按用户缓存，24h 随机刷新 + 失败指数退避 |
| 工具进度项 | `item_list[].type = 11`（TOOL_CALL_START）/ `12`（TOOL_CALL_RESULT） | 官方 `reply-progress-sender.ts` 在回合中逐条发，`replyProgressMessages` 默认开 |

### 9.2 关键约束：iLink **没有原地更新**

官方包一共只有 7 个端点：`getupdates`、`getuploadurl`、`sendmessage`、`getconfig`、`sendtyping`、`msg/notifystop`、`msg/notifystart`。**没有 PATCH/update 消息的接口**，且官方所有出站消息都写死 `message_state: 2 (FINISH)`。

结论：
- 飞书卡片那种"一个回合一个面、原地 PATCH"在微信侧**做不到**。11/12 进度项是**追加**：每个工具的 start 和 result 各占一条聊天记录，一次 5 工具回合 = 10 条气泡 + 最终回复。
- 类型枚举里的 `message_state: 1 (GENERATING)` 官方从未使用，也没有配套端点——只能作为逆向探针去试，**不作为方案基础**。
- 因此 P2 必须自带节流/聚合，否则刷屏。

### 9.3 待办

| 优先级 | 方案 | 形态 | 备注 |
|---|---|---|---|
| **P1 ✅ 已实现并真机验证** | typing 心跳：`fetch_typing_ticket()` + `send_typing()`；`progress_start` 起后台任务（首次立即置位，之后 5s 续期），`progress_finish` 中止任务 + 发 CANCEL | 零消息噪音（手机顶部"正在输入…"） | ticket 用**进程内** `TypingCache` 缓存（按 chat，12h TTL，空 ticket 也缓存=「该 bot 不支持」），不写同步表；失败全程静默 |
| P2 | 工具进度行 11/12 | — | **明确不做**（2026-11 决策）：iLink 无消息编辑端点，进度项只能追加，一个多工具回合就是一片气泡；typing 已足够表达"在工作"，逐工具直播性价比低 |
| P3 ✅ | 回执收敛 | — | 已随下线完成（不再有任何"正在思考"文本） |

### 9.4 验证方式

- 真机发一条会跑 >10s 的请求，观察微信聊天顶部是否出现"对方正在输入…"，并在回复到达后消失
- typing 失败（ticket 为空）时应当**完全静默**，不得回退成文本回执
- 工具进度行不做，因此微信侧一次回合**只应产生：可选说明文字 + 附件 + 最终回复**，不应出现任何工具行气泡

