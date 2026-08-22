use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use tracing::{info, instrument};

const MAX_IMAGE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn mime_to_extension(mime: &str) -> &'static str {
    match mime {
        "image/webp" => "webp",
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        _ => "bin",
    }
}

fn image_cache_dir(app_data_dir: &std::path::Path) -> PathBuf {
    app_data_dir.join("image_cache")
}

/// Download a remote image, cache it under the app data directory, and return
/// the relative path: "image_cache/{sha256}.{ext}". The frontend can resolve
/// it to an absolute path via `resolve_cached_image_path` or Tauri's asset
/// protocol.
#[tauri::command]
#[instrument(skip(app))]
pub async fn cache_remote_image(app: AppHandle, url: String) -> Result<String, String> {
    let parsed = url
        .parse::<reqwest::Url>()
        .map_err(|e| format!("invalid url: {e}"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err("only http/https image URLs are allowed".into());
    }

    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir error: {e}"))?;
    let cache_dir = image_cache_dir(&app_data_dir);
    std::fs::create_dir_all(&cache_dir).map_err(|e| format!("create cache dir failed: {e}"))?;

    // Cache key is the SHA-256 of the URL, so the same URL always maps to the
    // same file. If a previous download already exists (any extension), reuse
    // it instead of downloading again — otherwise every app restart re-downloads
    // and overwrites every remote image in notes.
    let hash = format!("{:x}", Sha256::digest(url.as_bytes()));
    if let Ok(entries) = std::fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(rest) = name.strip_prefix(&hash) else { continue };
            if rest.starts_with('.') && !rest.contains("tmp") {
                if entry.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                    info!(url = %url, cached = %name, "remote image cache hit");
                    return Ok(format!("image_cache/{name}"));
                }
            }
        }
    }

    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|e| format!("failed to build http client: {e}"))?;

    let resp = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| format!("download failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("download failed: {}", resp.status()));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    if !content_type.starts_with("image/") {
        return Err(format!("not an image: {content_type}"));
    }

    if let Some(len) = resp.content_length() {
        if len > MAX_IMAGE_SIZE {
            return Err("image exceeds maximum size".into());
        }
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("read response failed: {e}"))?;

    if bytes.len() as u64 > MAX_IMAGE_SIZE {
        return Err("image exceeds maximum size".into());
    }

    let ext = mime_to_extension(&content_type);
    let file_path: PathBuf = cache_dir.join(format!("{hash}.{ext}"));
    tokio::fs::write(&file_path, bytes)
        .await
        .map_err(|e| format!("write cache failed: {e}"))?;

    Ok(format!("image_cache/{hash}.{ext}"))
}

/// Resolve a cached image relative path to an absolute filesystem path.
#[tauri::command]
#[instrument(skip(app))]
pub async fn resolve_cached_image_path(app: AppHandle, rel_path: String) -> Result<String, String> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir error: {e}"))?;

    let abs = if let Some(stripped) = rel_path.strip_prefix("image_cache/") {
        image_cache_dir(&app_data_dir).join(stripped)
    } else {
        app_data_dir.join(&rel_path)
    };

    Ok(abs.to_string_lossy().to_string())
}
