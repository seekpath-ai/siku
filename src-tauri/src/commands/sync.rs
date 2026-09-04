use crate::core::models::DeviceAppSettings;
use crate::core::settings_service;
use crate::sync::engine::{flush_outbox_with, load_peer_progress, SyncEngine};
use crate::sync::lan_discovery::{LanBeacon, LanBeaconHandle, LanListenerHandle, LanPeer};
use crate::sync::mailbox_client::MailboxClient;
use crate::sync::onboarding::{
    create_encrypted_seed_archive_from_pool, ensure_device_id, jwt_sub, normalize_ws_url,
    restore_encrypted_seed_archive,
};
use crate::sync::webrtc_peer::{start_sync_session, SyncSession};
use crate::AppState;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex;
use tracing::info;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SyncConfig {
    pub sync_optional_data: bool,
    /// Allow unencrypted (`ws://`) relay connections (LAN debugging only).
    pub allow_plaintext_relay: bool,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            sync_optional_data: true,
            allow_plaintext_relay: false,
        }
    }
}

impl From<&DeviceAppSettings> for SyncConfig {
    fn from(settings: &DeviceAppSettings) -> Self {
        Self {
            sync_optional_data: settings.sync_optional_data,
            allow_plaintext_relay: settings.allow_plaintext_relay,
        }
    }
}

/// True when `relay_url` is an unencrypted `ws://` endpoint that the device is
/// not configured to allow. Used to block plaintext relay connections unless
/// the user explicitly enables them (see `SyncConfig.allow_plaintext_relay`).
fn plaintext_relay_blocked(relay_url: &str, allow_plaintext: bool) -> bool {
    !allow_plaintext && relay_url.starts_with("ws://")
}

pub struct SyncState {
    /// Active sync engines keyed by kind ("lan" / "cloud"). LAN and cloud
    /// sessions coexist instead of overwriting one shared slot, so starting,
    /// stopping or replacing one never disturbs the other.
    pub engines: Arc<Mutex<HashMap<String, Arc<SyncEngine>>>>,
    /// Kind of the most recently established session; used when a command is
    /// invoked without an explicit kind (backward-compatible default).
    pub current_kind: Arc<Mutex<String>>,
    pub lan_beacon: Mutex<Option<LanBeaconHandle>>,
    pub lan_listener: Mutex<Option<Arc<LanListenerHandle>>>,
    /// True while the LAN-local host loop is accepting guests. Used by the UI
    /// to restore the hosting state after page navigation or an app restart.
    pub lan_host_active: Arc<std::sync::atomic::AtomicBool>,
    /// Pairing code the LAN host is currently broadcasting; offers whose code
    /// does not match are rejected (protocol-level pairing check).
    pub lan_pairing_code: Arc<Mutex<Option<String>>>,
    /// Set to stop the host pairing loop (called on disconnect).
    pub host_stop: Arc<std::sync::atomic::AtomicBool>,
    /// Monotonic generation counter: starting a new host loop invalidates any
    /// older loops so only one host session accepts offers at a time.
    pub host_generation: Arc<std::sync::atomic::AtomicU64>,
    /// Set to stop the account auto-sync proxy (called on disconnect).
    pub auto_stop: Arc<std::sync::atomic::AtomicBool>,
    /// Monotonic generation counter: starting a new auto-sync proxy
    /// invalidates any older proxies so repeated spawns (backend login +
    /// frontend startAutoSync) leave exactly one live loop.
    pub auto_generation: Arc<std::sync::atomic::AtomicU64>,
    /// The auto-sync proxy's live discovery WebSocket, kept here so a logout /
    /// disconnect can close it *directly* instead of waiting for the proxy's
    /// own stop-check loop to notice the flag.
    pub discovery_relay: Arc<Mutex<Option<crate::sync::relay_client::RelayClient>>>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            engines: Arc::new(Mutex::new(HashMap::new())),
            current_kind: Arc::new(Mutex::new("unknown".to_string())),
            lan_beacon: Mutex::new(None),
            lan_listener: Mutex::new(None),
            lan_host_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            lan_pairing_code: Arc::new(Mutex::new(None)),
            host_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            host_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            auto_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            auto_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            discovery_relay: Arc::new(Mutex::new(None)),
        }
    }
}

// ── Engine map helpers ──────────────────────────────────────────────────────
// Lock order: `engines` is always locked before `current_kind`.

async fn set_engine(sync_state: &SyncState, kind: &str, engine: Arc<SyncEngine>) {
    sync_state.engines.lock().await.insert(kind.to_string(), engine);
    *sync_state.current_kind.lock().await = kind.to_string();
}

/// Fetch the engine for `kind` (or the most recently established one when
/// `kind` is None). The Arc is cloned so the lock is not held across awaits.
async fn engine_by_kind(sync_state: &SyncState, kind: Option<&str>) -> Option<Arc<SyncEngine>> {
    let guard = sync_state.engines.lock().await;
    match kind {
        Some(k) => guard.get(k).cloned(),
        None => {
            let cur = sync_state.current_kind.lock().await.clone();
            guard.get(&cur).cloned()
        }
    }
}

async fn take_engine(sync_state: &SyncState, kind: &str) -> Option<Arc<SyncEngine>> {
    sync_state.engines.lock().await.remove(kind)
}

async fn stop_all_engines(sync_state: &SyncState) {
    let mut guard = sync_state.engines.lock().await;
    for (_, engine) in guard.drain() {
        engine.stop().await;
    }
}

/// Best-effort local LAN IPv4 of this machine (UDP connect trick, no packets
/// sent). Used by the LAN manual pairing UI so the provider can tell the
/// receiver its IP without scanning.
#[tauri::command]
pub fn get_local_ip() -> Result<String, String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    socket
        .connect("8.8.8.8:80")
        .map_err(|e| e.to_string())?;
    let addr = socket.local_addr().map_err(|e| e.to_string())?;
    Ok(addr.ip().to_string())
}

