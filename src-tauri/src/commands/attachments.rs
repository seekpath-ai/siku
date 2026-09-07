use std::path::Path;
use tauri::State;
use tracing::instrument;

use base64::Engine;
use crate::ai::llm::ImageAttachment;
use crate::file_store;
use crate::AppState;

#[derive(serde::Deserialize)]
pub struct SaveClipboardImageInput {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Save an image from the clipboard into the content-addressed blob store.
/// Returns the Markdown-friendly relative path: "blobs/{sha256}.png".
#[tauri::command]
#[instrument(skip(input, state))]
pub async fn save_clipboard_image(
    state: State<'_, AppState>,
    input: SaveClipboardImageInput,
) -> Result<String, String> {
    let SaveClipboardImageInput {
        rgba,
        width,
        height,
    } = input;

    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or("image dimensions too large")?;
    if rgba.len() != expected {
        return Err(format!(
            "rgba size mismatch: expected {}, got {}",
            expected,
            rgba.len()
        ));
    }

    let img = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or("failed to create image buffer")?;

    let mut bytes: Vec<u8> = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .map_err(|e| format!("encode png failed: {e}"))?;

    let rel_path = file_store::write_blob(&state.app_data_dir, &bytes, "png")
        .map_err(|e| format!("write blob failed: {e}"))?;

    if let Some((hash, ext)) = file_store::parse_blob_path(&rel_path) {
        crate::sync::attachments::mark_blob_pending_push(
            &state.db,
            &state.app_data_dir,
            &hash,
            &ext,
        )
        .await;
    }

    Ok(rel_path)
}

#[derive(serde::Deserialize)]
pub struct SaveAttachmentBytesInput {
    pub bytes: Vec<u8>,
    pub filename: String,
}

/// Save arbitrary attachment bytes (used for drag-and-drop image files) into the
/// content-addressed blob store. Returns the Markdown-friendly relative path:
/// "blobs/{sha256}.{ext}".
#[tauri::command]
#[instrument(skip(input, state))]
pub async fn save_attachment_bytes(
    state: State<'_, AppState>,
    input: SaveAttachmentBytesInput,
) -> Result<String, String> {
    let SaveAttachmentBytesInput { bytes, filename } = input;

    let sanitized = sanitize_filename(&filename);
    if sanitized.is_empty() {
        return Err("invalid filename".into());
    }
    let lower = sanitized.to_lowercase();
    if !lower.ends_with(".png")
        && !lower.ends_with(".jpg")
        && !lower.ends_with(".jpeg")
        && !lower.ends_with(".gif")
        && !lower.ends_with(".webp")
        && !lower.ends_with(".svg")
        && !lower.ends_with(".bmp")
        && !lower.ends_with(".avif")
    {
        return Err("only image attachments are supported".into());
    }

    let ext = Path::new(&sanitized)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin");

    let rel_path = file_store::write_blob(&state.app_data_dir, &bytes, ext)
        .map_err(|e| format!("write blob failed: {e}"))?;

    if let Some((hash, ext)) = file_store::parse_blob_path(&rel_path) {
        crate::sync::attachments::mark_blob_pending_push(
            &state.db,
            &state.app_data_dir,
            &hash,
            &ext,
        )
        .await;
    }

    Ok(rel_path)
}

fn sanitize_filename(name: &str) -> String {
    // Keep basename only; drop any path components.
    let base = std::path::Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    base.chars()
        .map(|c| match c {
            // Windows reserved characters.
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            // Whitespace would produce invalid Markdown image URLs (spaces
            // terminate the URL). Replace with underscore so both the saved
            // filename and the relative link are safe.
            c if c.is_whitespace() => '_',
            _ => c,
        })
        .collect()
}

/// Return the absolute blob directory. Useful for the frontend to enumerate
/// or resolve local attachments.
#[tauri::command]
#[instrument(skip(state))]
pub async fn vault_attachments_dir(state: State<'_, AppState>) -> Result<String, String> {
    Ok(file_store::blob_dir(&state.app_data_dir)
        .to_string_lossy()
        .to_string())
}

fn guess_image_mime(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "image/png",
    }
}

/// Extensions whose formats vLLM-style vision backends decode reliably
/// (verified against the production endpoint: png/jpeg/gif/bmp pass, webp
/// fails with "Failed to load image or audio file"). Anything else is
/// transcoded to PNG before upload.
fn is_api_safe_image_ext(path: &std::path::Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp")
}

/// Decode `bytes` and re-encode as PNG. Returns None when decoding fails.
fn transcode_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    crate::ai::llm::transcode_image_to_png(bytes)
}

#[cfg(test)]
mod tests {
    #[test]
    fn transcodes_webp_to_png() {
        // 8x8 red webp (the format vLLM backends reject with "Failed to load
        // image or audio file").
        use base64::Engine;
        let webp = base64::engine::general_purpose::STANDARD
            .decode("UklGRjwAAABXRUJQVlA4IDAAAADQAQCdASoIAAgAAgA0JaACdLoB+AADsAD+8MQL/yC5YXXI1/8gP+QH/ID/+PIAAAA=")
            .unwrap();
        let png = super::transcode_to_png(&webp).expect("webp should decode");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "output must be a PNG");
        // And the PNG must itself decode back to an 8x8 image.
        let img = image::load_from_memory(&png).expect("png should decode");
        assert_eq!((img.width(), img.height()), (8, 8));
    }
}

/// Read a local image file and return it as a base64-encoded ImageAttachment.
/// WebP and other exotic formats are transcoded to PNG: several OpenAI-
/// compatible vision backends (vLLM without webp support) reject them with
/// "Failed to load image or audio file".
#[tauri::command]
#[instrument]
pub async fn read_image_file(path: String) -> Result<ImageAttachment, String> {
    let resolved = std::path::Path::new(&path)
        .canonicalize()
        .map_err(|e| format!("invalid path: {e}"))?;
    if !resolved.is_file() {
        return Err(format!("not a file: {path}"));
    }
    let bytes = tokio::fs::read(&resolved)
        .await
        .map_err(|e| format!("read failed: {e}"))?;
    let name = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_string());
    let (bytes, mime) = if is_api_safe_image_ext(&resolved) {
        (bytes, guess_image_mime(&resolved.to_string_lossy()))
    } else {
        match transcode_to_png(&bytes) {
            Some(png) => (png, "image/png"),
            // Decode failed (or the format needs a codec we don't ship): send
            // the original and let the API judge it.
            None => (bytes, guess_image_mime(&resolved.to_string_lossy())),
        }
    };
    let base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(ImageAttachment { mime: mime.to_string(), base64, name })
}
