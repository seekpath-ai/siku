use crate::types::{RelayPayload, ServerMessage, SignalPayload};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, warn};

pub type DeviceTx = mpsc::UnboundedSender<ServerMessage>;

#[derive(Clone)]
#[allow(dead_code)]
pub struct Device {
    pub device_id: String,
    pub user_id: String,
    pub room_id: String,
    pub tx: DeviceTx,
    pub last_seen: Instant,
}

#[derive(Default, Clone)]
pub struct RoomManager {
    inner: Arc<RwLock<RoomManagerInner>>,
}

#[derive(Default)]
struct RoomManagerInner {
    /// room_id -> device_id -> Device
    rooms: HashMap<String, HashMap<String, Device>>,
    /// device_id -> room_id
    device_index: HashMap<String, String>,
    /// offline device queued relay messages: device_id -> Vec<(received_at, payload)>
    relay_queue: HashMap<String, Vec<(Instant, RelayPayload)>>,
}

impl RoomManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn join(&self, room_id: String, device: Device) {
        let mut inner = self.inner.write().await;
        inner
            .device_index
            .insert(device.device_id.clone(), room_id.clone());

        let room = inner.rooms.entry(room_id.clone()).or_default();

        // Notify existing peers about the new device.
        let new_device_id = device.device_id.clone();
        for existing in room.values() {
            if existing.device_id == new_device_id {
                continue;
            }
            let _ = existing.tx.send(ServerMessage::PeerOnline {
                payload: crate::types::PeerPayload {
                    device_id: new_device_id.clone(),
                },
            });
        }

        // Notify the new device about existing peers.
        let peer_ids: Vec<String> = room.keys().cloned().collect();
        let _ = device.tx.send(ServerMessage::Presence {
            payload: crate::types::PresencePayload { device_ids: peer_ids },
        });

        room.insert(new_device_id.clone(), device);

        // Flush any queued relay messages for this device.
        if let Some(queue) = inner.relay_queue.remove(&new_device_id) {
            let room = inner.rooms.get(&room_id).expect("room exists");
            if let Some(dev) = room.get(&new_device_id) {
                for (_, payload) in queue {
                    let _ = dev.tx.send(ServerMessage::Relay { payload });
                }
            }
        }

        debug!(room = %room_id, device = %new_device_id, "device joined");
    }

    pub async fn leave(&self, device_id: &str) {
        let mut inner = self.inner.write().await;
        let Some(room_id) = inner.device_index.remove(device_id) else {
            return;
        };

        let should_remove_room = if let Some(room) = inner.rooms.get_mut(&room_id) {
            room.remove(device_id);
            for peer in room.values() {
                let _ = peer.tx.send(ServerMessage::PeerOffline {
                    payload: crate::types::PeerPayload {
                        device_id: device_id.to_string(),
                    },
                });
            }
            room.is_empty()
        } else {
            false
        };

        if should_remove_room {
            inner.rooms.remove(&room_id);
        }

        debug!(room = %room_id, device = %device_id, "device left");
    }

    pub async fn heartbeat(&self, device_id: &str) {
        let mut inner = self.inner.write().await;
        if let Some(room_id) = inner.device_index.get(device_id).cloned() {
            if let Some(room) = inner.rooms.get_mut(&room_id) {
                if let Some(dev) = room.get_mut(device_id) {
                    dev.last_seen = Instant::now();
                }
            }
        }
    }

    pub async fn send_signal(&self, payload: SignalPayload) -> anyhow::Result<()> {
        let inner = self.inner.read().await;
        let Some(room_id) = inner.device_index.get(&payload.to_device_id) else {
            warn!(to = %payload.to_device_id, "signal target offline, dropping");
            return Ok(());
        };
        let Some(room) = inner.rooms.get(room_id) else {
            return Ok(());
        };
        let Some(target) = room.get(&payload.to_device_id) else {
            return Ok(());
        };

        target
            .tx
            .send(ServerMessage::Signal { payload })
            .map_err(|_| anyhow::anyhow!("target channel closed"))?;
        Ok(())
    }

    pub async fn send_relay(&self, payload: RelayPayload, max_queue: usize) -> anyhow::Result<()> {
        let mut inner = self.inner.write().await;
        let Some(room_id) = inner.device_index.get(&payload.to_device_id).cloned() else {
            // Target offline: queue for later delivery.
            let queue = inner.relay_queue.entry(payload.to_device_id.clone()).or_default();
            if queue.len() >= max_queue {
                queue.remove(0);
            }
            queue.push((Instant::now(), payload));
            return Ok(());
        };

        let Some(room) = inner.rooms.get(&room_id) else {
            return Ok(());
        };
        let Some(target) = room.get(&payload.to_device_id) else {
            return Ok(());
        };

        target
            .tx
            .send(ServerMessage::Relay { payload })
            .map_err(|_| anyhow::anyhow!("target channel closed"))?;
        Ok(())
    }

    /// Remove stale devices. Returns list of removed device IDs.
    pub async fn cleanup(&self, timeout: Duration) -> Vec<String> {
        let now = Instant::now();
        let mut to_remove = Vec::new();

        {
            let inner = self.inner.read().await;
            for room in inner.rooms.values() {
                for dev in room.values() {
                    if now.duration_since(dev.last_seen) > timeout {
                        to_remove.push(dev.device_id.clone());
                    }
                }
            }
        }

        for device_id in &to_remove {
            self.leave(device_id).await;
        }

        // Also prune expired queued relay messages.
        {
            let mut inner = self.inner.write().await;
            inner.relay_queue.retain(|_, queue| {
                queue.retain(|(at, _)| now.duration_since(*at) < timeout);
                !queue.is_empty()
            });
        }

        to_remove
    }
}
