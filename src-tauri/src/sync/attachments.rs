use crate::file_store;
use anyhow::{Context, Result};
use base64::Engine;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tracing::info;

#[allow(dead_code)]
/// List all blob hashes currently in the blob store.
/// Hashes are the filename without extension, e.g. "abc123" from "abc123.pdf".
pub fn list_blob_hashes(
    app_data_dir: &std::path::Path,
) -> Result<std::collections::HashSet<String>> {
    let mut hashes = std::collections::HashSet::new();
    let dir = file_store::blob_dir(app_data_dir);
    if !dir.exists() {
        return Ok(hashes);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if let Some(hash) = name_str.split_once('.').map(|(h, _)| h.to_string()) {
            hashes.insert(hash);
        }
    }
    Ok(hashes)
}

/// Extract `blobs/<hash>.<ext>` references from a text (note Markdown content).
/// The paths produced by `file_store::write_blob` are `blobs/` + a 64-char hex
/// sha256 + optional `.ext`; anything shorter is not a blob reference.
fn extract_blob_refs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let Some(rel) = bytes[i..].windows(6).position(|w| w == b"blobs/") else {
            break;
        };
        let hash_start = i + rel + 6;
        let hash_len = bytes[hash_start..]
            .iter()
            .take_while(|b| b.is_ascii_hexdigit())
            .count();
        if hash_len < 64 {
            i = hash_start + hash_len;
            continue;
        }
        // Optional `.ext` (1-8 alphanumeric chars).
        let mut end = hash_start + hash_len;
        if bytes.get(end) == Some(&b'.') {
            let ext_len = bytes[end + 1..]
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric())
                .take(8)
                .count();
            if ext_len >= 1 {
                end = end + 1 + ext_len;
            }
        }
        out.push(String::from_utf8_lossy(&bytes[hash_start - 6..end]).to_string());
        i = end;
    }
    out
}

/// Collect blob hashes referenced by synced papers/attachments/files **and
/// note Markdown content** that are not present locally. Notes embed
/// pasted/dropped images as `![...](blobs/<sha256>.png)`; without scanning
/// note content the image files would never be requested from the peer and
/// show as broken images on synced devices. Same for the vault `files` table:
/// its rows sync via CRR, but without scanning `blob_path` the actual
/// PDF/image/text content would never arrive.
pub async fn collect_missing_blob_hashes(
    db: &SqlitePool,
    app_data_dir: &std::path::Path,
) -> Result<Vec<(String, String)>> {
    let rows: Vec<(String,)> = sqlx::query_as::<_, (String,)>(
        "SELECT file_path FROM papers WHERE file_path LIKE 'blobs/%' \
         UNION \
         SELECT file_path FROM attachments WHERE file_path LIKE 'blobs/%' \
         UNION \
         SELECT blob_path FROM files WHERE blob_path LIKE 'blobs/%'",
    )
    .fetch_all(db)
    .await
    .context("collect blob paths")?;

    // Note content references: only fetch notes that mention `blobs/` so the
    // scan stays cheap on large note collections.
    let note_rows: Vec<(String,)> =
        sqlx::query_as::<_, (String,)>("SELECT content FROM notes WHERE content LIKE '%blobs/%'")
            .fetch_all(db)
            .await
            .context("collect note blob references")?;

    let mut missing = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut consider = |rel_path: &str| {
        if let Some((hash, ext)) = file_store::parse_blob_path(rel_path) {
            if seen.insert(hash.clone()) && !file_store::has_blob(app_data_dir, &hash) {
                missing.push((hash, ext));
            }
        }
    };
    for (rel_path,) in rows {
        consider(&rel_path);
    }
    for (content,) in note_rows {
        for rel in extract_blob_refs(&content) {
            consider(&rel);
        }
    }
    Ok(missing)
}

/// Maximum blob size served through the mailbox. A mailbox message is one
/// relay frame holding the entire base64 payload (~1.33x wire size, ~2.7x
/// memory after encryption) and the relay has no per-message size guard, so
/// oversized blobs are only served over a live P2P DataChannel (which chunks
/// them via `MAX_WIRE_MSG`) instead.
pub const MAX_MAILBOX_BLOB_BYTES: u64 = 20 * 1024 * 1024;

