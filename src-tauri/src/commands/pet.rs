//! Pet skin commands: `.codex-pet`-compatible sprite pets.
//!
//! Sources scanned (first match wins on id):
//! - `{app_data_dir}/pets/<id>/` — user-installed pets
//! - `$CODEX_HOME/pets/<id>/` or `~/.codex/pets/<id>/` — pets installed for
//!   Codex show up automatically (read-only), same as the DSH plugin.
//!
//! Manifest is `pet.json` (legacy `avatar.json` accepted): id, displayName,
//! description, spritesheetPath (default `spritesheet.webp`), frame
//! (default 192×208; grid = sheet size ÷ frame size), spriteVersionNumber,
//! animations: name → { frames: [sprite indices], fps = 8, loop = true,
//! fallback = "idle" }.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tauri::{Manager, State};

use crate::AppState;

const SKIN_SETTING_KEY: &str = "pet.skin";
const SIZE_SETTING_KEY: &str = "pet.size";
/// Refuse pathological sheets before base64'ing them into the webview.
const MAX_SHEET_BYTES: u64 = 16 * 1024 * 1024;

/// Selectable ball edge lengths (logical px).
pub const PET_SIZES: [u32; 4] = [48, 72, 96, 128];

/// Configured ball edge length. Read by `create_pet_window` and the resize
/// self-heal clamp in lib.rs; written at startup (from the device setting)
/// and by `pet_set_size`.
static PET_WINDOW_SIZE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(48);

pub fn pet_window_size() -> u32 {
    PET_WINDOW_SIZE.load(std::sync::atomic::Ordering::Relaxed)
}

fn set_pet_window_size(size: u32) {
    PET_WINDOW_SIZE.store(size, std::sync::atomic::Ordering::Relaxed);
}

/// Load the persisted size into the global. Called at startup before the
/// pet window is created.
pub async fn load_pet_size(db: &sqlx::SqlitePool) {
    if let Ok(Some(s)) = crate::core::settings_service::get_device_setting(db, SIZE_SETTING_KEY)
        .await
    {
        if let Ok(n) = s.parse::<u32>() {
            if PET_SIZES.contains(&n) {
                set_pet_window_size(n);
            }
        }
    }
}

/// Current ball edge length (ball window asks on startup).
#[tauri::command]
pub async fn pet_get_size() -> u32 {
    pet_window_size()
}

/// Resize the ball. The window itself is rebuilt at the same edge length so
/// the transparent hit region always matches the visible ball exactly —
/// growing the rectangle beyond the ball is the click-trap bug this whole
/// design exists to avoid.
#[tauri::command]
pub async fn pet_set_size(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    size: u32,
) -> Result<(), String> {
    if !PET_SIZES.contains(&size) {
        return Err(format!("invalid pet size: {size}"));
    }
    crate::core::settings_service::set_device_setting(&state.db, SIZE_SETTING_KEY, &size.to_string())
        .await?;
    set_pet_window_size(size);
    if let Some(pet) = app_handle.get_webview_window("pet") {
        let edge = size as f64;
        let _ = pet.set_size(tauri::Size::Logical(tauri::LogicalSize::new(edge, edge)));
    }
    // The ball window listens and re-renders at the new scale.
    use tauri::Emitter;
    let _ = app_handle.emit("pet:size_changed", size);
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetTrack {
    pub frames: Vec<u32>,
    #[serde(default = "default_fps")]
    pub fps: f32,
    #[serde(default = "default_loop", rename = "loop")]
    pub loop_: bool,
    #[serde(default)]
    pub fallback: Option<String>,
}

fn default_fps() -> f32 {
    8.0
}
fn default_loop() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetFrame {
    #[serde(default = "default_frame_w")]
    pub width: u32,
    #[serde(default = "default_frame_h")]
    pub height: u32,
}

// Manual impl: serde's per-field defaults only apply when a (partial) frame
// object is present; a missing `frame` key falls back to `Default::default()`,
// which must also be the codex default 192×208.
impl Default for PetFrame {
    fn default() -> Self {
        Self { width: 192, height: 208 }
    }
}

fn default_frame_w() -> u32 {
    192
}
fn default_frame_h() -> u32 {
    208
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetManifest {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_sheet")]
    pub spritesheet_path: String,
    #[serde(default)]
    pub frame: PetFrame,
    #[serde(default)]
    pub sprite_version_number: Option<u32>,
    #[serde(default)]
    pub animations: HashMap<String, PetTrack>,
}

fn default_sheet() -> String {
    "spritesheet.webp".to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PetSkinSummary {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PetSkinDetail {
    pub manifest: PetManifest,
    /// data: URL of the spritesheet (image/webp or image/png).
    pub sheet_data_url: String,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

fn pet_dirs(app_data_dir: &Path) -> Vec<(PathBuf, &'static str)> {
    let mut dirs = vec![(app_data_dir.join("pets"), "app")];
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".codex")));
    if let Some(home) = codex_home {
        dirs.push((home.join("pets"), "codex"));
    }
    dirs
}

fn read_manifest(dir: &Path) -> Option<PetManifest> {
    for name in ["pet.json", "avatar.json"] {
        let path = dir.join(name);
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(mut m) = serde_json::from_str::<PetManifest>(&text) {
                if m.id.is_empty() {
                    m.id = dir.file_name()?.to_string_lossy().into_owned();
                }
                return Some(m);
            }
        }
    }
    None
}

fn scan_pets(app_data_dir: &Path) -> Vec<(PetSkinSummary, PathBuf, PetManifest)> {
    let mut out: Vec<(PetSkinSummary, PathBuf, PetManifest)> = Vec::new();
    for (root, source) in pet_dirs(app_data_dir) {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(manifest) = read_manifest(&dir) else { continue };
            // First source wins on id collision.
            if out.iter().any(|(s, _, _)| s.id == manifest.id) {
                continue;
            }
            // A pet without its spritesheet is broken; list it disabled-less:
            // skip entirely, the settings page has no use for it.
            if !dir.join(&manifest.spritesheet_path).is_file() {
                continue;
            }
            out.push((
                PetSkinSummary {
                    id: manifest.id.clone(),
                    display_name: if manifest.display_name.is_empty() {
                        manifest.id.clone()
                    } else {
                        manifest.display_name.clone()
                    },
                    description: manifest.description.clone(),
                    source: source.to_string(),
                },
                dir,
                manifest,
            ));
        }
    }
    out.sort_by(|a, b| a.0.display_name.cmp(&b.0.display_name));
    out
}

