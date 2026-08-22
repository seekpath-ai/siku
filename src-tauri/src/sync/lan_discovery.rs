use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{watch, Mutex};
use tracing::{info, warn};

pub const LAN_DISCOVERY_PORT: u16 = 53455;
pub const LAN_BROADCAST_INTERVAL_MS: u64 = 1000;
pub const LAN_PEER_TIMEOUT_MS: u64 = 5000;

/// Beacon broadcast by a host that is willing to accept a new device. The
/// pairing code is deliberately NOT broadcast: it is shown only on the host
/// screen and typed manually by the guest, so sniffing the LAN does not leak
/// it. The host validates offers against its own `lan_pairing_code`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanBeacon {
    pub device_id: String,
    pub pairing_payload: String,
}

/// A discovered LAN peer (host).
#[derive(Debug, Clone)]
pub struct LanPeer {
    pub device_id: String,
    pub pairing_payload: String,
    pub addr: SocketAddr,
    pub last_seen: std::time::Instant,
}

/// Handle to a running LAN beacon broadcaster.
pub struct LanBeaconHandle {
    _stop: watch::Sender<()>,
}

impl LanBeaconHandle {
    /// Start broadcasting a beacon on the LAN.
    pub async fn start(beacon: LanBeacon) -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0").await.context("bind LAN beacon socket")?;
        socket
            .set_broadcast(true)
            .context("enable broadcast on beacon socket")?;

        let broadcast_addr: SocketAddr = format!("255.255.255.255:{}", LAN_DISCOVERY_PORT)
            .parse()
            .context("parse broadcast address")?;

        let payload = serde_json::to_vec(&beacon).context("serialize LAN beacon")?;
        let (tx, mut rx) = watch::channel(());

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(LAN_BROADCAST_INTERVAL_MS));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if socket.send_to(&payload, broadcast_addr).await.is_err() {
                            // Broadcasting can fail on some interfaces; ignore individual failures.
                        }
                    }
                    _ = rx.changed() => {
                        info!("stopping LAN beacon");
                        break;
                    }
                }
            }
        });

        info!(port = LAN_DISCOVERY_PORT, "started LAN beacon");
        Ok(Self { _stop: tx })
    }
}

/// Handle to a running LAN peer listener.
pub struct LanListenerHandle {
    peers: Arc<Mutex<HashMap<String, LanPeer>>>,
    _stop: watch::Sender<()>,
}

impl LanListenerHandle {
    /// Start listening for LAN beacons.
    pub async fn start() -> Result<Self> {
        let socket = UdpSocket::bind(format!("0.0.0.0:{}", LAN_DISCOVERY_PORT))
            .await
            .context("bind LAN listener socket")?;

        let peers: Arc<Mutex<HashMap<String, LanPeer>>> = Arc::new(Mutex::new(HashMap::new()));
        let peers2 = peers.clone();
        let (tx, mut rx) = watch::channel(());

        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            let mut cleanup_interval = tokio::time::interval(tokio::time::Duration::from_millis(LAN_PEER_TIMEOUT_MS));
            loop {
                tokio::select! {
                    result = socket.recv_from(&mut buf) => {
                        match result {
                            Ok((len, addr)) => {
                                match serde_json::from_slice::<LanBeacon>(&buf[..len]) {
                                    Ok(beacon) => {
                                        let mut map = peers2.lock().await;
                                        map.insert(
                                            beacon.device_id.clone(),
                                            LanPeer {
                                                device_id: beacon.device_id,
                                                pairing_payload: beacon.pairing_payload,
                                                addr,
                                                last_seen: std::time::Instant::now(),
                                            },
                                        );
                                    }
                                    Err(e) => {
                                        warn!(error = %e, addr = %addr, "ignored malformed LAN beacon");
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "LAN listener recv error");
                            }
                        }
                    }
                    _ = cleanup_interval.tick() => {
                        let now = std::time::Instant::now();
                        let mut map = peers2.lock().await;
                        let timeout = std::time::Duration::from_millis(LAN_PEER_TIMEOUT_MS);
                        map.retain(|_, peer| now.duration_since(peer.last_seen) < timeout);
                    }
                    _ = rx.changed() => {
                        info!("stopping LAN listener");
                        break;
                    }
                }
            }
        });

        info!(port = LAN_DISCOVERY_PORT, "started LAN listener");
        Ok(Self { peers, _stop: tx })
    }

    /// Return a snapshot of currently discovered peers.
    pub async fn peers(&self) -> Vec<LanPeer> {
        self.peers.lock().await.values().cloned().collect()
    }
}

/// Internal helper for tests: send a beacon directly to a target address.
#[cfg(test)]
async fn send_test_beacon(target: SocketAddr, beacon: &LanBeacon) -> Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let payload = serde_json::to_vec(beacon)?;
    socket.send_to(&payload, target).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lan_listener_discovers_beacon() -> Result<()> {
        let listener = LanListenerHandle::start().await?;
        let target: SocketAddr = format!("127.0.0.1:{}", LAN_DISCOVERY_PORT).parse()?;

        // Give the listener a moment to bind.
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let beacon = LanBeacon {
            device_id: "host-abc".into(),
            pairing_payload: "payload-xyz".into(),
        };
        send_test_beacon(target, &beacon).await?;

        // Wait for the beacon to be processed.
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        let peers = listener.peers().await;
        // The listener binds 0.0.0.0:53455 and may pick up beacons from real
        // devices broadcasting on the LAN, so assert on our own beacon rather
        // than on the total count.
        let mine = peers.iter().find(|p| p.device_id == "host-abc");
        assert!(mine.is_some(), "should discover the test beacon; got {peers:?}");
        let mine = mine.unwrap();
        assert_eq!(mine.pairing_payload, "payload-xyz");

        Ok(())
    }
}
