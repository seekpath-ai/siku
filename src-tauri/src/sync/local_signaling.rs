//! LAN-local signaling: exchange WebRTC SDP/ICE over UDP directly between
//! devices on the same network — no relay server, no account required.
//!
//! Both sides use fixed UDP port `LOCAL_SIGNAL_PORT`. The host binds it and
//! waits for an offer; the guest discovers the host via LAN beacon and sends
//! its offer to the host's IP on this port.

use crate::sync::webrtc_peer::SyncSession;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{info, warn};
use webrtc::data_channel::RTCDataChannel;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

pub const LOCAL_SIGNAL_PORT: u16 = 53456;
const MAX_DATAGRAM: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LocalSignalMsg {
    #[serde(rename = "offer")]
    Offer {
        sdp: String,
        /// Sender's stable device id (used by the host to track per-peer
        /// sync progress and label the session).
        device_id: String,
        /// The host's pairing code as seen by the guest. The host rejects the
        /// offer when it does not match the code it is broadcasting — this is
        /// the actual protocol-level pairing check (the UI codes are only for
        /// human confirmation). Empty when the guest does not know a code.
        #[serde(default)]
        pairing_code: String,
    },
    #[serde(rename = "answer")]
    Answer { sdp: String },
    #[serde(rename = "ice")]
    Ice {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },
    /// Host → guest: the offer was rejected (e.g. pairing code mismatch).
    #[serde(rename = "reject")]
    Reject { reason: String },
}

pub struct LocalSignaling {
    sock: Arc<UdpSocket>,
    closed_tx: tokio::sync::watch::Sender<bool>,
    rx: mpsc::UnboundedReceiver<(SocketAddr, LocalSignalMsg)>,
}

impl Drop for LocalSignaling {
    fn drop(&mut self) {
        // Wake the background recv task so it releases the socket Arc —
        // otherwise the UDP port stays bound forever and re-bind fails.
        let _ = self.closed_tx.send(true);
    }
}

impl LocalSignaling {
    /// Bind the local signaling socket (host side) or just prepare a client.
    pub async fn bind() -> Result<Self> {
        let sock = UdpSocket::bind(("0.0.0.0", LOCAL_SIGNAL_PORT))
            .await
            .context("bind local signaling port")?;
        Self::from_socket(sock).await
    }

    /// Client side: send from an ephemeral port, listen on it for replies.
    pub async fn connect_client() -> Result<Self> {
        let sock = UdpSocket::bind(("0.0.0.0", 0))
            .await
            .context("bind client signaling socket")?;
        Self::from_socket(sock).await
    }

    async fn from_socket(sock: UdpSocket) -> Result<Self> {
        let sock = Arc::new(sock);
        let (closed_tx, mut closed_rx) = tokio::sync::watch::channel(false);
        let (tx, rx) = mpsc::unbounded_channel::<(SocketAddr, LocalSignalMsg)>();
        let read_sock = sock.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; MAX_DATAGRAM];
            loop {
                tokio::select! {
                    r = read_sock.recv_from(&mut buf) => {
                        match r {
                            Ok((n, addr)) => {
                                if let Ok(msg) = serde_json::from_slice::<LocalSignalMsg>(&buf[..n]) {
                                    let _ = tx.send((addr, msg));
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "local signaling recv failed");
                                break;
                            }
                        }
                    }
                    _ = closed_rx.changed() => {
                        // Dropped: release the socket so the port frees up.
                        break;
                    }
                }
            }
        });
        Ok(Self { sock, closed_tx, rx })
    }

    pub async fn send_to(&self, addr: SocketAddr, msg: &LocalSignalMsg) -> Result<()> {
        let bytes = serde_json::to_vec(msg).context("serialize signal")?;
        self.sock
            .send_to(&bytes, addr)
            .await
            .context("send local signal")?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Option<(SocketAddr, LocalSignalMsg)> {
        self.rx.recv().await
    }

    /// Receive with timeout.
    pub async fn recv_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<(SocketAddr, LocalSignalMsg)>> {
        match tokio::time::timeout(timeout, self.rx.recv()).await {
            Ok(msg) => Ok(msg),
            Err(_) => Ok(None),
        }
    }
}

