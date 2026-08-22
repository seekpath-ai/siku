use serde::{Deserialize, Serialize};

/// 客户端 → 服务器 的消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Join { payload: JoinPayload },
    Signal { payload: SignalPayload },
    Relay { payload: RelayPayload },
    Presence,
    Pong,
}

/// 服务器 → 客户端 的消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    PeerOnline { payload: PeerPayload },
    PeerOffline { payload: PeerPayload },
    Signal { payload: SignalPayload },
    Relay { payload: RelayPayload },
    Presence { payload: PresencePayload },
    Error { payload: ErrorPayload },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinPayload {
    pub room_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalPayload {
    pub from_device_id: Option<String>,
    pub to_device_id: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayPayload {
    pub from_device_id: Option<String>,
    pub to_device_id: String,
    pub ciphertext: String,
    #[serde(default = "default_relay_ttl")]
    pub ttl_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerPayload {
    pub device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresencePayload {
    pub device_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
}

fn default_relay_ttl() -> u64 {
    300
}
