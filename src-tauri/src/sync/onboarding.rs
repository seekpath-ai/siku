use crate::core::settings_service;
use anyhow::{Context, Result};
use base64::Engine;
use std::fs::File;
use std::io::{self};
use std::path::Path;
use tracing::{info, warn};
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::AesMode;

/// Settings key under which the account-level sync key is stored
/// (device-local, never synced).
pub const ACCOUNT_SYNC_KEY_SETTING: &str = "account.sync_key";

/// Normalize a user-entered relay URL to the WebSocket endpoint
/// (`ws://host:port/v1/signaling`). Accepts any of:
///   ws://host:port, wss://host, http://host:port/v1/signaling, host:port …
pub fn normalize_ws_url(input: &str) -> String {
    let s = input.trim().trim_end_matches('/');
    // Keep only scheme://host:port (drop any path such as /v1/signaling).
    let base = s.split('/').take(3).collect::<Vec<_>>().join("/");
    let ws = if base.starts_with("ws://") || base.starts_with("wss://") {
        base
    } else if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{base}")
    };
    format!("{ws}/v1/signaling")
}

/// Normalize a user-entered relay URL to the HTTP API base
/// (`http://host:port`).
pub fn normalize_http_base(input: &str) -> String {
    let s = input.trim().trim_end_matches('/');
    let base = s.split('/').take(3).collect::<Vec<_>>().join("/");
    if base.starts_with("http://") || base.starts_with("https://") {
        base
    } else if let Some(rest) = base.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = base.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        format!("http://{base}")
    }
}

/// Extract the `sub` claim from a JWT without verifying the signature. The
/// relay treats `sub` as the room id, so the pairing payload must carry it —
/// a random room id would fail the relay's Join authorization check.
pub fn jwt_sub(token: &str) -> Result<String> {
    let payload_part = token
        .split('.')
        .nth(1)
        .context("invalid jwt: missing payload")?;
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_part)
        .context("invalid jwt payload encoding")?;
    let claims: serde_json::Value =
        serde_json::from_slice(&json).context("invalid jwt claims")?;
    claims
        .get("sub")
        .and_then(|v| v.as_str())
        .map(String::from)
        .context("jwt missing sub claim")
}

const SEED_DB_NAME: &str = "siku.db";
const SEED_BLOBS_DIR: &str = "blobs";

/// Create an AES-256 encrypted zip archive containing the SQLite database
/// and every file under `blobs/`. The password is typically the pairing code
/// or a user-supplied passphrase.
///
/// This synchronous variant archives files already on disk. For a live database
/// snapshot use `create_encrypted_seed_archive_from_pool` instead.
#[cfg(test)]
fn create_encrypted_seed_archive(
    app_data_dir: &Path,
    archive_path: &Path,
    password: &str,
) -> Result<()> {
    create_encrypted_seed_archive_from_dirs(app_data_dir, app_data_dir, archive_path, password)
}

fn create_encrypted_seed_archive_from_dirs(
    db_source_dir: &Path,
    blobs_source_dir: &Path,
    archive_path: &Path,
    password: &str,
) -> Result<()> {
    std::fs::create_dir_all(
        archive_path
            .parent()
            .context("archive path has no parent directory")?,
    )?;
    let file = File::create(archive_path).context("create seed archive")?;
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .with_aes_encryption(AesMode::Aes256, password);

    // Include the main database file if it exists.
    let db_path = db_source_dir.join(SEED_DB_NAME);
    if db_path.exists() {
        zip.start_file_from_path(SEED_DB_NAME, options)
            .context("start db entry")?;
        let mut db_file = File::open(&db_path).context("open db file")?;
        io::copy(&mut db_file, &mut zip).context("copy db into archive")?;
        info!(path = %db_path.display(), "added db to seed archive");
    }

    // Include all blob files, preserving relative paths.
    let blobs_dir = blobs_source_dir.join(SEED_BLOBS_DIR);
    if blobs_dir.exists() {
        for entry in walkdir::WalkDir::new(&blobs_dir).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(blobs_source_dir)
                .context("blob path is not under blobs_source_dir")?;
            let name = relative.to_string_lossy();
            zip.start_file_from_path(name.as_ref(), options)
                .with_context(|| format!("start archive entry {name}"))?;
            let mut src = File::open(path).with_context(|| format!("open blob {name}"))?;
            io::copy(&mut src, &mut zip).with_context(|| format!("copy blob {name}"))?;
        }
    }

    zip.finish().context("finish seed archive")?;
    info!(path = %archive_path.display(), "created encrypted seed archive");
    Ok(())
}

/// Create an AES-256 encrypted zip archive containing a consistent snapshot of
/// the currently-open SQLite database and all blobs. VACUUM INTO is used to
/// avoid copying the live database file directly, so the exported DB is
/// complete even when WAL mode is active.
pub async fn create_encrypted_seed_archive_from_pool(
    app_data_dir: &Path,
    archive_path: &Path,
    password: &str,
    db: &sqlx::SqlitePool,
) -> Result<()> {
    let temp_dir = tempfile::tempdir().context("create temp dir for seed export")?;
    let backup_path = temp_dir.path().join(SEED_DB_NAME);

    // VACUUM INTO creates a brand-new, consistent copy of the current database.
    // The destination path is a SQL literal, so single quotes are escaped.
    let backup_path_str = backup_path.to_string_lossy().replace('\'', "''");
    let sql = format!("VACUUM INTO '{backup_path_str}'");
    let mut conn = db.acquire().await.context("acquire db connection for seed export")?;
    sqlx::query(&sql)
        .execute(&mut *conn)
        .await
        .context("VACUUM INTO seed backup")?;
    drop(conn);

    create_encrypted_seed_archive_from_dirs(
        temp_dir.path(),
        app_data_dir,
        archive_path,
        password,
    )
}