/// List installed pet skins (settings page picker).
#[tauri::command]
pub async fn pet_list_skins(state: State<'_, AppState>) -> Result<Vec<PetSkinSummary>, String> {
    Ok(scan_pets(&state.app_data_dir)
        .into_iter()
        .map(|(s, _, _)| s)
        .collect())
}

/// Full manifest + spritesheet for the ball window's renderer.
#[tauri::command]
pub async fn pet_get_skin(state: State<'_, AppState>, id: String) -> Result<PetSkinDetail, String> {
    let (_, dir, manifest) = scan_pets(&state.app_data_dir)
        .into_iter()
        .find(|(s, _, _)| s.id == id)
        .ok_or_else(|| format!("pet skin not found: {id}"))?;
    let sheet = dir.join(&manifest.spritesheet_path);
    let meta = std::fs::metadata(&sheet).map_err(|e| format!("read spritesheet: {e}"))?;
    if meta.len() > MAX_SHEET_BYTES {
        return Err("spritesheet too large".to_string());
    }
    let bytes = std::fs::read(&sheet).map_err(|e| format!("read spritesheet: {e}"))?;
    let mime = match bytes.get(..4) {
        Some(b"RIFF") => "image/webp",
        Some([0x89, b'P', b'N', b'G']) => "image/png",
        Some([0xFF, 0xD8, 0xFF, _]) => "image/jpeg",
        _ => "image/webp",
    };
    use base64::Engine;
    Ok(PetSkinDetail {
        manifest,
        sheet_data_url: format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ),
    })
}

/// Selected skin id ('' = the built-in CSS ball). Device-local setting.
#[tauri::command]
pub async fn pet_get_skin_selection(state: State<'_, AppState>) -> Result<String, String> {
    Ok(
        crate::core::settings_service::get_device_setting(&state.db, SKIN_SETTING_KEY)
            .await?
            .unwrap_or_default(),
    )
}

#[tauri::command]
pub async fn pet_set_skin_selection(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    id: String,
) -> Result<(), String> {
    if !id.is_empty()
        && scan_pets(&state.app_data_dir)
            .iter()
            .all(|(s, _, _)| s.id != id)
    {
        return Err(format!("pet skin not found: {id}"));
    }
    crate::core::settings_service::set_device_setting(&state.db, SKIN_SETTING_KEY, &id).await?;
    // The ball window listens and re-renders with the new skin.
    use tauri::Emitter;
    let _ = app_handle.emit("pet:skin_changed", ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_finds_codex_manifest_and_skips_broken() {
        let root = std::env::temp_dir().join(format!("siku-pets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let good = root.join("pets").join("rocky");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(good.join("spritesheet.webp"), b"RIFF....WEBP").unwrap();
        std::fs::write(
            good.join("pet.json"),
            r#"{"id":"rocky","displayName":"Rocky","animations":{"idle":{"frames":[0,1]},"running":{"frames":[8,9,10]}}}"#,
        )
        .unwrap();
        // Broken: manifest without its spritesheet must be skipped.
        let broken = root.join("pets").join("ghost");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("pet.json"), r#"{"id":"ghost"}"#).unwrap();

        let found = scan_pets(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0.id, "rocky");
        assert_eq!(found[0].0.display_name, "Rocky");
        assert_eq!(found[0].2.animations["running"].frames, vec![8, 9, 10]);
        // Defaults: 192×208 frames, 8fps looping sheet path.
        assert_eq!(found[0].2.frame.width, 192);
        assert_eq!(found[0].2.animations["idle"].fps, 8.0);
        assert!(found[0].2.animations["idle"].loop_);
        std::fs::remove_dir_all(&root).ok();
    }
}