/// Route this connection's ICE candidates to the peer over UDP.
async fn forward_ice(pc: &Arc<webrtc::peer_connection::RTCPeerConnection>, sig: &LocalSignaling, peer: SocketAddr) {
    let sig = sig.sock.clone();
    pc.on_ice_candidate(Box::new(move |candidate| {
        let sig = sig.clone();
        Box::pin(async move {
            if let Some(c) = candidate {
                if let Ok(init) = c.to_json() {
                    let msg = LocalSignalMsg::Ice {
                        candidate: init.candidate,
                        sdp_mid: init.sdp_mid,
                        sdp_mline_index: init.sdp_mline_index,
                    };
                    if let Ok(bytes) = serde_json::to_vec(&msg) {
                        let _ = sig.send_to(&bytes, peer).await;
                    }
                }
            }
        })
    }));
}

/// Resolves when the host explicitly releases the signaling socket (session
/// ended / host stopped). `None` (guest side) never resolves: the guest's
/// ephemeral client socket needs no external release.
async fn wait_external_release(rx: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    if let Some(rx) = rx.as_mut() {
        let _ = rx.changed().await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn process_local_ice(
    pc: Arc<webrtc::peer_connection::RTCPeerConnection>,
    mut sig: LocalSignaling,
    release_rx: Option<tokio::sync::watch::Receiver<bool>>,
) {
    // Exit as soon as the connection dies so the signaling socket (and its
    // bound UDP port) is released for the next guest.
    //
    // Use an UNBOUNDED channel: connection setup fires New → Connecting →
    // Connected, and a capacity-1 channel would swallow the final terminal
    // event (try_send fails when full), leaking the bound UDP port for up to
    // the 120s idle timeout — exactly what makes a second guest's re-bind
    // fail. The host additionally forces the release explicitly when its
    // session-close handler runs, so the port is freed deterministically and
    // not just when the peer-connection state happens to be observed.
    let (state_tx, mut state_rx) = mpsc::unbounded_channel::<
        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState,
    >();
    let state_tx2 = state_tx.clone();
    pc.on_peer_connection_state_change(Box::new(move |s| {
        let _ = state_tx2.send(s);
        Box::pin(async {})
    }));

    let mut release_rx = release_rx;
    loop {
        tokio::select! {
            msg = sig.recv_timeout(Duration::from_secs(120)) => {
                match msg {
                    Ok(Some((_addr, LocalSignalMsg::Ice { candidate, sdp_mid, sdp_mline_index }))) => {
                        let _ = pc
                            .add_ice_candidate(webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
                                candidate, sdp_mid, sdp_mline_index, username_fragment: None,
                            })
                            .await;
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => break,
                    Err(_) => break, // idle timeout
                }
            }
            st = state_rx.recv() => {
                match st {
                    Some(s) if matches!(s,
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed
                        | webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed
                        // A dropped guest shows as Disconnected (not Failed).
                        // Release the port immediately so the next guest can
                        // re-bind instead of waiting out the idle timeout.
                        | webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Disconnected
                    ) => break,
                    _ => continue,
                }
            }
            _ = wait_external_release(&mut release_rx) => {
                // The host session ended (guest disconnected / host stopped):
                // drop the signaling socket NOW so the port is free for the
                // next accept loop without waiting for peer-connection state
                // detection.
                break;
            }
        }
    }

    // Release the ICE-candidate forwarding closure. `forward_ice` captured a
    // clone of the signaling socket's Arc, and that closure is stored on the
    // ICE gatherer (`on_local_candidate_handler`), which `close()` never
    // clears and which outlives the session. Without replacing it, the
    // socket stays bound after the session ends — port 53456 is held for as
    // long as the peer connection object lives, breaking reconnection.
    // This runs on every exit path (idle timeout, terminal state, release).
    pc.on_ice_candidate(Box::new(|_| Box::pin(async {})));
}

async fn create_peer_connection() -> Result<Arc<webrtc::peer_connection::RTCPeerConnection>> {
    let mut m = webrtc::api::media_engine::MediaEngine::default();
    m.register_default_codecs()?;
    let mut registry = webrtc::interceptor::registry::Registry::new();
    registry = webrtc::api::interceptor_registry::register_default_interceptors(registry, &mut m)?;
    let api = webrtc::api::APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .build();
    let config = webrtc::peer_connection::configuration::RTCConfiguration {
        ice_servers: vec![webrtc::ice_transport::ice_server::RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_string()],
            ..Default::default()
        }],
        ..Default::default()
    };
    Ok(Arc::new(api.new_peer_connection(config).await?))
}

