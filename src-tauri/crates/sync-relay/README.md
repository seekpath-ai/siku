# siku-sync-relay

Siku 多端同步的官方中继服务。职责三块：**WebRTC 信令转发**（设备发现 + SDP/ICE）、**账户服务**（注册/登录/设备管理）、**加密邮箱**（离线投递，SQLite 持久化）。服务只接触密文与账户元数据，**不接触用户明文数据**。

> ⚠️ 已知限制：账号级同步密钥（`sync_key`）目前由服务器在注册时生成、登录时下发，即服务器持有密钥——并非严格 E2E。改造方案（客户端持钥 + 设备配对）见 `docs/sync-hardening-plan.md` #1。

> 📌 版本要求：本服务实现协议 v2（`ServerHello` + `mailbox_deposit_ack`）。客户端与 relay 必须同批升级——旧 relay 不回 ack，新客户端会判定为「relay 过旧」并暂停 mailbox 投递（数据不丢，走 outbox 兜底重投）。

## 功能

- WebSocket 接入：`/v1/signaling`，token 走 `Authorization: Bearer <jwt>` 头（旧客户端的 `?token=` 查询串仍兼容）
- 协议握手：join 成功后服务端回发 `server_hello { protocol: 2 }`，客户端据此识别 relay 能力（无 hello 或 protocol < 2 = 旧版 relay，mailbox 确认不可用）
- 账户 API：`/api/register`、`/api/login`、`/api/devices`（列表/改名/移除）
- JWT（HS256）认证：token 由 `/api/login` 服务端签发（有效期 7 天，claims 含 `jti`）
- 设备校验：已移除或未知设备的连接在 WS 握手阶段直接拒绝
- 房间（room）管理：同一 room（= 用户 id）内的设备互相可见；一个设备的多条连接（发现/会话/邮箱）互不影响在线状态
- `PeerOnline` / `PeerOffline` / `Presence` 状态广播
- `signal` 消息转发（offer / answer / ICE candidate）
- 通用加密转发 `relay`（密文透传，不落盘）
- **加密邮箱（离线投递，SQLite 持久化）**：`mailbox_deposit` / `mailbox_poll` / `mailbox_ack`
  - per-device 队列（上限 500 条）+ 账号级 archive（上限 2000 条，空 `to_device_id` 即投递到账号级，未来设备也能拉到）
  - 只存密文，TTL 默认 7 天，FIFO 超限丢最旧；重启不丢消息
  - 投递确认：每个 `mailbox_deposit` 落库后回 `mailbox_deposit_ack`（含拒绝，携带客户端 `message_id`；同 id 重投为幂等 no-op）
  - per-device 消息：poll 只标记 `delivered_at` 不删除，**ack 才删**；60 秒未 ack 自动重投（客户端按游标/幂等键去重）
- 心跳保活：服务端按间隔发 `ping`，超过接收超时未收到任何消息即断开
- 健康检查：`/healthz`

## 快速启动

### 本地源码运行

```bash
cd src-tauri/crates/sync-relay
cargo run
```

默认监听 `127.0.0.1:8080`，默认 `JWT_SECRET=siku-dev-secret-change-me`；账户库与邮箱库均默认存内存（`:memory:`，重启即丢，仅适合本地开发）。

### Docker Compose

> 本目录的 `docker-compose.yml` 是**本地开发最小起**（无持久化卷、无 TLS）。生产部署（持久化 + Caddy TLS）请用仓库根目录的 `docker/docker-compose.yml`，见下方「生产部署」。

```bash
cd src-tauri/crates/sync-relay
# 使用默认密钥
docker-compose up --build

# 或指定自定义密钥与持久化数据库（邮箱库默认紧随账户库：<RELAY_DB_PATH>.mailbox.sqlite）
JWT_SECRET=your-strong-secret RELAY_DB_PATH=/data/relay.json docker-compose up --build
```

服务暴露在宿主机的 `8080` 端口。

> ⚠️ 本目录的 Dockerfile 未挂载持久化卷，容器重建会丢失账户与邮箱数据。生产部署请挂载 volume 并设置 `RELAY_DB_PATH`（邮箱库可用 `RELAY_MAILBOX_DB_PATH` 单独指定）。

### Docker Compose（生产，推荐）

仓库根目录的 `docker/docker-compose.yml` 是推荐的生产编排：账户库与邮箱库持久化到 volume、relay 只绑本机回环（`127.0.0.1:8080`）、强制显式提供 `JWT_SECRET`，并内置 Caddy TLS 终止（`tls` profile）：

```bash
cd docker
# 裸 relay（仅本机回环，前置自己的反向代理）
JWT_SECRET=your-strong-secret docker compose up -d --build

# relay + Caddy 自动 HTTPS（公网域名）
JWT_SECRET=your-strong-secret RELAY_DOMAIN=relay.example.com \
  docker compose --profile tls up -d --build
```

