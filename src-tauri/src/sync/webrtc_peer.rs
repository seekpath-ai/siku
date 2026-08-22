use crate::sync::relay_client::RelayClient;
use crate::sync::types::{JoinPayload, RelayClientMsg, RelayServerMsg, SignalData, SignalPayload};
use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{info, warn};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

pub struct SyncSession {
    #[allow(dead_code)]
    pub pc: Arc<RTCPeerConnection>,
    pub dc: Arc<RTCDataChannel>,
}

/// Join the relay room and wait until `peer_device_id` is reported online
/// (or already present). Errors from the relay are propagated — they used to
/// be swallowed, which made the caller hang forever on a forbidden join.
async fn wait_for_peer_online(
    relay: &RelayClient,
    peer_device_id: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("timed out waiting for peer {peer_device_id} to come online");
        }
        match tokio::time::timeout(remaining, relay.recv()).await {
            Ok(Some(RelayServerMsg::PeerOnline { payload }))
                if payload.device_id == peer_device_id =>
            {
                info!(peer = %peer_device_id, "target peer online");
                return Ok(());
            }
            Ok(Some(RelayServerMsg::Presence { payload })) => {
                if payload.device_ids.iter().any(|id| id == peer_device_id) {
                    info!(peer = %peer_device_id, "target peer already present");
                    return Ok(());
                }
            }
            Ok(Some(RelayServerMsg::Ping)) => continue,
            Ok(Some(RelayServerMsg::Error { payload })) => {
                anyhow::bail!("relay error {}: {}", payload.code, payload.message);
            }
            Ok(Some(other)) => warn!(msg = ?other, "unexpected msg"),
            Ok(None) => anyhow::bail!("relay closed before peer online"),
            Err(_) => continue, // deadline re-checked at loop top
        }
    }
}

/// RAII guard: shuts down the relay WebSocket when dropped without being
/// disarmed. A failed signaling handshake must close its connection instead
/// of leaking it — a leaked connection would keep this device marked online
/// on the relay long after the session attempt failed (and after logout).
struct ShutdownOnDrop(RelayClient);

impl Drop for ShutdownOnDrop {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

pub async fn start_sync_session(
    relay_url: String,
    token: String,
    room_id: String,
    peer_device_id: String,
    guest_device_id: Option<String>,
) -> Result<SyncSession> {
    let relay = RelayClient::connect(&relay_url, &token).await?;
    // On any early `?` return the guard closes the socket; on success it is
    // disarmed below (the relay moves into the background ICE loop).
    let guard = ShutdownOnDrop(relay.clone());
    relay.send(RelayClientMsg::Join {
        payload: JoinPayload {
            room_id,
            device_id: guest_device_id,
        },
    })?;

    // Wait for the target peer to come online (bounded; the caller may decide
    // to fall back to the mailbox when this times out).
    wait_for_peer_online(&relay, &peer_device_id, Duration::from_secs(30)).await?;

    let pc = create_peer_connection().await?;
    setup_ice_forwarding(
        &pc,
        &relay,
        "tauri-device".to_string(),
        peer_device_id.clone(),
    )
    .await?;

    let dc = pc
        .create_data_channel("siku-sync", None)
        .await
        .context("create data channel")?;

    let (dc_open_tx, mut dc_open_rx) = mpsc::channel::<()>(1);
    dc.on_open(Box::new(move || {
        let _ = dc_open_tx.try_send(());
        Box::pin(async {})
    }));

    // Create offer.
    let offer = pc.create_offer(None).await.context("create offer")?;
    pc.set_local_description(offer.clone()).await?;
    relay.send(RelayClientMsg::Signal {
        payload: SignalPayload {
            to_device_id: peer_device_id.clone(),
            data: SignalData::Offer { sdp: offer.sdp },
        },
    })?;
    info!("sent offer");

    // Wait for answer and ICE candidates.
    let answer = loop {
        let sig = relay.expect_signal().await?;
        match sig.data {
            SignalData::Answer { sdp } => {
                info!("received answer");
                break RTCSessionDescription::answer(sdp)?;
            }
            SignalData::Ice {
                candidate,
                sdp_mid,
                sdp_mline_index,
            } => {
                pc.add_ice_candidate(webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
                    candidate,
                    sdp_mid,
                    sdp_mline_index,
                    username_fragment: None,
                })
                .await?;
            }
            other => warn!(data = ?other, "unexpected signal data"),
        }
    };
    pc.set_remote_description(answer).await?;