/// Whether the blob is small enough to be served through the mailbox.
#[allow(dead_code)]
pub fn blob_fits_mailbox(app_data_dir: &std::path::Path, hash: &str, ext: &str) -> bool {
    std::fs::metadata(file_store::blob_path(app_data_dir, hash, ext))
        .map(|m| m.len() <= MAX_MAILBOX_BLOB_BYTES)
        .unwrap_or(false)
}

// ── Chunked transfer for large blobs ──────────────────────────────────────

/// Raw bytes per chunk: base64'd this is ~4MB, comfortably inside the
/// relay's 8MB batch-frame budget and the 64MB websocket cap.
pub const CHUNK_RAW_BYTES: usize = 3 * 1024 * 1024;
/// Largest blob served through the mailbox at all (as chunks). Bigger files
/// only sync over a live P2P DataChannel.
pub const MAX_CHUNKED_BLOB_BYTES: u64 = 200 * 1024 * 1024;
/// Blobs up to this size are proactively pushed to the account archive on
/// local write (paper import, note attachment, vault file), so peers receive
/// them without a request round-trip.
pub const PROACTIVE_PUSH_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Number of chunks a blob of `size` bytes splits into.
pub fn chunk_count(size: u64) -> u32 {
    size.div_ceil(CHUNK_RAW_BYTES as u64) as u32
}

/// Read one chunk of a blob, base64-encoded. Returns `Ok(None)` when the
/// blob is absent; an out-of-range `index` yields an empty payload.
pub fn read_blob_chunk_base64(
    app_data_dir: &std::path::Path,
    hash: &str,
    ext: &str,
    index: u32,
) -> Result<Option<String>> {
    use std::io::{Read, Seek, SeekFrom};
    let path = file_store::blob_path(app_data_dir, hash, ext);
    if !path.exists() {
        return Ok(None);
    }
    let mut f = std::fs::File::open(&path)?;
    f.seek(SeekFrom::Start(index as u64 * CHUNK_RAW_BYTES as u64))?;
    let mut buf = Vec::new();
    f.take(CHUNK_RAW_BYTES as u64).read_to_end(&mut buf)?;
    Ok(Some(base64::engine::general_purpose::STANDARD.encode(buf)))
}

fn staging_dir(app_data_dir: &std::path::Path, hash: &str) -> std::path::PathBuf {
    file_store::blob_dir(app_data_dir)
        .join(".incoming")
        .join(hash)
}

/// Stage one received chunk as `<index>.part`, recording the chunk count in
/// a `total` file so a later resume request knows how many slices exist.
/// Idempotent: a duplicate chunk simply overwrites itself.
pub fn write_incoming_chunk(
    app_data_dir: &std::path::Path,
    hash: &str,
    index: u32,
    total: u32,
    data: &str,
) -> Result<()> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("decode blob chunk base64")?;
    let dir = staging_dir(app_data_dir, hash);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{index}.part")), bytes)?;
    let total_file = dir.join("total");
    if !total_file.exists() {
        std::fs::write(&total_file, total.to_string())?;
    }
    Ok(())
}