/// LAN-local host: bind the UDP signaling port and continuously accept local
/// guest connections (no relay, no account). `pairing_code` is the code the
/// UI is broadcasting; guest offers carrying a different code are rejected.
#[tauri::command]
pub async fn start_local_host(
    sync_state: State<'_, SyncState>,
    app_state: State<'_, AppState>,
    pairing_code: String,
) -> Result<String, String> {
    sync_state.host_stop.store(false, std::sync::atomic::Ordering::Relaxed);
    sync_state.lan_host_active.store(true, std::sync::atomic::Ordering::Relaxed);
    *sync_state.lan_pairing_code.lock().await = Some(pairing_code);
    let generation = sync_state
        .host_generation
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1;
    let stop = sync_state.host_stop.clone();
    let gen_flag = sync_state.host_generation.clone();
    let lan_host_active = sync_state.lan_host_active.clone();
    let lan_pairing_code = sync_state.lan_pairing_code.clone();
    let engines = sync_state.engines.clone();
    let current_kind = sync_state.current_kind.clone();
    let db = app_state.db.clone();
    let app_data_dir = app_state.app_data_dir.clone();

    tokio::spawn(async move {
        loop {
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                info!("local host loop stopped");
                break;
            }
            if gen_flag.load(std::sync::atomic::Ordering::Relaxed) != generation {
                info!("local host loop superseded");
                break;
            }
            let expected_code = lan_pairing_code.lock().await.clone();
            match crate::sync::local_signaling::accept_local_guest(
                expected_code,
                stop.clone(),
                gen_flag.clone(),
                generation,
            )
            .await
            {
                Ok((session, guest_addr, guest_device_id, release_tx)) => {
                    info!(guest = %guest_addr, device = %guest_device_id, "local guest connected");
                    let session_arc = Arc::new(session);
                    // Per-guest sync progress: the guest id from the offer
                    // identifies this peer across reconnections.
                    let (sent, snapshot_sent) =
                        load_peer_progress(&db, &guest_device_id).await;
                    let engine = SyncEngine::new(
                        session_arc.clone(),
                        db.clone(),
                        app_data_dir.clone(),
                        Some(guest_device_id.clone()),
                    )
                    .with_kind("lan")
                    .with_peer_key(&guest_device_id)
                    .with_peer_progress(sent, snapshot_sent);
                    let engine = Arc::new(engine);
                    engine.clone().start();
                    {
                        let mut guard = engines.lock().await;
                        guard.insert("lan".to_string(), engine.clone());
                        *current_kind.lock().await = "lan".to_string();
                    }
                    {
                        let engine = engine.clone();
                        tokio::spawn(async move {
                            if let Err(e) = engine.sync_once().await {
                                tracing::warn!(error = %e, "local host initial sync failed");
                            }
                        });
                    }
                    let (close_tx, mut close_rx) = tokio::sync::mpsc::channel::<()>(1);
                    session_arc.on_close(move || {
                        let _ = close_tx.try_send(());
                    });
                    tokio::select! {
                        _ = close_rx.recv() => {
                            info!("local guest disconnected");
                            // Release the UDP signaling port NOW so the next
                            // guest can re-bind immediately — do not wait for
                            // peer-connection state detection, which can be
                            // delayed or dropped (a lost event used to keep
                            // port 53456 occupied for up to 120s, breaking
                            // reconnection).
                            let _ = release_tx.send(true);
                            // Stop the engine so its periodic tasks and message
                            // handlers wind down, then drop it from the map so
                            // the UI no longer shows "已连接" for a session
                            // that is gone.
                            engine.stop().await;
                            let mut guard = engines.lock().await;
                            if guard.get("lan").map(|e| Arc::ptr_eq(e, &engine)).unwrap_or(false) {
                                guard.remove("lan");
                            }
                        }
                        _ = async {
                            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            }
                            // Host loop stopping while a guest is connected:
                            // release the port too, so a restart (or the next
                            // host loop) can re-bind immediately.
                            let _ = release_tx.send(true);
                        } => break,
                    }
                }
                Err(e) => {
                    if stop.load(std::sync::atomic::Ordering::Relaxed)
                        || gen_flag.load(std::sync::atomic::Ordering::Relaxed) != generation
                    {
                        // Normal shutdown or superseded by a newer host loop —
                        // not a retryable failure. The loop top breaks next.
                        continue;
                    }
                    tracing::warn!(error = %e, "local accept failed; retrying");
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
            }
        }
        // Clear the "hosting" flag only if we are still the live loop — a
        // superseded loop must not flip it while its successor is running.
        if gen_flag.load(std::sync::atomic::Ordering::Relaxed) == generation {
            lan_host_active.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    });

    Ok("local host started".to_string())
}

/// Stop the LAN-local host loop only (keeps account auto-sync running). A
/// live LAN session is torn down too, so "停止等待" fully disconnects — the
/// guest stops showing "已连接" instead of staying glued to a session the
/// host no longer serves.
#[tauri::command]
pub async fn stop_local_host(sync_state: State<'_, SyncState>) -> Result<(), String> {
    sync_state.host_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    sync_state.lan_host_active.store(false, std::sync::atomic::Ordering::Relaxed);
    if let Some(engine) = take_engine(&sync_state, "lan").await {
        engine.stop().await;
    }
    Ok(())
}

/// Whether the LAN-local host loop is currently accepting guests (restored by
/// the UI after page navigation or an app restart).
#[tauri::command]
pub fn get_lan_host_active(sync_state: State<'_, SyncState>) -> Result<bool, String> {
    Ok(sync_state.lan_host_active.load(std::sync::atomic::Ordering::Relaxed))
}

/// LAN-local guest: connect to the host's UDP signaling port and establish a
/// direct WebRTC session (no relay, no account). `host_device_id` is the
/// provider's id when known (from LAN discovery) and is used to persist
/// per-host sync progress. `pairing_code` is the host's code (from the LAN
/// beacon or typed manually) — the host rejects the offer when it mismatches.
#[tauri::command]
pub async fn connect_local_host(
    sync_state: State<'_, SyncState>,
    app_state: State<'_, AppState>,
    host_ip: String,
    host_device_id: Option<String>,
    pairing_code: Option<String>,
) -> Result<String, String> {
    let addr: std::net::SocketAddr = format!(
        "{}:{}",
        host_ip.trim(),
        crate::sync::local_signaling::LOCAL_SIGNAL_PORT
    )
    .parse()
    .map_err(|e| format!("invalid host address: {e}"))?;

    let device_id = ensure_device_id(&app_state.db).await.map_err(|e| e.to_string())?;
    let session = crate::sync::local_signaling::connect_local_host(addr, device_id, pairing_code)
        .await
        .map_err(|e| e.to_string())?;

    let peer_key = host_device_id.clone().unwrap_or_else(|| "lan".to_string());
    let (sent, snapshot_sent) = load_peer_progress(&app_state.db, &peer_key).await;
    let engine = SyncEngine::new(
        Arc::new(session),
        app_state.db.clone(),
        app_state.app_data_dir.clone(),
        host_device_id,
    )
    .with_kind("lan")
    .with_peer_key(&peer_key)
    .with_peer_progress(sent, snapshot_sent);
    let engine = Arc::new(engine);
    engine.clone().start();
    set_engine(&sync_state, "lan", engine.clone()).await;
    tokio::spawn(async move {
        if let Err(e) = engine.sync_once().await {
            tracing::warn!(error = %e, "local guest initial sync failed");
        }
    });
    Ok("local connection established".to_string())
}