    // Process remaining ICE candidates in background.
    tokio::spawn(process_incoming_ice(pc.clone(), relay));

    // Wait for data channel open.
    timeout(Duration::from_secs(30), dc_open_rx.recv())
        .await
        .context("data channel open timeout")?
        .context("open channel closed")?;
    info!("data channel open");

    // Success: the relay now lives in the background ICE task; disarm the
    // shutdown guard so the socket stays open for the session's lifetime.
    std::mem::forget(guard);
    Ok(SyncSession { pc, dc })
}

/// Accept an already-received offer (account auto-sync): join the relay room
/// under our own identity, answer the given offer, exchange ICE, and wait for
/// the data channel. The caller consumed the offer from the discovery
/// connection already.
pub async fn accept_offer(
    relay_url: String,
    token: String,
    room_id: String,
    peer_device_id: String,
    offer_sdp: String,
) -> Result<SyncSession> {
    let relay = RelayClient::connect(&relay_url, &token).await?;
    let guard = ShutdownOnDrop(relay.clone());
    relay.send(RelayClientMsg::Join {
        payload: JoinPayload {
            room_id,
            device_id: None, // our own token identity
        },
    })?;

    let pc = create_peer_connection().await?;
    let (dc_open_tx, mut dc_open_rx) = mpsc::channel::<()>(1);
    let (dc_tx, mut dc_rx) = mpsc::channel::<Arc<RTCDataChannel>>(1);
    pc.on_data_channel(Box::new(move |dc| {
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

    pc.set_remote_description(RTCSessionDescription::offer(offer_sdp)?)
        .await?;
    setup_ice_forwarding(&pc, &relay, "tauri-device".to_string(), peer_device_id.clone())
        .await?;

    let answer = pc.create_answer(None).await.context("create answer")?;
    pc.set_local_description(answer.clone()).await?;
    relay.send(RelayClientMsg::Signal {
        payload: SignalPayload {
            to_device_id: peer_device_id,
            data: SignalData::Answer { sdp: answer.sdp },
        },
    })?;
    info!("sent answer (auto-sync)");

    tokio::spawn(process_incoming_ice(pc.clone(), relay));

    timeout(Duration::from_secs(30), dc_open_rx.recv())
        .await
        .context("data channel open timeout")?
        .context("open channel closed")?;
    info!("data channel open (auto-sync)");

    let dc = dc_rx
        .recv()
        .await
        .context("no data channel received on host peer connection")?;

    // Success: disarm the shutdown guard (relay now lives in the ICE task).
    std::mem::forget(guard);
    Ok(SyncSession { pc, dc })
}

async fn create_peer_connection() -> Result<Arc<RTCPeerConnection>> {
    let mut m = MediaEngine::default();
    m.register_default_codecs()?;

    let mut registry = webrtc::interceptor::registry::Registry::new();
    registry = register_default_interceptors(registry, &mut m)?;

    let setting_engine = SettingEngine::default();
    let api = APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .with_setting_engine(setting_engine)
        .build();

    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_string()],
            ..Default::default()
        }],
        ..Default::default()
    };

    Ok(Arc::new(api.new_peer_connection(config).await?))
}