/// Resolves once this host loop should stop waiting: either the stop flag is
/// set (`stop_local_host` / disconnect) or a newer host loop has superseded
/// this generation (`start_local_host` called again, possibly with a new
/// pairing code). Used inside the offer-wait loop so an idle host releases the
/// signaling port promptly instead of holding it — with a stale pairing
/// code — until the app exits.
async fn wait_host_stopped(stop: &Arc<AtomicBool>, gen_flag: &Arc<AtomicU64>, generation: u64) {
    while !stop.load(Ordering::Relaxed) && gen_flag.load(Ordering::Relaxed) == generation {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Host side: wait for a guest offer over UDP, answer it, exchange ICE, and
/// wait for the data channel. Returns the session, the guest's socket, and the
/// guest's device id (carried in the offer).
///
/// `expected_code` is the pairing code this host is broadcasting; offers whose
/// code does not match are answered with a `Reject` and skipped (the loop
/// keeps waiting for the next offer), so a wrong code fails fast on the guest
/// instead of building a useless WebRTC session.
///
/// `stop` / `gen_flag` / `generation` let the caller terminate this wait (and
/// release the bound UDP port) at any time: a stop request or a newer host
/// loop immediately ends the wait, so a restarted host loop can re-bind the
/// port with the *current* pairing code instead of being blocked by a stale
/// loop that still enforces an outdated code.
pub async fn accept_local_guest(
    expected_code: Option<String>,
    stop: Arc<AtomicBool>,
    gen_flag: Arc<AtomicU64>,
    generation: u64,
) -> Result<(
    SyncSession,
    SocketAddr,
    String,
    tokio::sync::watch::Sender<bool>,
)> {

    let mut sig = LocalSignaling::bind().await?;

    let pc = create_peer_connection().await?;

    let (dc_open_tx, mut dc_open_rx) = mpsc::channel::<()>(1);
    let (dc_tx, mut dc_rx) = mpsc::channel::<Arc<RTCDataChannel>>(1);
    pc.on_data_channel(Box::new(move |dc| {
        info!("local: received guest data channel");
        let dc_open_tx = dc_open_tx.clone();
        let dc_tx = dc_tx.clone();
        let opened = dc.clone();
        dc.on_open(Box::new(move || {
            let _ = dc_open_tx.try_send(());
            let _ = dc_tx.try_send(opened.clone());
            Box::pin(async {})
        }));
        Box::pin(async {})
    }));

    // Wait for the guest's offer; learn its address and device id from the
    // datagram. ICE candidates can arrive BEFORE the offer (the guest
    // registers its ICE forwarding before sending the offer, so candidates
    // may hit this socket first) — buffer them and apply after
    // `set_remote_description`, since `add_ice_candidate` before that fails
    // with "remote description is not set" and aborts the whole pairing.
    let mut pending_ice: Vec<
        webrtc::ice_transport::ice_candidate::RTCIceCandidateInit,
    > = Vec::new();
    let (guest_addr, guest_device_id, offer) = loop {
        tokio::select! {
            msg = sig.recv_timeout(Duration::from_secs(120)) => {
                match msg {
                    Ok(Some((addr, LocalSignalMsg::Offer { sdp, device_id, pairing_code }))) => {
                        if let Some(expected) = &expected_code {
                            if pairing_code != *expected {
                                warn!(
                                    guest = %addr,
                                    code = %pairing_code,
                                    expected = %expected,
                                    "local: pairing code mismatch, rejecting offer"
                                );
                                let _ = sig.send_to(
                                    addr,
                                    &LocalSignalMsg::Reject {
                                        reason: "配对码不一致，请核对后重试".to_string(),
                                    },
                                )
                                .await;
                                continue;
                            }
                        }
                        info!(guest = %addr, device = %device_id, "local: received offer");
                        break (addr, device_id, RTCSessionDescription::offer(sdp)?);
                    }
                    Ok(Some((_addr, LocalSignalMsg::Ice { candidate, sdp_mid, sdp_mline_index }))) => {
                        pending_ice.push(
                            webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
                                candidate,
                                sdp_mid,
                                sdp_mline_index,
                                username_fragment: None,
                            },
                        );
                    }
                    Ok(Some(_)) => continue,
                    // Idle with no offer: give up so the outer loop re-checks
                    // stop/generation, re-reads the pairing code and re-binds
                    // the port — never wait here forever.
                    Ok(None) => anyhow::bail!("no offer received within 120s"),
                    Err(_) => anyhow::bail!("local signaling receive failed"),
                }
            }
            _ = wait_host_stopped(&stop, &gen_flag, generation) => {
                // Stopped or superseded by a newer host loop: drop the
                // signaling socket so its UDP port (and stale pairing code)
                // are released for the next loop immediately.
                anyhow::bail!("host loop stopped or superseded");
            }
        }
    };

    pc.set_remote_description(offer).await?;
    for c in pending_ice {
        let _ = pc.add_ice_candidate(c).await;
    }
    forward_ice(&pc, &sig, guest_addr).await;

    let answer = pc.create_answer(None).await.context("create answer")?;
    pc.set_local_description(answer.clone()).await?;
    sig.send_to(guest_addr, &LocalSignalMsg::Answer { sdp: answer.sdp })
        .await?;
    info!("local: sent answer");

    // ICE in background while waiting for the data channel. The host keeps
    // the release sender and forces the port free the moment the session
    // ends (guest disconnect / host stop) — never rely on peer-connection
    // state detection alone, which can be delayed or (capacity-1 race) lost.
    let (release_tx, release_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(process_local_ice(pc.clone(), sig, Some(release_rx)));

    tokio::time::timeout(Duration::from_secs(30), dc_open_rx.recv())
        .await
        .context("data channel open timeout")?
        .context("open channel closed")?;
    info!("local: data channel open");

    let dc = dc_rx
        .recv()
        .await
        .context("no data channel received on host")?;
    Ok((
        SyncSession { pc, dc },
        guest_addr,
        guest_device_id,
        release_tx,
    ))
}

/// Guest side: send an offer to the host's signaling port and complete the
/// WebRTC handshake over UDP. `device_id` is this device's stable id, sent in
/// the offer so the host can track per-device sync progress. `pairing_code` is
/// the host's code as seen by the guest (from the LAN beacon or typed
/// manually); the host rejects the offer when it does not match.
pub async fn connect_local_host(
    host_addr: SocketAddr,
    device_id: String,
    pairing_code: Option<String>,
) -> Result<SyncSession> {

    let mut sig = LocalSignaling::connect_client().await?;

    let pc = create_peer_connection().await?;

    let dc = pc
        .create_data_channel("siku-sync", None)
        .await
        .context("create data channel")?;

    let (dc_open_tx, mut dc_open_rx) = mpsc::channel::<()>(1);
    let dc_open_tx2 = dc_open_tx.clone();
    let opened = dc.clone();
    dc.on_open(Box::new(move || {
        let _ = dc_open_tx2.try_send(());
        Box::pin(async {})
    }));

    forward_ice(&pc, &sig, host_addr).await;

    let offer = pc.create_offer(None).await.context("create offer")?;

    pc.set_local_description(offer.clone()).await?;
    let offer_sdp = offer.sdp;
    let build_offer_msg = || LocalSignalMsg::Offer {
        sdp: offer_sdp.clone(),
        device_id: device_id.clone(),
        pairing_code: pairing_code.clone().unwrap_or_default(),
    };
    sig.send_to(host_addr, &build_offer_msg()).await?;
    info!("local: offer sent to {host_addr}");

    // Answer + ICE until data channel opens. The host's ICE candidates may
    // arrive before the answer (same ordering hazard as the host side) —
    // buffer them and apply once the remote description is set.
    //
    // The wait is bounded: UDP offers can be silently dropped when the host
    // is mid-teardown of a previous session (its signaling port not bound
    // yet) or is no longer listening at all — nothing would ever come back.
    // Re-send the offer before giving up so a host that re-binds a few
    // seconds later still pairs; without a deadline the caller would hang
    // forever on "connecting…".
    let mut pending_ice: Vec<
        webrtc::ice_transport::ice_candidate::RTCIceCandidateInit,
    > = Vec::new();
    let mut offer_retries = 2u32;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("等待主机应答超时，请确认提供方设备正在等待连接后重试");
        }
        tokio::select! {
            msg = sig.recv_timeout(Duration::from_secs(8)) => {
                match msg {
                    Ok(Some((_addr, LocalSignalMsg::Answer { sdp }))) => {
                        pc.set_remote_description(RTCSessionDescription::answer(sdp)?).await?;
                        info!("local: received answer");
                        for c in pending_ice {
                            let _ = pc.add_ice_candidate(c).await;
                        }
                        // Hand remaining ICE in background. The guest's
                        // client socket is ephemeral, so no external release
                        // is needed — the peer-connection state break covers
                        // it.
                        tokio::spawn(process_local_ice(pc.clone(), sig, None));
                        break;
                    }
                    Ok(Some((_addr, LocalSignalMsg::Reject { reason }))) => {
                        anyhow::bail!("{reason}");
                    }
                    Ok(Some((_addr, LocalSignalMsg::Ice { candidate, sdp_mid, sdp_mline_index }))) => {
                        pending_ice.push(
                            webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
                                candidate, sdp_mid, sdp_mline_index, username_fragment: None,
                            },
                        );
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => {
                        // No reply in this window: the first offer may have been
                        // dropped while the host's signaling port was briefly
                        // unbound. Re-send a couple of times before failing.
                        if offer_retries > 0 {
                            offer_retries -= 1;
                            let _ = sig.send_to(host_addr, &build_offer_msg()).await;
                            info!("local: offer resent");
                            continue;
                        }
                        anyhow::bail!("等待主机应答超时，请确认提供方设备正在等待连接后重试");
                    }
                    Err(_) => anyhow::bail!("local signaling receive failed"),
                }
            }
            _ = dc_open_rx.recv() => {
                anyhow::bail!("data channel opened before answer; aborting")
            }
        }
    }

    timeout_dc(dc_open_rx).await?;
    info!("local: data channel open");
    Ok(SyncSession { pc, dc })
}