/// Wrap a WebRTC session in a SyncEngine, register it, and kick off the
/// initial sync. Shared by auto-sync and pairing paths. Attaches the mailbox
/// transport so offline changes still reach the peer when it comes online.
async fn spawn_engine_session(
    session: SyncSession,
    peer_device_id: Option<String>,
    db: sqlx::SqlitePool,
    app_data_dir: std::path::PathBuf,
    engine_slot: &Arc<Mutex<HashMap<String, Arc<SyncEngine>>>>,
    current_kind: &Arc<Mutex<String>>,
    relay_url: &str,
    token: &str,
    room_id: &str,
    stop: &Arc<std::sync::atomic::AtomicBool>,
) {
    let peer_key = peer_device_id.clone().unwrap_or_else(|| "cloud".to_string());
    let (sent, snapshot_sent) = load_peer_progress(&db, &peer_key).await;
    let mut engine = SyncEngine::new(Arc::new(session), db.clone(), app_data_dir, peer_device_id)
        .with_kind("cloud")
        .with_peer_key(&peer_key)
        .with_peer_progress(sent, snapshot_sent);
    engine = attach_mailbox(engine, &db, relay_url, token, room_id).await;
    let engine = Arc::new(engine);
    engine.clone().start();
    {
        let mut guard = engine_slot.lock().await;
        guard.insert("cloud".to_string(), engine.clone());
        *current_kind.lock().await = "cloud".to_string();
    }
    // A logout/disconnect may have landed while this session was being set up
    // (mailbox connect is network I/O). Stop it immediately so it is not
    // orphaned — stop_all_engines already ran and would never see it, leaving
    // its relay connections open and the device marked online forever.
    if stop.load(std::sync::atomic::Ordering::Relaxed) {
        tracing::info!("session established after stop; shutting down engine");
        engine.stop().await;
        let mut guard = engine_slot.lock().await;
        if guard.get("cloud").map(|e| Arc::ptr_eq(e, &engine)).unwrap_or(false) {
            guard.remove("cloud");
        }
        return;
    }
    tokio::spawn(async move {
        if let Err(e) = engine.sync_once().await {
            tracing::warn!(error = %e, "initial sync failed");
        }
    });
}

/// Account auto-sync: runs in the background after login. Joins the account
/// room, watches for other devices, and automatically establishes sync
/// sessions — no manual "connect / allow" interaction needed. Role rule: the
/// device with the lexicographically smaller id sends the offer; both sides
/// independently reach the same decision.
#[tauri::command]
pub async fn start_auto_sync(
    sync_state: State<'_, SyncState>,
    app_state: State<'_, AppState>,
    relay_url: String,
    token: String,
) -> Result<(), String> {
    spawn_auto_sync_proxy(&sync_state, &app_state, &relay_url, &token).await;
    Ok(())
}