/// Restore an encrypted seed archive into a target directory. Existing files
/// with the same relative path are overwritten. Callers should ensure the
/// target directory is not currently in use by an open database.
pub fn restore_encrypted_seed_archive(
    archive_path: &Path,
    target_app_data_dir: &Path,
    password: &str,
) -> Result<()> {
    let file = File::open(archive_path).context("open seed archive")?;
    let mut archive = zip::ZipArchive::new(file).context("read seed archive")?;

    std::fs::create_dir_all(target_app_data_dir)?;

    let mut restored_entries = 0usize;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index_decrypt(i, password.as_bytes())
            .context("invalid seed archive password")?;

        let entry_name = entry.name().to_owned();

        // Refuse unsafe entry names: absolute paths (including Windows drive
        // prefixes), root-relative paths, and any `..` component could escape
        // the target directory. Component-level checks are used instead of
        // `canonicalize()` — on Windows the canonical target path can carry a
        // `\\?\` prefix that never matches the non-existent destination's
        // fallback path, which silently skipped every entry.
        let entry_path = Path::new(&entry_name);
        let mut safe = !entry_path.is_absolute();
        if safe {
            for comp in entry_path.components() {
                match comp {
                    std::path::Component::Normal(_) => {}
                    std::path::Component::CurDir => {}
                    _ => {
                        safe = false;
                        break;
                    }
                }
            }
        }
        if !safe {
            warn!(entry = %entry_name, "skipping archive entry with unsafe path");
            continue;
        }

        let dest = target_app_data_dir.join(&entry_name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut out = File::create(&dest).with_context(|| format!("create {entry_name}"))?;
        io::copy(&mut entry, &mut out).with_context(|| format!("extract {entry_name}"))?;
        restored_entries += 1;
        info!(entry = %entry_name, "restored seed entry");
    }

    if restored_entries == 0 {
        anyhow::bail!("seed archive contained no extractable entries");
    }

    // The whole point of a seed is the database; fail loudly instead of
    // reporting a successful import that later yields an empty app.
    let db_path = target_app_data_dir.join(SEED_DB_NAME);
    let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    if db_size == 0 {
        anyhow::bail!(
            "seed archive did not contain a valid {} ({} bytes extracted)",
            SEED_DB_NAME,
            db_size
        );
    }
    info!(db_size, "seed restore verified {}", SEED_DB_NAME);

    Ok(())
}

/// Return a stable device ID, generating and persisting one if necessary.
pub async fn ensure_device_id(db: &sqlx::SqlitePool) -> Result<String> {
    let mut device_settings = settings_service::load_device_settings(db)
        .await
        .map_err(|e| anyhow::anyhow!("load device settings: {e}"))?;
    if device_settings.device_id.is_empty() {
        device_settings.device_id = Uuid::new_v4().to_string();
        settings_service::save_device_settings(db, &device_settings)
            .await
            .map_err(|e| anyhow::anyhow!("save device settings: {e}"))?;
    }
    Ok(device_settings.device_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "siku-onboarding-test-{}-{}",
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

    #[test]
    fn url_normalization() {
        assert_eq!(
            normalize_ws_url("ws://192.168.21.100:8080"),
            "ws://192.168.21.100:8080/v1/signaling"
        );
        assert_eq!(
            normalize_ws_url("http://192.168.21.100:8080/v1/signaling"),
            "ws://192.168.21.100:8080/v1/signaling"
        );
        assert_eq!(
            normalize_ws_url("192.168.21.100:8080"),
            "ws://192.168.21.100:8080/v1/signaling"
        );
        assert_eq!(
            normalize_http_base("ws://192.168.21.100:8080/v1/signaling"),
            "http://192.168.21.100:8080"
        );
        assert_eq!(
            normalize_http_base("https://relay.example.com/v1/signaling"),
            "https://relay.example.com"
        );
    }

    #[test]
    fn encrypted_seed_archive_round_trip() {
        let src = make_temp_dir();
        let db_path = src.join(SEED_DB_NAME);
        std::fs::write(&db_path, b"fake sqlite db").unwrap();
        let blob_dir = src.join(SEED_BLOBS_DIR);
        std::fs::create_dir_all(&blob_dir).unwrap();
        std::fs::write(blob_dir.join("abc.pdf"), b"fake pdf").unwrap();
        std::fs::write(blob_dir.join("def.png"), b"fake image").unwrap();

        let archive = make_temp_dir().join("seed.zip");
        let password = "123456";
        create_encrypted_seed_archive(&src, &archive, password).unwrap();

        let dst = make_temp_dir();
        restore_encrypted_seed_archive(&archive, &dst, password).unwrap();

        assert_eq!(
            std::fs::read(dst.join(SEED_DB_NAME)).unwrap(),
            b"fake sqlite db"
        );
        assert_eq!(
            std::fs::read(dst.join(SEED_BLOBS_DIR).join("abc.pdf")).unwrap(),
            b"fake pdf"
        );
        assert_eq!(
            std::fs::read(dst.join(SEED_BLOBS_DIR).join("def.png")).unwrap(),
            b"fake image"
        );

        let wrong_dst = make_temp_dir();
        let err = restore_encrypted_seed_archive(&archive, &wrong_dst, "wrong").unwrap_err();
        assert!(
            err.to_string().contains("invalid seed archive password")
                || err.to_string().contains("Password")
                || err.to_string().contains("password"),
            "unexpected error: {err}"
        );
    }
}
