use serde::{Deserialize, Serialize};

/// 客户端 → 中继 的消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayClientMsg {
    Join { payload: JoinPayload },
    Signal { payload: SignalPayload },
    Relay { payload: serde_json::Value },
    MailboxDeposit { payload: MailboxDepositPayload },
    MailboxPoll { payload: MailboxPollPayload },
    MailboxAck { payload: MailboxAckPayload },
    /// Reply to the relay's periodic `Ping`. Without it the relay's receive
    /// timeout drops idle connections, silently marking the device offline.
    Pong,
}

/// 中继 → 客户端 的消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayServerMsg {
    PeerOnline { payload: PeerPayload },
    PeerOffline { payload: PeerPayload },
    Presence { payload: PresencePayload },
    Signal { payload: RelaySignalPayload },
    Relay { payload: serde_json::Value },
    Ping,
    Error { payload: ErrorPayload },
    MailboxBatch { payload: MailboxBatchPayload },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinPayload {
    pub room_id: String,
    /// PoC pairing: the guest joins with the guest identity assigned in the
    /// pairing payload while still authenticating with the host's token.
    /// Removed in the account phase (each device gets its own token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalPayload {
    pub to_device_id: String,
    pub data: SignalData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SignalData {
    Offer { sdp: String },
    Answer { sdp: String },
    Ice {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelaySignalPayload {
    pub from_device_id: String,
    pub to_device_id: String,
    pub data: SignalData,
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

// ── Encrypted mailbox ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxDepositPayload {
    pub to_device_id: String,
    pub ciphertext: String,
    pub nonce: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxPollPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxAckPayload {
    pub message_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxMessage {
    pub id: String,
    pub from_device_id: String,
    pub ciphertext: String,
    pub nonce: String,
    /// True for account-level archive messages (shared by every device of the
    /// account); false for per-device messages.
    #[serde(default)]
    pub account_level: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxBatchPayload {
    pub messages: Vec<MailboxMessage>,
}