/// Background auto-sync proxy (also invoked directly from login so it always
/// starts, regardless of frontend timing).
pub async fn spawn_auto_sync_proxy(
    sync_state: &SyncState,
    app_state: &AppState,
    relay_url: &str,
    token: &str,
) {
    let relay_url = normalize_ws_url(relay_url);
    // Security: refuse plaintext (`ws://`) relays unless the user explicitly
    // allowed them. Tokens and (until true E2E lands) sync keys would cross
    // the network unencrypted; a public relay must be served as `wss://`.
    let device_settings = settings_service::load_device_settings(&app_state.db)
        .await
        .unwrap_or_default();
    if plaintext_relay_blocked(&relay_url, device_settings.allow_plaintext_relay) {
        tracing::warn!(
            relay_url = %relay_url,
            "auto-sync: refusing unencrypted relay (enable \"允许明文中继\" in 同步设置 to override)"
        );
        return;
    }
    let token = token.to_string();
    let room_id = match jwt_sub(&token) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "auto-sync: cannot derive room id");
            return;
        }
    };
    let self_id = match ensure_device_id(&app_state.db).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "auto-sync: no device id");
            return;
        }
    };
    let db = app_state.db.clone();
    let app_data_dir = app_state.app_data_dir.clone();
    let engine_slot = sync_state.engines.clone();
    let current_kind = sync_state.current_kind.clone();
    let discovery_slot = sync_state.discovery_relay.clone();

    // Account sync key for encrypted offline (mailbox) delivery. Without it
    // (not logged in / no key yet) the proxy still does discovery + P2P, just
    // no mailbox fallback.
    let mailbox_key = crate::sync::mailbox_client::load_sync_key(&app_state.db).await;
    if mailbox_key.is_none() {
        tracing::info!("auto-sync: no sync key; mailbox (offline) delivery disabled");
    }

    sync_state.auto_stop.store(false, std::sync::atomic::Ordering::Relaxed);
    let stop = sync_state.auto_stop.clone();
    let generation = sync_state
        .auto_generation
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1;
    let gen_flag = sync_state.auto_generation.clone();
    let tried_peers = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::HashSet::<String>::new(),
    ));
    info!(device_id = %self_id, room = %room_id, "auto-sync proxy starting");

    tokio::spawn(async move {
        loop {
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                info!("auto-sync proxy stopped");
                break;
            }
            if gen_flag.load(std::sync::atomic::Ordering::Relaxed) != generation {
                info!("auto-sync proxy superseded by a newer one");
                break;
            }
            let relay = match crate::sync::relay_client::RelayClient::connect(&relay_url, &token).await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "auto-sync relay connect failed; retrying");
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    continue;
                }
            };
            if relay
                .send(crate::sync::types::RelayClientMsg::Join {
                    payload: crate::sync::types::JoinPayload {
                        room_id: room_id.clone(),
                        device_id: Some(self_id.clone()),
                    },
                })
                .is_err()
            {
                // Send failed: the socket is already unusable — close it
                // explicitly instead of leaking a half-open connection that
                // keeps this device marked online on the relay.
                relay.shutdown();
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
            info!("auto-sync discovery connected");
            // 进程级 relay 连接状态：前端据此显示「已连接，同步中…/已同步」，
            // 与是否有 P2P engine 会话无关。
            crate::sync::engine::set_relay_connected(true);

            // Register this connection so a logout/disconnect can shut it down
            // directly (a stop-check race in this loop must never leave the
            // device looking online).
            *discovery_slot.lock().await = Some(relay.clone());

            // Offline delivery cadence: periodically deposit local changes
            // into the ACCOUNT-LEVEL archive (empty to_device_id). Every
            // device of the account — already registered, currently offline,
            // or a future device that does not exist yet — can pull them on
            // connect. This is what makes "edit on A while B is offline, then
            // B logs in and gets it" work even when B was never registered.
            let mut mailbox_tick =
                tokio::time::interval(std::time::Duration::from_secs(10));
            mailbox_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            // Also drop a FULL SNAPSHOT into the archive on connect and then
            // every ~2h. The delta path (crsql_changes) can still lose rows
            // when the relay archive prunes old changesets (7-day TTL /
            // 2000-message cap) after this device's cursor already advanced
            // past them — a device that comes online later would never see
            // them, e.g. an old "我的图书馆" folder row or its notes. The
            // snapshot is INSERT OR IGNORE, so it is idempotent and simply
            // fills whatever the peer is missing.
            if let Some(key) = &mailbox_key {
                if let Err(e) =
                    crate::sync::engine::deliver_full_snapshot_mailbox(&db, &relay, key).await
                {
                    tracing::warn!(error = %e, "auto-sync: initial mailbox snapshot failed");
                }
            }
            let mut snapshot_ticks: u64 = 0;

            loop {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    info!("auto-sync proxy stopped");
                    crate::sync::engine::set_relay_connected(false);
                    relay.shutdown();
                    if discovery_slot
                        .lock()
                        .await
                        .as_ref()
                        .map(|r| r.is_same_connection(&relay))
                        .unwrap_or(false)
                    {
                        *discovery_slot.lock().await = None;
                    }
                    break;
                }
                if gen_flag.load(std::sync::atomic::Ordering::Relaxed) != generation {
                    info!("auto-sync proxy superseded; dropping connection");
                    crate::sync::engine::set_relay_connected(false);
                    relay.shutdown();
                    if discovery_slot
                        .lock()
                        .await
                        .as_ref()
                        .map(|r| r.is_same_connection(&relay))
                        .unwrap_or(false)
                    {
                        *discovery_slot.lock().await = None;
                    }
                    break;
                }
                // `relay.recv()` blocks indefinitely while the socket is
                // alive, so the stop check must race it: without this, a
                // logout would set the stop flag but the proxy would never
                // notice, leaving the discovery WebSocket open and the device
                // permanently "online" on other devices.
                tokio::select! {
                    msg = relay.recv() => {
                        match msg {
                            Some(crate::sync::types::RelayServerMsg::PeerOnline { payload })
                                if payload.device_id != self_id =>
                            {
                                let peer = payload.device_id;
                                info!(peer = %peer, "auto-sync: peer online");
                                maybe_offer_to_peer(
                                    &peer,
                                    &relay_url,
                                    &token,
                                    &room_id,
                                    &self_id,
                                    &db,
                                    &app_data_dir,
                                    &engine_slot,
                                    &current_kind,
                                    &tried_peers,
                                    &stop,
                                );
                            }
                            Some(crate::sync::types::RelayServerMsg::Presence { payload }) => {
                                // Join-time member list. PeerOnline only fires for
                                // devices that join after us; Presence covers peers
                                // already in the room, so a session can start even if
                                // we missed their broadcast.
                                for peer in payload.device_ids {
                                    if peer != self_id {
                                        info!(peer = %peer, "auto-sync: peer present");
                                        maybe_offer_to_peer(
                                            &peer,
                                            &relay_url,
                                            &token,
                                            &room_id,
                                            &self_id,
                                            &db,
                                            &app_data_dir,
                                            &engine_slot,
                                            &current_kind,
                                            &tried_peers,
                                            &stop,
                                        );
                                    }
                                }
                            }
                            Some(crate::sync::types::RelayServerMsg::Signal { payload }) => {
                                if let crate::sync::types::SignalData::Offer { sdp } = payload.data {
                                    if payload.from_device_id != self_id {
                                        let peer = payload.from_device_id;
                                        info!(peer = %peer, "auto-sync: received offer");
                                        let url = relay_url.clone();
                                        let tok = token.to_string();
                                        let room = room_id.clone();
                                        let db = db.clone();
                                        let dir = app_data_dir.clone();
                                        let slot = engine_slot.clone();
                                        let ck = current_kind.clone();
                                        let stop = stop.clone();
                                        tokio::spawn(async move {
                                            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                                                return;
                                            }
                                            match crate::sync::webrtc_peer::accept_offer(
                                                url.clone(), tok.clone(), room.clone(), peer.clone(), sdp,
                                            )
                                            .await
                                            {
                                                Ok(session) => {
                                                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                                                        return;
                                                    }
                                                    spawn_engine_session(
                                                        session, Some(peer), db, dir, &slot, &ck, &url, &tok, &room, &stop,
                                                    )
                                                    .await;
                                                }
                                                Err(e) => {
                                                    tracing::warn!(error = %e, "auto-sync answer failed");
                                                }
                                            }
                                        });
                                    }
                                }
                            }
                            Some(crate::sync::types::RelayServerMsg::PeerOffline { payload }) => {
                                if payload.device_id != self_id {
                                    info!(peer = %payload.device_id, "auto-sync: peer offline");
                                    // Forget the dedup entry so a later re-online can
                                    // re-establish the session; the previous one is
                                    // gone and must not block a fresh offer.
                                    tried_peers.lock().unwrap().remove(&payload.device_id);
                                }
                            }
                            Some(crate::sync::types::RelayServerMsg::Ping) => continue,
                            Some(crate::sync::types::RelayServerMsg::Error { payload }) => {
                                tracing::warn!(code = %payload.code, message = %payload.message, "auto-sync relay error");
                            }
                            Some(crate::sync::types::RelayServerMsg::MailboxBatch { payload }) => {
                                // A peer deposited changes while we were
                                // offline (per-device or account archive); the
                                // relay delivers them on join. Apply them even
                                // without a P2P session.
                                let batch_is_empty = payload.messages.is_empty();
                                if let Some(key) = &mailbox_key {
                                    let batch_full = payload.messages.len() >= 100;
                                    if let Err(e) = crate::sync::engine::handle_mailbox_batch(
                                        &db, &app_data_dir, key, &relay, payload.messages,
                                    )
                                    .await
                                    {
                                        tracing::warn!(error = %e, "auto-sync: mailbox batch failed");
                                    }
                                    // A large account-level archive is paged:
                                    // poll again while the last batch was full —
                                    // account messages may still be queued even
                                    // when the page was filled by per-device
                                    // messages (poll drains the device queue
                                    // first and would otherwise hide them).
                                    if batch_full {
                                        relay
                                            .send(crate::sync::types::RelayClientMsg::MailboxPoll {
                                                payload: crate::sync::types::MailboxPollPayload {
                                                    max_count: Some(100),
                                                },
                                            })
                                            .ok();
                                    }
                                    // batch 应用完成（空 batch 亦然——它正是 relay
                                    // 「没有新消息」的明确信号）：本机已获取云端当前
                                    // 可见的全部变更，更新进程级最近同步时间。
                                    crate::sync::engine::note_synced_now();
                                } else if batch_is_empty {
                                    // 无 sync key 时无法解密应用非空 batch；但空 batch
                                    // 本身即「与云端一致」，与是否能解密无关。
                                    crate::sync::engine::note_synced_now();
                                }
                            }
                            Some(_) => continue,
                            None => {
                                info!("auto-sync discovery relay closed; reconnecting");
                                crate::sync::engine::set_relay_connected(false);
                                relay.shutdown();
                                if discovery_slot
                                    .lock()
                                    .await
                                    .as_ref()
                                    .map(|r| r.is_same_connection(&relay))
                                    .unwrap_or(false)
                                {
                                    *discovery_slot.lock().await = None;
                                }
                                break;
                            }
                        }
                    }
                    _ = watch_stop_flag(&stop) => {
                        info!("auto-sync proxy stop requested; closing discovery connection");
                        crate::sync::engine::set_relay_connected(false);
                        relay.shutdown();
                        if discovery_slot
                            .lock()
                            .await
                            .as_ref()
                            .map(|r| r.is_same_connection(&relay))
                            .unwrap_or(false)
                        {
                            *discovery_slot.lock().await = None;
                        }
                        break;
                    }
                    _ = mailbox_tick.tick() => {
                        // Periodic account-level offline delivery: deposit
                        // local changes into the account archive (empty
                        // to_device_id). The cursor is shared with P2P, so a
                        // live session makes this a no-op; when the peer is
                        // offline (or does not exist yet) the relay holds the
                        // archive until some device pulls it.
                        if let Some(key) = &mailbox_key {
                            if let Err(e) = crate::sync::engine::deliver_changes_mailbox(
                                &db, &relay, key, "", "account",
                            )
                            .await
                            {
                                tracing::warn!(error = %e, "auto-sync: account-level mailbox deposit failed");
                            }
                            // Drain the local outbox on every tick: rows queued
                            // while the relay was unreachable are otherwise
                            // only retried by the P2P engine loop (which pure
                            // mailbox users never run) or a manual command.
                            // Failures are retried on the next tick.
                            if let Err(e) = flush_outbox_with(&db, &relay).await {
                                tracing::warn!(error = %e, "auto-sync: outbox flush failed");
                            }
                        }
                        // Periodically drain the mailbox (account archive +
                        // this device's per-device queue). The relay only
                        // pushes a MailboxBatch on a device's FIRST join; after
                        // a reconnect — or when the relay still holds a stale
                        // connection for this device (heartbeat timeout window)
                        // — no batch is delivered, so changes deposited while
                        // this device was offline would otherwise sit in the
                        // archive until the next clean disconnect/join cycle
                        // (or indefinitely if the connection stays up). This
                        // poll makes the mailbox a real bidirectional offline
                        // channel. The relay responses are handled above in the
                        // `MailboxBatch` arm of this select.
                        relay
                            .send(crate::sync::types::RelayClientMsg::MailboxPoll {
                                payload: crate::sync::types::MailboxPollPayload {
                                    max_count: Some(100),
                                },
                            })
                            .ok();
                        // Refresh the account archive's full snapshot so a
                        // device that connects days later still finds one
                        // before the 7-day TTL prunes the connect-time copy.
                        snapshot_ticks += 1;
                        if snapshot_ticks % 720 == 0 {
                            if let Some(key) = &mailbox_key {
                                if let Err(e) =
                                    crate::sync::engine::deliver_full_snapshot_mailbox(&db, &relay, key).await
                                {
                                    tracing::warn!(error = %e, "auto-sync: periodic mailbox snapshot failed");
                                }
                            }
                        }
                    }
                }
            }
        }
    });
}

