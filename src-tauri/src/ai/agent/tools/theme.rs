//! set_app_theme: switch the UI theme from chat ("换成护眼绿" / "强调色改成橙色").
//!
//! Writes `theme_preset` / `theme_overrides` into AppSettings and emits
//! `app:settings_changed`; every webview re-applies the CSS variables live
//! (see src/lib/theme.ts). UI-only and fully reversible ("恢复默认主题"),
//! so it is auto-approved like a read-only tool.
//!
//! The preset list here (background/text pairs) mirrors THEME_PRESETS in
//! src/lib/theme.ts for validation only — the frontend owns the full
//! variable tables.

use async_trait::async_trait;
use sqlx::SqlitePool;
use tauri::Emitter;

use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::core::settings_service;

/// preset id → (background hex, primary text hex) for the contrast guard.
const PRESET_CONTRAST: &[(&str, &str, &str)] = &[
    ("default-dark", "#0d0d0d", "#f5f5f5"),
    ("light", "#ffffff", "#18181b"),
    ("eye-care", "#161d18", "#e4ede6"),
    ("ocean", "#0b1220", "#f1f5f9"),
];

/// Semantic color keys accepted as overrides (see THEME_SEMANTIC_VARS).
const OVERRIDE_KEYS: &[&str] = &[
    "accent",
    "accent_hover",
    "background",
    "surface",
    "hover",
    "border",
    "text",
    "text_secondary",
    "muted",
    "primary_brand",
    "warning",
    "danger",
    "code_bg",
];

pub struct SetAppThemeTool {
    db: SqlitePool,
    app_handle: Option<tauri::AppHandle>,
}

impl SetAppThemeTool {
    pub fn new(db: SqlitePool, app_handle: Option<tauri::AppHandle>) -> Self {
        Self { db, app_handle }
    }
}

fn parse_hex(s: &str) -> Option<(f64, f64, f64)> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some((
        ((n >> 16) & 255) as f64 / 255.0,
        ((n >> 8) & 255) as f64 / 255.0,
        (n & 255) as f64 / 255.0,
    ))
}

fn luminance(rgb: (f64, f64, f64)) -> f64 {
    let f = |c: f64| {
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(rgb.0) + 0.7152 * f(rgb.1) + 0.0722 * f(rgb.2)
}

fn contrast_ratio(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    let (hi, lo) = (luminance(a).max(luminance(b)), luminance(a).min(luminance(b)));
    (hi + 0.05) / (lo + 0.05)
}

#[async_trait]
impl Tool for SetAppThemeTool {
    fn name(&self) -> &str {
        "set_app_theme"
    }

    fn description(&self) -> &str {
        "Change the app's UI theme. Applies immediately. Use `preset` for a whole theme (default-dark 深色 / light 浅色 / eye-care 护眼绿 / ocean 深蓝), and/or individual color overrides like accent=\"#e67e22\" (semantic keys: accent, accent_hover, background, surface, hover, border, text, text_secondary, muted, primary_brand, warning, danger, code_bg). `reset=true` restores the default dark theme and clears all overrides. Changes persist across restarts and sync to the user's other devices."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        let mut params = vec![
            ToolParameter {
                name: "preset".into(),
                param_type: "string".into(),
                description: "Theme preset id. Omit to only adjust individual colors.".into(),
                required: false,
            },
            ToolParameter {
                name: "reset".into(),
                param_type: "boolean".into(),
                description: "Restore the default dark theme and clear all overrides.".into(),
                required: false,
            },
        ];
        for key in OVERRIDE_KEYS {
            params.push(ToolParameter {
                name: (*key).into(),
                param_type: "string".into(),
                description: format!("Hex color override, e.g. \"#e67e22\" ({key})."),
                required: false,
            });
        }
        params
    }

    fn param_enums(&self) -> &'static [(&'static str, &'static [&'static str])] {
        &[(
            "preset",
            &["default-dark", "light", "eye-care", "ocean"],
        )]
    }

    /// UI-only, instantly reversible ("恢复默认主题") — no approval prompt.
    fn readonly(&self) -> bool {
        true
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let mut settings = settings_service::load_app_settings(&self.db).await?;

        if args.get("reset").and_then(|v| v.as_bool()) == Some(true) {
            settings.theme_preset = None;
            settings.theme_overrides = None;
        } else {
            if let Some(preset) = args.get("preset").and_then(|v| v.as_str()) {
                if !PRESET_CONTRAST.iter().any(|(id, _, _)| *id == preset) {
                    return Err(format!(
                        "unknown preset \"{preset}\"; available: default-dark, light, eye-care, ocean"
                    ));
                }
                settings.theme_preset = Some(preset.to_string());
            }

            let mut touched: Vec<(String, String)> = Vec::new();
            for key in OVERRIDE_KEYS {
                if let Some(hex) = args.get(*key).and_then(|v| v.as_str()) {
                    if parse_hex(hex).is_none() {
                        return Err(format!("invalid hex color for {key}: \"{hex}\" (expect #rrggbb)"));
                    }
                    let normalized = format!("#{}", hex.trim().trim_start_matches('#').to_lowercase());
                    touched.push(((*key).to_string(), normalized));
                }
            }
            if !touched.is_empty() {
                let overrides = settings.theme_overrides.get_or_insert_with(Default::default);
                for (k, v) in touched {
                    overrides.insert(k, v);
                }
            }
        }

        // Contrast guard: reject a combination where body text would be
        // unreadable on the background (WCAG ratio < 3:1).
        let preset_id = settings.theme_preset.as_deref().unwrap_or("default-dark");
        let (_, preset_bg, preset_text) = PRESET_CONTRAST
            .iter()
            .find(|(id, _, _)| *id == preset_id)
            .copied()
            .unwrap_or(PRESET_CONTRAST[0]);
        let overrides = settings.theme_overrides.clone().unwrap_or_default();
        let bg_hex = overrides.get("background").map(String::as_str).unwrap_or(preset_bg);
        let text_hex = overrides.get("text").map(String::as_str).unwrap_or(preset_text);
        let ratio = contrast_ratio(parse_hex(bg_hex).unwrap(), parse_hex(text_hex).unwrap());
        if ratio < 3.0 {
            return Err(format!(
                "refused: text {text_hex} on background {bg_hex} has contrast {ratio:.1}:1 (< 3:1), it would be unreadable; pick a lighter/darker color or use reset"
            ));
        }

        settings_service::save_app_settings(&self.db, &settings).await?;
        if let Some(app) = &self.app_handle {
            let _ = app.emit("app:settings_changed", ());
        }

        let mut summary = vec![format!("preset: {preset_id}")];
        if !overrides.is_empty() {
            let mut parts: Vec<String> = overrides
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            parts.sort();
            summary.push(format!("overrides: {}", parts.join(", ")));
        }
        Ok(format!(
            "Theme applied: {}. It is already live; if the user dislikes it, offer set_app_theme with reset=true.",
            summary.join("; ")
        ))
    }
}