async fn setup_ice_forwarding(
    pc: &Arc<RTCPeerConnection>,
    relay: &RelayClient,
    _own_device_id: String,
    peer_device_id: String,
) -> Result<()> {
    let relay_for_ice = relay.clone();
    let peer = peer_device_id.clone();
    pc.on_ice_candidate(Box::new(move |candidate| {
        let relay = relay_for_ice.clone();
        let peer = peer.clone();
        Box::pin(async move {
            if let Some(c) = candidate {
                match c.to_json() {
                    Ok(init) => {
                        let data = SignalData::Ice {
                            candidate: init.candidate,
                            sdp_mid: init.sdp_mid,
                            sdp_mline_index: init.sdp_mline_index,
                        };
                        let _ = relay.send(RelayClientMsg::Signal {
                            payload: SignalPayload {
                                to_device_id: peer.clone(),
                                data,
                            },
                        });
                        info!("sent ICE candidate");
                    }
                    Err(e) => warn!(error = %e, "failed to serialize ICE candidate"),
                }
            }
        })
    }));
    Ok(())
}

async fn process_incoming_ice(pc: Arc<RTCPeerConnection>, relay: RelayClient) {
    // When the peer connection is closed (session teardown), shut the relay
    // WebSocket down too: the signaling connection has no further purpose and
    // would otherwise keep this device marked online on the relay after a
    // logout or session end.
    // Use an UNBOUNDED channel: connection setup fires New → Connecting →
    // Connected, and a capacity-1 channel would swallow the final Closed
    // event (try_send fails when full), leaking the signaling connection.
    let (state_tx, mut state_rx) = mpsc::unbounded_channel::<
        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState,
    >();
    let state_tx2 = state_tx.clone();
    pc.on_peer_connection_state_change(Box::new(move |s| {
        let _ = state_tx2.send(s);
        Box::pin(async {})
    }));

    loop {
        tokio::select! {
            msg = relay.recv() => {
                match msg {
                    Some(RelayServerMsg::Signal { payload }) => {
                        if let SignalData::Ice {
                            candidate,
                            sdp_mid,
                            sdp_mline_index,
                        } = payload.data
                        {
                            let _ = pc
                                .add_ice_candidate(webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
                                    candidate,
                                    sdp_mid,
                                    sdp_mline_index,
                                    username_fragment: None,
                                })
                                .await;
                        }
                        // Offer/Answer arriving here (e.g. a retried pairing while this
                        // connection is still alive) must NOT terminate the loop.
                    }
                    Some(RelayServerMsg::Ping) => continue,
                    Some(RelayServerMsg::PeerOnline { .. })
                    | Some(RelayServerMsg::Presence { .. })
                    | Some(RelayServerMsg::PeerOffline { .. })
                    | Some(RelayServerMsg::MailboxBatch { .. }) => {
                        // Presence / mailbox traffic may arrive on this connection too
                        // (the relay broadcasts to every connection of the device);
                        // ignore instead of tearing down the ICE loop.
                        continue;
                    }
                    Some(RelayServerMsg::Error { payload }) => {
                        warn!(code = %payload.code, message = %payload.message, "relay error in ICE loop");
                    }
                    None => break,
                    Some(_) => continue,
                }
            }
            st = state_rx.recv() => {
                match st {
                    Some(s)
                        if matches!(
                            s,
                            webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed
                            | webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed
                        ) =>
                    {
                        info!("peer connection closed; shutting down signaling relay");
                        relay.shutdown();
                        break;
                    }
                    _ => continue,
                }
            }
        }
    }
}

impl SyncSession {
    pub async fn send_text(&self, text: String) -> Result<()> {
        self.dc.send_text(text).await?;
        Ok(())
    }

    /// Tear down the peer connection (also closes the data channel and fires
    /// `on_close`). Safe to call twice — errors are ignored.
    pub async fn close(&self) {
        let _ = self.dc.close().await;
        let _ = self.pc.close().await;
    }

    pub fn on_message<F>(&self, f: F)
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        self.dc.on_message(Box::new(move |msg: DataChannelMessage| {
            let text = String::from_utf8_lossy(&msg.data).to_string();
            f(text);
            Box::pin(async {})
        }));
    }

    pub fn on_close<F>(&self, f: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.dc.on_close(Box::new(move || {
            f();
            Box::pin(async {})
        }));
    }
}