/// Poll a stop flag with short sleeps; used inside `tokio::select!` so a
/// blocking `relay.recv()` cannot indefinitely delay a logout/disconnect.
async fn watch_stop_flag(stop: &std::sync::Arc<std::sync::atomic::AtomicBool>) {
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// If we are the offerer for `peer` (lexicographically smaller device id)
/// and have not already tried it, spawn a WebRTC session attempt. Dedup per
/// proxy lifetime so Presence + PeerOnline re-broadcasts cannot stack offers.
#[allow(clippy::too_many_arguments)]
fn maybe_offer_to_peer(
    peer: &str,
    relay_url: &str,
    token: &str,
    room_id: &str,
    self_id: &str,
    db: &sqlx::SqlitePool,
    app_data_dir: &std::path::PathBuf,
    engine_slot: &Arc<Mutex<HashMap<String, Arc<SyncEngine>>>>,
    current_kind: &Arc<Mutex<String>>,
    tried_peers: &std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    stop: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    if self_id >= peer {
        // The peer is the offerer; keep listening for its offer.
        return;
    }
    {
        let mut tried = tried_peers.lock().unwrap();
        if !tried.insert(peer.to_string()) {
            return;
        }
    }
    let url = relay_url.to_string();
    let tok = token.to_string();
    let room = room_id.to_string();
    let me = self_id.to_string();
    let peer_id = peer.to_string();
    let db = db.clone();
    let dir = app_data_dir.clone();
    let slot = engine_slot.clone();
    let ck = current_kind.clone();
    let stop = stop.clone();
    tokio::spawn(async move {
        // A logout/disconnect may have happened while we waited; do not
        // resurrect a session for it.
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        match start_sync_session(
            url.clone(),
            tok.clone(),
            room.clone(),
            peer_id.clone(),
            Some(me),
        )
        .await
        {
            Ok(session) => {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                spawn_engine_session(
                    session,
                    Some(peer_id),
                    db,
                    dir,
                    &slot,
                    &ck,
                    &url,
                    &tok,
                    &room,
                    &stop,
                )
                .await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "auto-sync offer failed");
            }
        }
    });
}