/// The chunk count recorded when the first chunk of `hash` was staged.
pub fn staged_chunk_total(app_data_dir: &std::path::Path, hash: &str) -> Option<u32> {
    std::fs::read_to_string(staging_dir(app_data_dir, hash).join("total"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Chunk indices currently staged for `hash` (empty when nothing staged).
pub fn received_chunk_indices(app_data_dir: &std::path::Path, hash: &str) -> Vec<u32> {
    let dir = staging_dir(app_data_dir, hash);
    let mut out: Vec<u32> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .strip_suffix(".part")
                        .and_then(|s| s.parse::<u32>().ok())
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_unstable();
    out
}

/// Indices still missing for a partially staged blob; `None` when no staging
/// exists (nothing received yet — request the whole blob, not chunks).
pub fn missing_chunk_indices(app_data_dir: &std::path::Path, hash: &str) -> Option<Vec<u32>> {
    let total = staged_chunk_total(app_data_dir, hash)?;
    let have: std::collections::HashSet<u32> =
        received_chunk_indices(app_data_dir, hash).into_iter().collect();
    Some((0..total).filter(|i| !have.contains(i)).collect())
}

/// Assemble a fully-staged blob: concatenate chunks in order, verify the
/// sha256, move into the blob store (existing blob = idempotent no-op), and
/// clear the staging dir. Returns `Ok(true)` when the blob is now present —
/// either just assembled or already in the store. Returns `Ok(false)` while
/// chunks are still missing. A hash mismatch discards the staging dir so the
/// next request starts clean.
pub fn try_assemble_blob(
    app_data_dir: &std::path::Path,
    hash: &str,
    ext: &str,
    total: u32,
) -> Result<bool> {
    use sha2::Digest;
    let dest = file_store::blob_path(app_data_dir, hash, ext);
    if dest.exists() {
        // Content-addressed: already have these bytes. Clean any staging.
        let _ = std::fs::remove_dir_all(staging_dir(app_data_dir, hash));
        return Ok(true);
    }
    let dir = staging_dir(app_data_dir, hash);
    let staged = received_chunk_indices(app_data_dir, hash);
    if staged.len() < total as usize {
        return Ok(false);
    }
    let tmp = file_store::blob_dir(app_data_dir).join(format!(".assemble-{hash}"));
    {
        let mut out = std::fs::File::create(&tmp)?;
        let mut hasher = sha2::Sha256::new();
        for index in 0..total {
            let bytes = std::fs::read(dir.join(format!("{index}.part")))
                .with_context(|| format!("read staged chunk {index} of {hash}"))?;
            hasher.update(&bytes);
            std::io::Write::write_all(&mut out, &bytes)?;
        }
        let computed = format!("{:x}", hasher.finalize());
        if computed != hash {
            drop(out);
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_dir_all(&dir);
            anyhow::bail!("assembled blob hash mismatch: expected {}, got {}", hash, computed);
        }
    }
    std::fs::rename(&tmp, &dest).with_context(|| format!("move assembled blob {}", hash))?;
    let _ = std::fs::remove_dir_all(&dir);
    info!(hash = %hash, ext = %ext, chunks = total, "assembled chunked blob");
    Ok(true)
}

/// `HMAC-SHA256(sync_key, hash)` in hex — the relay-visible dedup key. The
/// relay can dedupe two deposits of the same blob within an account, but
/// cannot correlate the key with any known file (it never sees `sync_key`
/// combined with the hash anywhere else).
pub fn blob_dedup_key(sync_key: &[u8; crate::sync::crypto::SYNC_KEY_LEN], hash: &str) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<sha2::Sha256> as Mac>::new_from_slice(sync_key)
        .expect("HMAC accepts any key length");
    mac.update(hash.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

// ── Proactive push bookkeeping ────────────────────────────────────────────
//
// Local blob writes (paper import, note attachment, vault file) mark the hash
// pending; the auto-sync tick deposits eligible blobs (≤ PROACTIVE_PUSH_MAX)
// into the account archive and marks them done. Cross-device duplicates are
// absorbed by the relay's dedup_key, so no local "already on relay" set is
// needed — a redundant push costs one upload but stores nothing.

const PUSH_PENDING_PREFIX: &str = "sync.blob_push.pending.";
const PUSH_DONE_PREFIX: &str = "sync.blob_push.done.";

/// Mark a locally written blob for proactive push on the next auto-sync tick.
/// No-op for blobs above the proactive size limit (they stay request-served).
pub async fn mark_blob_pending_push(db: &SqlitePool, app_data_dir: &std::path::Path, hash: &str, ext: &str) {
    let size = std::fs::metadata(file_store::blob_path(app_data_dir, hash, ext))
        .map(|m| m.len())
        .unwrap_or(u64::MAX);
    if size > PROACTIVE_PUSH_MAX_BYTES {
        return;
    }
    let done = crate::core::settings_service::get_device_setting(
        db,
        &format!("{PUSH_DONE_PREFIX}{hash}"),
    )
    .await
    .ok()
    .flatten();
    if done.is_some() {
        return;
    }
    let _ = crate::core::settings_service::set_device_setting(
        db,
        &format!("{PUSH_PENDING_PREFIX}{hash}", ),
        &ext,
    )
    .await;
}

/// Re-arm a pending push after a failed attempt: the marker value becomes
/// `ext@retry_after_epoch` so `pending_blob_pushes` skips it until the
/// backoff window has passed.
pub async fn mark_push_retry(db: &SqlitePool, hash: &str, ext: &str, delay_secs: u64) {
    let retry_after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        + delay_secs;
    let _ = crate::core::settings_service::set_device_setting(
        db,
        &format!("{PUSH_PENDING_PREFIX}{hash}"),
        &format!("{ext}@{retry_after}"),
    )
    .await;
}

/// Hashes awaiting proactive push: `(hash, ext)`. A marker value of
/// `ext@retry_after_epoch` (written by `mark_push_retry` after a failed
/// attempt) is held back until that time.
pub async fn pending_blob_pushes(db: &SqlitePool) -> Vec<(String, String)> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM device_settings WHERE key LIKE 'sync.blob_push.pending.%'",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rows.into_iter()
        .filter_map(|(k, v)| {
            let hash = k.strip_prefix(PUSH_PENDING_PREFIX)?.to_string();
            let (ext, retry_after) = match v.split_once('@') {
                Some((e, ts)) => (e.to_string(), ts.parse::<u64>().unwrap_or(0)),
                None => (v, 0),
            };
            if retry_after > now {
                return None;
            }
            Some((hash, ext))
        })
        .collect()
}

/// Mark a blob as pushed (or deduped by the relay — same outcome).
pub async fn mark_blob_pushed(db: &SqlitePool, hash: &str) {
    let _ = crate::core::settings_service::set_device_setting(
        db,
        &format!("{PUSH_DONE_PREFIX}{hash}"),
        "1",
    )
    .await;
    let _ = sqlx::query("DELETE FROM device_settings WHERE key = ?")
        .bind(format!("{PUSH_PENDING_PREFIX}{hash}"))
        .execute(db)
        .await;
}

// ── Request/answer throttling ───────────────────────────────────────────────
//
// Blob sync is request-driven and every applied changeset re-scans for missing
// blobs. Without throttling, a batch of N changesets fires N identical
// full-list requests within a second, and each queued request is answered with
// the FULL blob set — an N×M payload amplification that once clogged a
// device's own outbox (deposit ack timeouts) and flooded the relay mailbox.

/// Per-peer cooldown for outgoing blob requests.
#[cfg_attr(test, allow(dead_code))]
const BLOB_REQUEST_COOLDOWN: Duration = Duration::from_secs(300);
/// Per-hash cooldown for answering blob requests: duplicate queued requests
/// are answered once per hash per window, not once per request message.
#[cfg_attr(test, allow(dead_code))]
const BLOB_ANSWER_COOLDOWN: Duration = Duration::from_secs(600);

/// Engine integration tests run several engines in one process and share this
/// global state; throttling there would make blob transfers timing-dependent
/// (and parallel tests reuse keys like "device-a"), so the test build disables
/// the cooldowns. The cooldown logic itself is covered by the unit tests below
/// via `on_cooldown` with an explicit duration.
#[cfg(test)]
const REQUEST_COOLDOWN: Duration = Duration::ZERO;
#[cfg(not(test))]
const REQUEST_COOLDOWN: Duration = BLOB_REQUEST_COOLDOWN;
#[cfg(test)]
const ANSWER_COOLDOWN: Duration = Duration::ZERO;
#[cfg(not(test))]
const ANSWER_COOLDOWN: Duration = BLOB_ANSWER_COOLDOWN;

fn blob_request_log() -> &'static Mutex<HashMap<String, Instant>> {
    static LOG: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn blob_answer_log() -> &'static Mutex<HashMap<String, Instant>> {
    static LOG: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn on_cooldown(log: &Mutex<HashMap<String, Instant>>, key: &str, cooldown: Duration) -> bool {
    log.lock()
        .unwrap()
        .get(key)
        .map(|t| t.elapsed() < cooldown)
        .unwrap_or(false)
}

/// True while a blob request to `peer` is inside its cooldown window.
pub fn blob_request_on_cooldown(peer_device_id: &str) -> bool {
    on_cooldown(blob_request_log(), peer_device_id, REQUEST_COOLDOWN)
}

/// Record that a blob request to `peer` was (attempted to be) sent. Recorded
/// on attempt rather than success so a failing relay is not hammered on every
/// incoming changeset.
pub fn note_blob_request_sent(peer_device_id: &str) {
    blob_request_log()
        .lock()
        .unwrap()
        .insert(peer_device_id.to_string(), Instant::now());
}

/// True while answers for `hash` are inside their cooldown window.
pub fn blob_answer_on_cooldown(hash: &str) -> bool {
    on_cooldown(blob_answer_log(), hash, ANSWER_COOLDOWN)
}

/// Record that the payload for `hash` was sent to a requester.
pub fn note_blob_answered(hash: &str) {
    blob_answer_log()
        .lock()
        .unwrap()
        .insert(hash.to_string(), Instant::now());
}

/// Read a blob and encode it as base64 for transport.
pub fn read_blob_base64(
    app_data_dir: &std::path::Path,
    hash: &str,
    ext: &str,
) -> Result<Option<String>> {
    let path = file_store::blob_path(app_data_dir, hash, ext);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read blob {}", path.display()))?;
    Ok(Some(
        base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
}

/// Decode base64 data and write it into the blob store.
pub fn write_blob_from_base64(
    app_data_dir: &std::path::Path,
    hash: &str,
    ext: &str,
    data: &str,
) -> Result<()> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("decode blob base64")?;
    let computed = file_store::sha256_hex(&bytes);
    if computed != hash {
        anyhow::bail!("blob hash mismatch: expected {}, got {}", hash, computed);
    }
    let dest = file_store::blob_path(app_data_dir, hash, ext);
    // Content-addressed: an existing file with this name already holds these
    // exact bytes. Duplicate payloads (several peers answering the same
    // startup rescan, or pre-throttle request storms still in flight) must
    // not rewrite it.
    if dest.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(file_store::blob_dir(app_data_dir))?;
    std::fs::write(&dest, bytes).with_context(|| format!("write blob {}", dest.display()))?;
    info!(hash = %hash, ext = %ext, "wrote received blob");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "siku-attachments-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn blob_ref(hash: &str, ext: &str) -> String {
        format!("blobs/{hash}.{ext}")
    }

    #[test]
    fn extract_blob_refs_finds_markdown_paths() {
        let h1 = "a".repeat(64);
        let h2 = "b".repeat(64);
        let h3 = "c".repeat(64);
        let text = format!(
            "开头 ![Pasted image]({}) 中间 ![{alt}]({}) 结尾引用 {}，\n\
             短路径 blobs/short.png 不算，非 blobs 的 ./img.png 不算",
            blob_ref(&h1, "png"),
            blob_ref(&h2, "jpg"),
            blob_ref(&h3, "webp"),
            alt = "alt"
        );
        let refs = extract_blob_refs(&text);
        assert!(refs.contains(&blob_ref(&h1, "png")), "png ref: {refs:?}");
        assert!(refs.contains(&blob_ref(&h2, "jpg")), "jpg ref: {refs:?}");
        assert!(refs.contains(&blob_ref(&h3, "webp")), "webp ref: {refs:?}");
        assert_eq!(refs.len(), 3, "only full sha256 blobs/: paths: {refs:?}");
    }

    #[test]
    fn blob_request_throttle_blocks_immediate_repeat() {
        // The public wrappers use a zero cooldown in test builds (parallel
        // engine tests share the global state), so exercise the logic via
        // `on_cooldown` with an explicit window.
        let peer = format!("throttle-peer-{}", std::process::id());
        let window = Duration::from_secs(3600);
        assert!(!on_cooldown(blob_request_log(), &peer, window));
        note_blob_request_sent(&peer);
        assert!(on_cooldown(blob_request_log(), &peer, window));
    }

    #[test]
    fn blob_answer_throttle_blocks_immediate_repeat() {
        let hash = format!("throttle-hash-{}", std::process::id());
        let window = Duration::from_secs(3600);
        assert!(!on_cooldown(blob_answer_log(), &hash, window));
        note_blob_answered(&hash);
        assert!(on_cooldown(blob_answer_log(), &hash, window));
    }

    #[tokio::test]
    async fn collect_missing_finds_note_image_blobs() {
        let dir = temp_dir("note-img");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE notes (id TEXT PRIMARY KEY, content TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE papers (id TEXT PRIMARY KEY, file_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE attachments (id TEXT PRIMARY KEY, file_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE files (id TEXT PRIMARY KEY, blob_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        let h1 = "a".repeat(64);
        let h2 = "b".repeat(64);
        let missing_ref = blob_ref(&h1, "png");
        let present_ref = blob_ref(&h2, "png");
        // h2 already exists locally, h1 does not.
        std::fs::create_dir_all(file_store::blob_dir(&dir)).unwrap();
        std::fs::write(file_store::blob_path(&dir, &h2, "png"), b"present").unwrap();

        sqlx::query("INSERT INTO notes (id, content) VALUES ('n1', ?)")
            .bind(format!("![a]({missing_ref}) 正文 ![b]({present_ref})"))
            .execute(&pool)
            .await
            .unwrap();

        let missing = collect_missing_blob_hashes(&pool, &dir).await.unwrap();
        assert!(
            missing.contains(&(h1.clone(), "png".to_string())),
            "note-referenced blob must be reported missing: {missing:?}"
        );
        assert!(
            !missing.iter().any(|(h, _)| *h == h2),
            "present blob must not be requested: {missing:?}"
        );
    }

    #[tokio::test]
    async fn collect_missing_finds_vault_file_blobs() {
        let dir = temp_dir("vault-file");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE notes (id TEXT PRIMARY KEY, content TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE papers (id TEXT PRIMARY KEY, file_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE attachments (id TEXT PRIMARY KEY, file_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE files (id TEXT PRIMARY KEY, blob_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        let h1 = "d".repeat(64);
        let h2 = "e".repeat(64);
        // h2 already exists locally, h1 does not.
        std::fs::create_dir_all(file_store::blob_dir(&dir)).unwrap();
        std::fs::write(file_store::blob_path(&dir, &h2, "pdf"), b"present").unwrap();

        sqlx::query("INSERT INTO files (id, blob_path) VALUES ('f1', ?), ('f2', ?), ('f3', '')")
            .bind(blob_ref(&h1, "pdf"))
            .bind(blob_ref(&h2, "pdf"))
            .execute(&pool)
            .await
            .unwrap();

        let missing = collect_missing_blob_hashes(&pool, &dir).await.unwrap();
        assert!(
            missing.contains(&(h1.clone(), "pdf".to_string())),
            "files.blob_path must be reported missing: {missing:?}"
        );
        assert!(
            !missing.iter().any(|(h, _)| *h == h2),
            "present blob must not be requested: {missing:?}"
        );
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn chunks_assemble_out_of_order_byte_identical() {
        let dir = temp_dir("assemble");
        let parts: [&[u8]; 3] = [b"aaa", b"bbb", b"ccc"];
        let full: Vec<u8> = parts.concat();
        let hash = file_store::sha256_hex(&full);

        // Out-of-order arrival: 2, 0, 1 — assembly must wait for all three.
        write_incoming_chunk(&dir, &hash, 2, 3, &b64(parts[2])).unwrap();
        assert!(!try_assemble_blob(&dir, &hash, "pdf", 3).unwrap());
        write_incoming_chunk(&dir, &hash, 0, 3, &b64(parts[0])).unwrap();
        assert!(!try_assemble_blob(&dir, &hash, "pdf", 3).unwrap());
        assert_eq!(received_chunk_indices(&dir, &hash), vec![0, 2]);
        assert_eq!(missing_chunk_indices(&dir, &hash), Some(vec![1]));
        write_incoming_chunk(&dir, &hash, 1, 3, &b64(parts[1])).unwrap();
        assert!(try_assemble_blob(&dir, &hash, "pdf", 3).unwrap());

        let written = std::fs::read(file_store::blob_path(&dir, &hash, "pdf")).unwrap();
        assert_eq!(written, full, "assembled blob must be byte-identical");
        // Staging is cleared and re-assembly is an idempotent no-op.
        assert_eq!(received_chunk_indices(&dir, &hash), Vec::<u32>::new());
        assert!(try_assemble_blob(&dir, &hash, "pdf", 3).unwrap());
    }

    #[test]
    fn missing_chunk_indices_none_without_staging() {
        let dir = temp_dir("no-staging");
        let hash = "f".repeat(64);
        assert_eq!(missing_chunk_indices(&dir, &hash), None);
    }

    #[test]
    fn hash_mismatch_discards_staging_and_errors() {
        let dir = temp_dir("bad-hash");
        let parts: [&[u8]; 2] = [b"xxx", b"yyy"];
        // Claim a hash that does not match the assembled bytes.
        let hash = file_store::sha256_hex(b"something else entirely");
        for (i, p) in parts.iter().enumerate() {
            write_incoming_chunk(&dir, &hash, i as u32, 2, &b64(p)).unwrap();
        }
        let err = try_assemble_blob(&dir, &hash, "pdf", 2).unwrap_err();
        assert!(err.to_string().contains("hash mismatch"), "{err}");
        assert!(
            !staging_dir(&dir, &hash).exists(),
            "staging dir must be discarded after a hash mismatch"
        );
        assert!(!file_store::has_blob(&dir, &hash));
    }

    #[test]
    fn dedup_key_is_deterministic_and_key_bound() {
        let key_a = [7u8; crate::sync::crypto::SYNC_KEY_LEN];
        let key_b = [9u8; crate::sync::crypto::SYNC_KEY_LEN];
        let hash = "a".repeat(64);
        assert_eq!(blob_dedup_key(&key_a, &hash), blob_dedup_key(&key_a, &hash));
        assert_ne!(blob_dedup_key(&key_a, &hash), blob_dedup_key(&key_b, &hash));
        assert_ne!(
            blob_dedup_key(&key_a, &hash),
            blob_dedup_key(&key_a, &format!("{hash}:0")),
            "chunk keys must differ from the whole-blob key"
        );
    }

    async fn settings_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE device_settings (key TEXT PRIMARY KEY, value TEXT, updated_at TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn push_marks_flow_pending_to_pushed() {
        let dir = temp_dir("push-marks");
        let pool = settings_pool().await;
        let bytes = b"push me";
        let hash = file_store::sha256_hex(bytes);
        std::fs::create_dir_all(file_store::blob_dir(&dir)).unwrap();
        std::fs::write(file_store::blob_path(&dir, &hash, "pdf"), bytes).unwrap();

        mark_blob_pending_push(&pool, &dir, &hash, "pdf").await;
        assert_eq!(
            pending_blob_pushes(&pool).await,
            vec![(hash.clone(), "pdf".to_string())]
        );

        mark_blob_pushed(&pool, &hash).await;
        assert!(pending_blob_pushes(&pool).await.is_empty());
        // A blob already pushed must not be re-pended.
        mark_blob_pending_push(&pool, &dir, &hash, "pdf").await;
        assert!(pending_blob_pushes(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn push_retry_marker_backs_off() {
        let dir = temp_dir("push-retry");
        let pool = settings_pool().await;
        let bytes = b"retry me";
        let hash = file_store::sha256_hex(bytes);
        std::fs::create_dir_all(file_store::blob_dir(&dir)).unwrap();
        std::fs::write(file_store::blob_path(&dir, &hash, "pdf"), bytes).unwrap();

        mark_blob_pending_push(&pool, &dir, &hash, "pdf").await;
        mark_push_retry(&pool, &hash, "pdf", 3600).await;
        assert!(
            pending_blob_pushes(&pool).await.is_empty(),
            "a marker inside its backoff window must be skipped"
        );
        // A zero delay re-arms it immediately (still carries the ext).
        mark_push_retry(&pool, &hash, "pdf", 0).await;
        assert_eq!(
            pending_blob_pushes(&pool).await,
            vec![(hash.clone(), "pdf".to_string())]
        );
    }

    #[tokio::test]
    async fn oversized_blob_is_not_marked_pending() {
        let dir = temp_dir("push-oversized");
        let pool = settings_pool().await;
        // mark_blob_pending_push only consults the file size; a missing file
        // maps to u64::MAX and must be skipped the same way.
        let hash = "b".repeat(64);
        mark_blob_pending_push(&pool, &dir, &hash, "pdf").await;
        assert!(pending_blob_pushes(&pool).await.is_empty());
    }
}