本目录另附 `docker-compose.prod.yml`（无 TLS 的简化版）：仅绑定本机回环地址、数据持久化到 volume、带健康检查，并要求显式提供 `JWT_SECRET`（防止默认密钥上线）：

```bash
cd src-tauri/crates/sync-relay
JWT_SECRET=your-strong-secret docker-compose -f docker-compose.prod.yml up -d --build
```

## 生产部署（nginx 为例）

### 部署拓扑

```
Siku 客户端 ──wss://relay.siku.app──▶ nginx（TLS 终止 + 反代）──▶ relay（127.0.0.1:8080）
```

TLS 在 nginx 终止；relay 只监听本机回环地址，不直接暴露公网。

### 1. 启动 relay（Docker 方式）

```bash
cd src-tauri/crates/sync-relay
JWT_SECRET=<强随机密钥> docker-compose -f docker-compose.prod.yml up -d --build
docker-compose -f docker-compose.prod.yml ps   # healthy
```

### 2. nginx 反向代理配置

WebSocket 需要 HTTP/1.1 升级头；`proxy_read_timeout` 必须大于 relay 的心跳间隔（服务端每 30s 发 ping，消息流不断，180s 只是兜底）：

```nginx
# /etc/nginx/conf.d/relay.conf
# conf.d 下的文件位于 http{} 内，map / limit_req_zone 放文件顶层即合法

# WebSocket 升级头映射
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}

# 登录/注册接口限速，防暴力破解
limit_req_zone $binary_remote_addr zone=relay_auth:10m rate=5r/m;

# HTTP → HTTPS 跳转
server {
    listen 80;
    server_name relay.siku.app;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl http2;
    server_name relay.siku.app;

    ssl_certificate     /etc/letsencrypt/live/relay.siku.app/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/relay.siku.app/privkey.pem;

    # WebSocket 长连接兜底超时（relay 心跳 30s，正常不会触发）
    proxy_read_timeout 180s;
    proxy_send_timeout 180s;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    location ~ ^/api/(login|register)$ {
        limit_req zone=relay_auth burst=10 nodelay;
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

> `listen 443 ssl http2;` 兼容 Debian 12 / Ubuntu 24.04 的 nginx 版本；新版 nginx（1.25.1+）推荐改为 `listen 443 ssl;` + `http2 on;`。

### 3. TLS 证书（certbot）

```bash
sudo apt install certbot python3-certbot-nginx
sudo certbot --nginx -d relay.siku.app   # 自动签发并写入 nginx 配置
sudo certbot renew --dry-run             # 验证自动续期
```

### 4. 防火墙

```bash
# 仅放行 80/443；8080 不需要公网放行（relay 只监听 127.0.0.1）
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw enable
```

### 5. 验证

```bash
curl https://relay.siku.app/healthz                      # ok
# 注册 + 登录 + WebSocket 联调，见下方「测试 WebSocket 连接」章节
```

### 不用 Docker 的替代：systemd 直跑

```ini
# /etc/systemd/system/siku-sync-relay.service
[Unit]
Description=Siku sync relay
After=network.target

[Service]
User=siku-relay
WorkingDirectory=/opt/siku-sync-relay
EnvironmentFile=/etc/siku-relay.env
ExecStart=/opt/siku-sync-relay/siku-sync-relay
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

```bash
# /etc/siku-relay.env
JWT_SECRET=<强随机密钥>
RELAY_HOST=127.0.0.1
RELAY_PORT=8080
RELAY_DB_PATH=/var/lib/siku-relay/relay.json
RUST_LOG=info
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now siku-sync-relay
```

> 仓库另附 `Caddyfile` 供 Caddy 用户参考；nginx 用户以上面配置为准。

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `RELAY_HOST` | `127.0.0.1` | 监听地址。容器内请用 `0.0.0.0` |
| `RELAY_PORT` | `8080` | 监听端口 |
| `JWT_SECRET` | `siku-dev-secret-change-me` | HS256 签名密钥（账户 token 签发 + WS token 验签） |
| `RELAY_DB_PATH` | `:memory:` | 账户/设备持久化路径（JSON 文件） |
| `RELAY_MAILBOX_DB_PATH` | `<RELAY_DB_PATH>.mailbox.sqlite` | 邮箱持久化路径（SQLite）。`RELAY_DB_PATH=:memory:` 时默认同为 `:memory:` |
| `HEARTBEAT_INTERVAL_SECONDS` | `30` | 服务端发送 Ping 的间隔 |
| `HEARTBEAT_TIMEOUT_SECONDS` | `60` | 多久没收到任何消息就断开（`docker-compose.yml` 显式覆盖为 `120`） |
| `RUST_LOG` | `info` | 日志级别 |

## 账户与 Token

### 注册 + 登录（推荐）

```bash
# 注册
curl -X POST http://127.0.0.1:8080/api/register \
  -H 'Content-Type: application/json' \
  -d '{"email":"me@example.com","password":"hunter2"}'

# 登录：返回 access_token（7 天）、user_id、sync_key（账号级同步密钥，base64）
curl -X POST http://127.0.0.1:8080/api/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"me@example.com","password":"hunter2","device_id":"device-a","device_name":"我的电脑"}'
```