/// Attach the encrypted mailbox transport (and account sync key) to an engine
/// when account credentials are available. Best-effort: failures only log.
async fn attach_mailbox(
    engine: SyncEngine,
    db: &sqlx::SqlitePool,
    relay_url: &str,
    token: &str,
    room_id: &str,
) -> SyncEngine {
    let Some(key) = crate::sync::mailbox_client::load_sync_key(db).await else {
        info!("no sync key yet; mailbox transport disabled");
        return engine;
    };
    // Refuse plaintext relays for the mailbox transport (P2P itself is DTLS
    // and unaffected, but mailbox payloads would leave the device via ws://).
    let device_settings = settings_service::load_device_settings(db)
        .await
        .unwrap_or_default();
    if plaintext_relay_blocked(relay_url, device_settings.allow_plaintext_relay) {
        tracing::warn!(
            relay_url = %relay_url,
            "mailbox transport disabled: unencrypted relay not allowed"
        );
        return engine;
    }
    let device_id = match ensure_device_id(db).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "no device id; mailbox transport disabled");
            return engine;
        }
    };
    match MailboxClient::connect(relay_url, token, room_id, &device_id).await {
        Ok(mailbox) => engine.with_mailbox(mailbox).with_sync_key(key),
        Err(e) => {
            tracing::warn!(error = %e, "mailbox connect failed; P2P only");
            engine
        }
    }
}

/// Retry delivering queued outbox messages. Prefers the current engine's
/// mailbox transport; without a live engine a temporary mailbox is built from
/// the stored account settings so rows are not stranded.
#[tauri::command]
pub async fn flush_sync_outbox(
    state: State<'_, SyncState>,
    app_state: State<'_, AppState>,
) -> Result<String, String> {
    if let Some(engine) = engine_by_kind(&state, None).await {
        engine.flush_outbox().await.map_err(|e| e.to_string())?;
        return Ok("outbox flushed".to_string());
    }
    let client = build_mailbox_from_account(&app_state).await?;
    let result = flush_outbox_with(&app_state.db, client.relay()).await;
    // Close the one-shot mailbox connection (see mailbox_deliver_offline).
    client.shutdown();
    result.map_err(|e| e.to_string())?;
    Ok("outbox flushed".to_string())
}

/// Number of messages waiting in the local outbox.
#[tauri::command]
pub async fn get_sync_outbox_count(app_state: State<'_, AppState>) -> Result<i64, String> {
    sqlx::query_scalar("SELECT count(*) FROM sync_outbox")
        .fetch_one(&app_state.db)
        .await
        .map_err(|e| format!("db error: {e}"))
}

/// Trigger a one-shot sync for `kind` ("lan" / "cloud" / None = most recent
/// session). Tries the live DataChannel first; when the peer is offline
/// (channel dead or no session at all) it automatically falls back to the
/// encrypted mailbox so the changes are delivered once the peer comes
/// online — the user never needs to pick a transport based on peer presence.
#[tauri::command]
pub async fn sync_once(
    state: State<'_, SyncState>,
    app_state: State<'_, AppState>,
    kind: Option<String>,
) -> Result<String, String> {
    // 1) Active session: try P2P, then that session's mailbox.
    let peer = {
        let engine = engine_by_kind(&state, kind.as_deref()).await;
        match engine {
            Some(engine) => match engine.sync_once().await {
                Ok(()) => return Ok("sync triggered".to_string()),
                Err(p2p_err) => match engine.status().await.peer_device_id {
                    Some(p) => Some((p, p2p_err.to_string())),
                    None => return Err(p2p_err.to_string()),
                },
            },
            None => None,
        }
    };
    // 2) Peer offline / no session: deposit into the ACCOUNT-LEVEL archive —
    // shared by every device of the account, so it reaches the peer whenever
    // it next connects (even a device that does not exist yet).
    let p2p_err = match peer {
        Some((_, e)) => e,
        None => "no active sync session".to_string(),
    };
    match mailbox_deliver_offline(&app_state).await {
        Ok(()) => {
            if let Some(engine) = engine_by_kind(&state, kind.as_deref()).await {
                engine.set_last_error(None).await;
            }
            Ok("直连暂不可用，已转账号邮箱存档，对方上线后自动同步".to_string())
        }
        Err(mb_err) => Err(format!("同步失败（P2P: {p2p_err}；邮箱: {mb_err}）")),
    }
}

/// Build a mailbox client from the stored account settings (device-local).
async fn build_mailbox_from_account(app_state: &AppState) -> Result<MailboxClient, String> {
    let db = &app_state.db;
    let relay_url = settings_service::get_device_setting(
        db,
        crate::commands::account::ACCOUNT_RELAY_URL_KEY,
    )
    .await
    .map_err(|e| e.to_string())?
    .filter(|v| !v.is_empty())
    .ok_or("账号未登录或未配置服务器")?;
    let token = settings_service::get_device_setting(db, crate::commands::account::ACCOUNT_TOKEN_KEY)
        .await
        .map_err(|e| e.to_string())?
        .filter(|v| !v.is_empty())
        .ok_or("账号未登录")?;
    let device_id = ensure_device_id(db).await.map_err(|e| e.to_string())?;
    let room_id = jwt_sub(&token).map_err(|e| format!("token 无效: {e}"))?;
    let normalized = normalize_ws_url(&relay_url);
    let device_settings = settings_service::load_device_settings(db)
        .await
        .map_err(|e| e.to_string())?;
    if plaintext_relay_blocked(&normalized, device_settings.allow_plaintext_relay) {
        return Err(
            "中继地址为不加密的 ws://，已拒绝连接。生产环境请使用 wss://；仅局域网调试可在「同步 → 同步范围」开启「允许明文中继」"
                .to_string(),
        );
    }
    MailboxClient::connect(&normalized, &token, &room_id, &device_id)
        .await
        .map_err(|e| format!("邮箱连接失败: {e}"))
}

