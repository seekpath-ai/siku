# Siku Sync（PoC 阶段）

Tauri 主应用内的同步能力原型，用于阶段 2 核心数据同步的基础验证。

## 结构

- `types.rs`：与中继交互的协议消息类型。
- `relay_client.rs`：WebSocket 中继客户端。
- `webrtc_peer.rs`：基于 `webrtc-rs` 的 PeerConnection / DataChannel 封装。

## Tauri 命令

### `start_sync_poc`

输入：

```json
{
  "relayUrl": "ws://localhost:8080/v1/signaling",
  "token": "<jwt>",
  "roomId": "user-1",
  "peerDeviceId": "device-b"
}
```

行为：

1. 连接官方信令中继。
2. 加入房间并等待目标设备上线。
3. 创建 WebRTC DataChannel，生成 offer，经中继转发 SDP/ICE。
4. 等待 answer 并建立 P2P 通道。
5. DataChannel 打开后返回成功。

### `send_sync_message`

输入：文本字符串。向已建立的 DataChannel 发送消息。

## 后续工作（阶段 2）

- 用 DataChannel 承载 CR-SQLite changeset。
- 添加设备配对、同步状态、冲突提示等 UI。
- 实现端到端加密与设备认证。