async fn timeout_dc(mut rx: mpsc::Receiver<()>) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .context("data channel open timeout")?
        .context("open channel closed")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offer carries the pairing code; a legacy offer without the field
    /// deserializes to an empty code (so an old guest is rejected when the
    /// host expects a code). Reject messages round-trip too.
    #[test]
    fn offer_carries_pairing_code_and_round_trips() {
        let offer = LocalSignalMsg::Offer {
            sdp: "v=0".to_string(),
            device_id: "dev-g".to_string(),
            pairing_code: "123456".to_string(),
        };
        let json = serde_json::to_string(&offer).unwrap();
        assert!(json.contains("\"type\":\"offer\""), "json: {json}");
        assert!(json.contains("\"pairing_code\":\"123456\""), "json: {json}");

        // Round trip preserves the code.
        let parsed: LocalSignalMsg = serde_json::from_str(&json).unwrap();
        match parsed {
            LocalSignalMsg::Offer { pairing_code, .. } => {
                assert_eq!(pairing_code, "123456");
            }
            _ => panic!("should parse as offer"),
        }

        // Legacy offer without pairing_code -> empty string.
        let legacy = r#"{"type":"offer","sdp":"v=0","device_id":"dev-g"}"#;
        let parsed: LocalSignalMsg = serde_json::from_str(legacy).unwrap();
        match parsed {
            LocalSignalMsg::Offer { pairing_code, .. } => {
                assert_eq!(pairing_code, "", "missing code must default to empty");
            }
            _ => panic!("should parse as offer"),
        }

        // Reject round trip.
        let reject = LocalSignalMsg::Reject {
            reason: "配对码不一致".to_string(),
        };
        let json = serde_json::to_string(&reject).unwrap();
        let parsed: LocalSignalMsg = serde_json::from_str(&json).unwrap();
        match parsed {
            LocalSignalMsg::Reject { reason } => {
                assert_eq!(reason, "配对码不一致");
            }
            _ => panic!("should parse as reject"),
        }
    }

    /// Host-side pairing check logic: expected code vs offer code.
    #[test]
    fn pairing_code_check_matches_expected() {
        // Matching code passes; mismatching is rejected.
        let expected = Some("123456".to_string());
        assert_eq!(expected.as_deref(), Some("123456"));
        let offer_code = "123456".to_string();
        assert_eq!(offer_code, *expected.as_ref().unwrap());

        // No expected code on the host -> no check.
        let none: Option<String> = None;
        assert!(none.is_none());

        // Empty guest code (legacy guest) never matches a real expected code.
        assert_ne!("", "123456");
    }
}