/// Encrypt and deposit the local changeset into the ACCOUNT-LEVEL archive
/// without an active session (peer offline / no session). The archive is
/// shared by every device of the account (including future ones), so the
/// changes reach the peer whenever it next connects. Resumes from the account
/// archive cursor so the full history is never re-sent; on a dead transport
/// the changeset is queued into the local outbox.
async fn mailbox_deliver_offline(app_state: &AppState) -> Result<(), String> {
    let mailbox = build_mailbox_from_account(app_state).await?;

    let result = mailbox_deliver_offline_inner(&mailbox, app_state).await;
    // This mailbox was built for a one-shot deposit; close its WebSocket so
    // the device does not stay marked online on the relay afterwards.
    mailbox.shutdown();
    result
}

async fn mailbox_deliver_offline_inner(mailbox: &MailboxClient, app_state: &AppState) -> Result<(), String> {
    let db = &app_state.db;
    let key = crate::sync::mailbox_client::load_sync_key(db)
        .await
        .ok_or("无同步密钥（请重新登录账号）")?;
    crate::sync::engine::deliver_changes_mailbox(db, mailbox.relay(), &key, "", "account")
        .await
        .map_err(|e| format!("账号级邮箱投递失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn get_sync_config(state: State<'_, AppState>) -> Result<SyncConfig, String> {
    let device_settings = settings_service::load_device_settings(&state.db).await?;
    Ok(SyncConfig::from(&device_settings))
}

/// Persist the sync-scope toggle and make it effective immediately: enabling
/// registers the optional tables as CRRs (so their changes are tracked and
/// sent), disabling drops the CRR artifacts (so they stop being tracked). The
/// export/apply filters in `crdt.rs` gate on the same setting, so a restart
/// is no longer required. Persisting the plaintext-relay override restarts the
/// auto-sync proxy when it changed, so the new policy applies without an app
/// restart.
#[tauri::command]
pub async fn set_sync_config(
    state: State<'_, AppState>,
    sync_state: State<'_, SyncState>,
    config: SyncConfig,
) -> Result<(), String> {
    let mut device_settings = settings_service::load_device_settings(&state.db).await?;
    let optional_changed = device_settings.sync_optional_data != config.sync_optional_data;
    let plaintext_changed =
        device_settings.allow_plaintext_relay != config.allow_plaintext_relay;
    device_settings.sync_optional_data = config.sync_optional_data;
    device_settings.allow_plaintext_relay = config.allow_plaintext_relay;
    settings_service::save_device_settings(&state.db, &device_settings).await?;

    if optional_changed {
        if config.sync_optional_data {
            crate::core::db::register_crr_tables(&state.db, crate::core::db::OPTIONAL_SYNC_TABLES)
                .await
                .map_err(|e| format!("启用可选同步表失败: {e}"))?;
        } else {
            for table in crate::core::db::OPTIONAL_SYNC_TABLES {
                crate::core::db::drop_crr_objects(&state.db, table)
                    .await
                    .map_err(|e| format!("停用可选同步表 {table} 失败: {e}"))?;
            }
        }
    }

    // The plaintext-relay override is only consulted when a relay connection
    // is established. Restart the auto-sync proxy so the new value takes
    // effect immediately — both directions: a refused proxy retries, and a
    // live ws:// connection is torn down when the user disables the override.
    if plaintext_changed {
        restart_auto_sync_proxy(&sync_state, &state).await;
    }
    Ok(())
}

/// Restart the account auto-sync proxy so connection-level settings (currently
/// `allow_plaintext_relay`) apply without an app restart. Stops the old proxy
/// the same way a disconnect does (stop flag + direct discovery-socket
/// shutdown), then respawns it from the persisted account credentials. A live
/// cloud P2P engine is left alone — it is an established session, not a relay
/// connection attempt. No-op when the device is not logged in.
async fn restart_auto_sync_proxy(sync_state: &SyncState, app_state: &AppState) {
    sync_state
        .auto_stop
        .store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(relay) = sync_state.discovery_relay.lock().await.take() {
        relay.shutdown();
    }
    let db = &app_state.db;
    let relay_url = settings_service::get_device_setting(
        db,
        crate::commands::account::ACCOUNT_RELAY_URL_KEY,
    )
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
    let token = settings_service::get_device_setting(
        db,
        crate::commands::account::ACCOUNT_TOKEN_KEY,
    )
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
    if relay_url.is_empty() || token.is_empty() {
        return;
    }
    // Same refresh-on-stale dance as the startup restore in lib.rs.
    let token = if crate::commands::account::access_token_is_fresh(db).await {
        token
    } else {
        match crate::commands::account::refresh_access_token(db, &relay_url).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "auto-sync restart: token refresh failed");
                return;
            }
        }
    };
    spawn_auto_sync_proxy(sync_state, app_state, &relay_url, &token).await;
    tracing::info!("auto-sync proxy restarted after sync config change");
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ExportSeedInput {
    pub archive_path: String,
    pub password: String,
}

/// Export the current database and all blobs into an AES-256 encrypted zip.
#[tauri::command]
pub async fn export_encrypted_seed(
    state: State<'_, AppState>,
    input: ExportSeedInput,
) -> Result<String, String> {
    let app_data_dir = state.app_data_dir.clone();
    let archive_path = std::path::PathBuf::from(&input.archive_path);
    let password = input.password.clone();
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || {
        // The VACUUM INTO itself is async, but the rest of the archiving is
        // synchronous I/O; run the whole pipeline on a blocking thread.
        let rt = tokio::runtime::Handle::current();
        rt.block_on(create_encrypted_seed_archive_from_pool(
            &app_data_dir,
            &archive_path,
            &password,
            &db,
        ))
    })
    .await
    .map_err(|e| format!("seed export task failed: {e}"))?
    .map_err(|e| e.to_string())?;
    Ok(input.archive_path)
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ImportSeedInput {
    pub archive_path: String,
    pub password: String,
}

