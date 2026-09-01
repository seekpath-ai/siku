# Siku Sync

多设备同步模块，基于 cr-sqlite（CRR 表）+ WebRTC DataChannel，覆盖 LAN 直连、公网中继、离线信箱三条路径。

## 传输路径

- **LAN P2P**：`lan_discovery.rs`（UDP 广播发现，端口 53455）+ `local_signaling.rs`（UDP 信令交换 SDP/ICE，端口 53456，6 位配对码握手）→ WebRTC DataChannel。
- **公网自动同步**：登录后由命令层（`commands/sync.rs`）启动 auto-sync 代理，经自建 relay（`crates/sync-relay`，axum WebSocket `/v1/signaling`）按账号房间撮合，设备 ID 字典序较小的一方发 offer。
- **离线 mailbox 中继**：P2P 不可达时，加密 changeset 投递到 relay 上的 per-device 信箱（另有账号级存档供未来新设备拉取）；中继 mailbox 已 SQLite 持久化，投递成功以 `MailboxDepositAck` 确认——**发送端仅在 ack 后才推进游标**，失败写本地 `sync_outbox` 表由 `flush_outbox_with` 重试（同 message_id 幂等重投；投递目标已有待投行时跳过重发防堆积，有毒消息/过期清理见 `engine.rs::prune_outbox`）。auto-sync proxy 每个 tick 都会自动 flush outbox。**自托管 relay 必须与客户端同批升级**（`MailboxDepositAck` 协议），旧 relay 不回 ack，客户端会把所有投递视为未确认。
- **种子导出/导入**（`onboarding.rs`）：`VACUUM INTO` 快照 + blobs 打成加密 zip，用于无网络换机。

## 模块

- `types.rs`：协议消息类型（Changeset、AttachmentRequest/Payload、信箱信封等）。
- `crdt.rs`：crsql_changes 导出/应用、旧版全量快照兼容（`apply_full_snapshot` 带 tombstone 过滤：已知删除的行不再插回）、LWW 修正（按 `updated_at` 过滤本地较新的行，严格大于；删除标记无时间戳、总是应用）、密钥类 settings 行过滤。
- `engine.rs`：同步引擎——游标管理（per-peer 持久化在 device_settings）、>16KB 消息分块重组、缺失 blob 按需拉取、outbox 重试。
- `crypto.rs`：AES-256-GCM（每条消息随机 nonce）。
- `webrtc_peer.rs`：PeerConnection / DataChannel 封装。
- `relay_client.rs` / `mailbox_client.rs`：relay WebSocket 客户端 / 加密信箱存取。
- `attachments.rs`：扫描 `papers.file_path`、`attachments.file_path`、`files.blob_path` 及笔记正文中的 `blobs/<hash>` 引用，按需向 peer 请求缺失 blob，写入前 sha256 校验。
- `onboarding.rs`：配对种子导出/导入、JWT 解析。
- `commands/sync.rs`：Tauri 命令层（LAN/cloud 各一个 engine 槽位、配对码流程、状态查询、outbox flush）。

## 数据模型

- 核心同步表（始终同步）：notes、papers、attachments、annotations、vaults、note_versions、note_links、files、tags、paper_tags、collections、paper_collections、related_papers、bookmarks、file_bookmarks、saved_searches、saved_items、imports（见 `core/db.rs::CORE_SYNC_TABLES`）。
- 可选同步表（`sync_optional_data` 开关）：chat_sessions、chat_messages、settings（`app_settings`、`account.*` 与 `notes.current_vault_id` 键永不同步；chat_sessions 的 `working_dir` 列是设备本地绝对路径，导出与应用两侧均剥离）。
- 不同步：creators（接收端从 papers.authors 重建）、device_settings、搜索索引、LLM 提供商等设备本地数据。
- 冲突解决：cr-sqlite 默认 col_version 合并 + 应用前按 `updated_at` 的 LWW 过滤；删除为 delete-wins（已知取舍：peer 旧删除会覆盖本地新编辑）。
- 首次连接与周期存档刷新发送**全量历史 changeset**（从 `crsql_changes` 以 db_version 0 导出，含 tombstone），接收侧走与增量完全相同的 `apply_changes` 合并路径——已删除的行不会被快照复活。旧版 `FullSnapshot`（INSERT OR IGNORE）消息仍被接收侧兼容处理，但插入前会按 `crsql_changes` 中的删除标记过滤。之后按 per-peer 游标增量推送。

## 安全说明

- mailbox 载荷在发送端以 AES-256-GCM 加密，relay 只存密文；但 sync_key 由服务器在注册时生成并在登录时下发，属于"服务器信任的加密存储"，不是严格意义的端到端加密。
- **连接安全**：客户端默认只允许 `wss://`；`ws://` 明文连接需在「同步 → 同步范围」显式开启「允许不加密的中继连接」（仅局域网调试）。JWT 经 `Authorization: Bearer` 头发送（不再放 URL 查询串，避免进代理/访问日志）。部署 relay 必须走 TLS（见 `docker/docker-compose.yml` 的 Caddy 终止）。
- P2P 通道由 WebRTC DTLS 加密；LAN 信令（UDP）仅限本机局域网。
- 中继 mailbox 已 SQLite 持久化（`RELAY_MAILBOX_DB_PATH`），投递经 `MailboxDepositAck` 确认后才推进发送端游标。