### 设备管理（需要 Bearer token）

```bash
TOKEN=<上一步的 access_token>
# 列出本账号设备
curl http://127.0.0.1:8080/api/devices -H "Authorization: Bearer $TOKEN"
# 改名 / 移除（移除后该设备无法再连接）
curl -X PATCH http://127.0.0.1:8080/api/devices/<device_id> \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{"name":"新名字"}'
curl -X DELETE http://127.0.0.1:8080/api/devices/<device_id> -H "Authorization: Bearer $TOKEN"
```

### PoC：自行签发 WS token（无账户场景）

WS 握手只要求 `sub` / `device_id` / `exp` 三个字段；而设备管理 API 额外要求 `jti`。所以自签 token 只能用于信令直连，不能用于 `/api/*`。

用 Node.js：

```js
const jwt = require('jsonwebtoken');
const secret = process.env.JWT_SECRET || 'siku-dev-secret-change-me';
const token = jwt.sign(
  { sub: 'user-1', device_id: 'device-a' },
  secret,
  { algorithm: 'HS256', expiresIn: '1h' }
);
console.log(token);
```

## 测试 WebSocket 连接

使用 [websocat](https://github.com/vi/websocat) 或任意 WebSocket 客户端：

```bash
TOKEN=$(curl -s -X POST http://127.0.0.1:8080/api/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"me@example.com","password":"hunter2","device_id":"device-a","device_name":"A"}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')
# token 走 Authorization 头（不要放 URL 查询串——会进访问日志/代理日志）
websocat "ws://127.0.0.1:8080/v1/signaling" -H "Authorization: Bearer $TOKEN"
```

连接后发送 join（room 必须与 token 的 `sub` 一致，否则被拒）：

```json
{"type":"join","payload":{"room_id":"<token 里的 sub>"}}
```

join 成功后服务端先回发 `server_hello`（`protocol: 2`），随后推送邮箱存量（`mailbox_batch`）与在线状态。再开另一个终端用 `device-b` 登录并加入同一 room，双方会收到 `peer_online`。

### 协议消息一览

客户端 → 服务端：`join`、`signal`、`relay`、`mailbox_deposit`、`mailbox_poll`、`mailbox_ack`、`pong`

服务端 → 客户端：`server_hello`、`peer_online`、`peer_offline`、`presence`、`signal`、`relay`、`mailbox_batch`、`mailbox_deposit_ack`、`ping`、`error`

## 与 Siku 客户端对接

1. 启动 relay（生产环境置于反向代理后，用 `wss://` 地址）。
2. 在 Siku 设置页「中继服务器」填入 `wss://relay.siku.app/v1/signaling`（本地测试为 `ws://127.0.0.1:8080/v1/signaling`——客户端默认拒绝明文 `ws://`，需在「同步 → 同步范围」开启「允许明文中继（仅局域网调试）」）。
3. 用邮箱 + 密码登录：客户端自动调用 `/api/login` 获取 `access_token` 与 `sync_key` 并本地保存。
4. 同一账号的多个设备登录后自动互相发现并同步；离线期间的变更通过加密邮箱投递（ack 确认 + 失败重投），对方上线即收到。
5. 局域网直连（同网段无需 relay）走独立的 LAN 配对流程，不经过本服务。

> relay 与客户端必须同批升级：protocol v2 的客户端连旧 relay 会报「relay 过旧」并暂停 mailbox 投递（数据经 outbox 兜底，不丢）。

## 数据与安全模型

- **明文不落盘**：邮箱只存加密后的密文（`ciphertext` + `nonce`），SQLite 持久化，重启不丢。
- **密钥现状（非严格 E2E）**：`sync_key` 由服务器在注册时生成、随 `/api/login` 响应下发到各设备——**服务器当前持有密钥**，可解密邮箱载荷。真 E2E（客户端持钥 + 设备配对 + 服务器只存指纹）在 `docs/sync-hardening-plan.md` #1，实施前自托管者需信任自己的 relay 主机。
- **账户凭据**：密码以迭代 SHA-256（10 万轮 + 随机盐，PBKDF2 风格）存储，见 `auth.rs`。
- **设备移除**：`/api/devices/:id` DELETE 直接删除设备记录，其 refresh token 同步失效，后续 WS 连接在握手阶段即被拒绝；重新登录会注册为全新设备。
- **生产注意事项**：
  - 替换默认 `JWT_SECRET`（固定密钥仅用于本地/内部测试）；
  - 挂载持久化卷并设置 `RELAY_DB_PATH`（邮箱库默认紧随其后，或用 `RELAY_MAILBOX_DB_PATH` 单独指定）；
  - 密码哈希若需更强抗性，可换 argon2id（当前为无依赖的务实选择）；
  - 如需横向扩展，把 JSON 文件存储换成 SQLite/Postgres（见 `db.rs` 注释）。