/// Restore an encrypted seed archive into a fresh subdirectory under the
/// current app data dir, mark it as pending, and return that directory path.
/// On the next application startup `db::init` will move the pending seed into
/// place and load it.
#[tauri::command]
pub async fn import_encrypted_seed(
    state: State<'_, AppState>,
    input: ImportSeedInput,
) -> Result<String, String> {
    let timestamp = crate::core::time::now_iso().replace([':', '.'], "-");
    let seed_imports_dir = state.app_data_dir.join("seed-imports");
    let target = seed_imports_dir.join(timestamp);

    tokio::task::spawn_blocking({
        let archive_path = input.archive_path.clone();
        let password = input.password.clone();
        let target = target.clone();
        move || restore_encrypted_seed_archive(std::path::Path::new(&archive_path), &target, &password)
    })
    .await
    .map_err(|e| format!("seed import task failed: {e}"))?
    .map_err(|e| e.to_string())?;

    // Sanity-check the extracted database before marking the import pending.
    let imported_db = target.join("siku.db");
    let db_size = tokio::fs::metadata(&imported_db)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    if db_size == 0 {
        return Err("seed archive does not contain a valid siku.db".into());
    }
    info!(
        target = %target.display(),
        db_size,
        "seed archive extracted; marking import pending"
    );

    // Mark this import as the one to apply on next startup.
    let marker = seed_imports_dir.join(".pending-import");
    tokio::fs::write(&marker, target.to_string_lossy().as_bytes())
        .await
        .map_err(|e| format!("write pending import marker: {e}"))?;

    Ok(target.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn get_device_id(state: State<'_, AppState>) -> Result<String, String> {
    ensure_device_id(&state.db).await.map_err(|e| e.to_string())
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct StartLanBeaconInput {
    pub device_id: String,
    pub pairing_payload: String,
}

/// Start broadcasting this device's pairing beacon on the LAN. The pairing
/// code is intentionally NOT included in the broadcast — it is shown only on
/// this device's screen and typed manually by the guest (the host validates
/// offers against the code set via `start_local_host`).
#[tauri::command]
pub async fn start_lan_beacon(
    state: State<'_, SyncState>,
    input: StartLanBeaconInput,
) -> Result<(), String> {
    let beacon = LanBeacon {
        device_id: input.device_id,
        pairing_payload: input.pairing_payload,
    };
    let handle = LanBeaconHandle::start(beacon)
        .await
        .map_err(|e| e.to_string())?;
    let mut guard = state.lan_beacon.lock().await;
    *guard = Some(handle);
    Ok(())
}

/// Stop broadcasting the LAN pairing beacon.
#[tauri::command]
pub async fn stop_lan_beacon(state: State<'_, SyncState>) -> Result<(), String> {
    let mut guard = state.lan_beacon.lock().await;
    *guard = None;
    Ok(())
}

/// Start listening for LAN pairing beacons.
#[tauri::command]
pub async fn start_lan_discovery(state: State<'_, SyncState>) -> Result<(), String> {
    let mut guard = state.lan_listener.lock().await;
    if guard.is_none() {
        let handle = LanListenerHandle::start()
            .await
            .map_err(|e| e.to_string())?;
        *guard = Some(Arc::new(handle));
    }
    Ok(())
}

/// Stop listening for LAN pairing beacons (drops the UDP listener).
#[tauri::command]
pub async fn stop_lan_discovery(state: State<'_, SyncState>) -> Result<(), String> {
    let mut guard = state.lan_listener.lock().await;
    *guard = None;
    Ok(())
}

#[derive(serde::Serialize)]
pub struct LanPeerInfo {
    pub device_id: String,
    pub pairing_payload: String,
    pub addr: String,
}

/// Return currently discovered LAN peers.
#[tauri::command]
pub async fn get_lan_peers(state: State<'_, SyncState>) -> Result<Vec<LanPeerInfo>, String> {
    let guard = state.lan_listener.lock().await;
    match guard.as_ref() {
        Some(listener) => {
            let peers: Vec<LanPeer> = listener.peers().await;
            Ok(peers
                .into_iter()
                .map(|p| LanPeerInfo {
                    device_id: p.device_id,
                    pairing_payload: p.pairing_payload,
                    addr: p.addr.to_string(),
                })
                .collect())
        }
        None => Ok(Vec::new()),
    }
}

/// Get the sync session status for `kind` ("lan" / "cloud" / None = most
/// recent session). LAN and cloud sessions have independent engines, so each
/// tab can request its own status.
#[tauri::command]
pub async fn get_sync_status(
    state: State<'_, SyncState>,
    kind: Option<String>,
) -> Result<crate::sync::engine::SyncStatus, String> {
    let mut status = match engine_by_kind(&state, kind.as_deref()).await {
        Some(engine) => engine.status().await,
        None => crate::sync::engine::SyncStatus::default(),
    };
    // 「云端存储已满」是账号级状态，与是否有活跃会话无关，始终并入。
    status.quota_exceeded = crate::sync::engine::quota_exceeded();
    // relay 连接是进程级状态（auto-sync proxy 的 discovery 连接维护），与
    // engine 会话独立，始终并入：无 P2P 会话时纯邮箱路径也在同步。
    status.relay_connected = crate::sync::engine::relay_connected();
    // 会话没有 last_sync_at（或没有会话）时，用进程级最近同步时间兜底；
    // 有会话时优先会话值。
    if status.last_sync_at.is_none() {
        status.last_sync_at = crate::sync::engine::last_sync_at();
    }
    Ok(status)
}

/// Disconnect the LAN-local sync session only: stop the LAN host loop and the
/// LAN engine. Account auto-sync (cloud) keeps running.
#[tauri::command]
pub async fn stop_local_session(sync_state: State<'_, SyncState>) -> Result<(), String> {
    sync_state.host_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    sync_state.lan_host_active.store(false, std::sync::atomic::Ordering::Relaxed);
    if let Some(engine) = take_engine(&sync_state, "lan").await {
        engine.stop().await;
    }
    Ok(())
}

/// Disconnect the cloud (account) sync session only: stop the auto-sync
/// proxy and the cloud engine. LAN host loop keeps running.
#[tauri::command]
pub async fn stop_cloud_session(sync_state: State<'_, SyncState>) -> Result<(), String> {
    sync_state.auto_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(engine) = take_engine(&sync_state, "cloud").await {
        engine.stop().await;
    }
    Ok(())
}

/// Stop everything (LAN host loop + auto-sync proxy + all engines).
pub async fn stop_all_sync(sync_state: &SyncState) {
    tracing::info!("stop_all_sync: stopping host loop + auto-sync proxy + engines");
    sync_state.host_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    sync_state.auto_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    sync_state.lan_host_active.store(false, std::sync::atomic::Ordering::Relaxed);
    // Close the discovery WebSocket directly — never rely on the proxy loop
    // noticing the stop flag (it may be mid-`recv()`/reconnect).
    if let Some(relay) = sync_state.discovery_relay.lock().await.take() {
        tracing::info!("stop_all_sync: shutting down discovery relay connection");
        relay.shutdown();
    } else {
        tracing::info!("stop_all_sync: no discovery relay connection to close");
    }
    stop_all_engines(sync_state).await;
}
