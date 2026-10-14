use crate::core::models::FileEntry;

/// List directory contents
pub fn list_dir(path: &str, show_hidden: bool) -> Result<Vec<FileEntry>, String> {
    let dir = std::path::Path::new(path);
    if !dir.is_dir() { return Err("not a directory".into()); }

    let mut entries = Vec::new();
    let iter = std::fs::read_dir(dir).map_err(|e| format!("read dir: {e}"))?;

    for entry in iter.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !show_hidden && name.starts_with('.') { continue; }

        let metadata = entry.metadata().ok();
        let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified_at = metadata.and_then(|m| {
            m.modified().ok().map(|t| {
                chrono::DateTime::<chrono::Utc>::from(t)
                    .format("%Y-%m-%dT%H:%M:%SZ").to_string()
            })
        });

        entries.push(FileEntry {
            name: name.clone(),
            path: entry.path().to_string_lossy().to_string(),
            is_dir,
            size,
            modified_at,
            mime_type: if is_dir { None } else { mime_guess(&name) },
        });
    }

    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

/// Get info for a single file/directory
pub fn get_file_info(path: &str) -> Result<FileEntry, String> {
    let p = std::path::Path::new(path);
    if !p.exists() { return Err("not found".into()); }

    let metadata = p.metadata().map_err(|e| format!("metadata: {e}"))?;
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();

    Ok(FileEntry {
        is_dir: metadata.is_dir(),
        size: metadata.len(),
        modified_at: metadata.modified().ok().map(|t| {
            chrono::DateTime::<chrono::Utc>::from(t).format("%Y-%m-%dT%H:%M:%SZ").to_string()
        }),
        mime_type: if metadata.is_dir() { None } else { mime_guess(&name) },
        name,
        path: path.to_string(),
    })
}

/// Open a file with the system default application
pub fn open_in_system(path: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        crate::core::process::no_window(&mut std::process::Command::new("cmd"))
            .args(["/C", "start", "", path])
            .spawn()
            .map_err(|e| format!("failed to open: {e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("failed to open: {e}"))?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("failed to open: {e}"))?;
    }
    Ok(())
}

/// Reveal a file or directory in the system file manager, selecting the item when possible.
pub fn reveal_in_system(path: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Err(format!("path does not exist: {path}"));
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .args(["/select,", path])
            .spawn()
            .map_err(|e| format!("failed to reveal: {e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", path])
            .spawn()
            .map_err(|e| format!("failed to reveal: {e}"))?;
    }
    #[cfg(target_os = "linux")]
    {
        let target = if p.is_dir() {
            path.to_string()
        } else {
            p.parent()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string())
        };
        std::process::Command::new("xdg-open")
            .arg(target)
            .spawn()
            .map_err(|e| format!("failed to reveal: {e}"))?;
    }
    Ok(())
}

/// Show the Windows "Open with…" system dialog. Windows-only: macOS and
/// Linux have no equivalent native dialog, so the frontend hides this menu
/// item there (the stub errors out as a safety net).
#[cfg(target_os = "windows")]
pub fn open_with_dialog(path: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Err(format!("路径不存在：{path}"));
    }
    if p.is_dir() {
        return Err("目录不支持「打开方式」".into());
    }
    crate::core::process::no_window(&mut std::process::Command::new("rundll32"))
        .args(["shell32.dll,OpenAs_RunDLL", path])
        .spawn()
        .map_err(|e| format!("failed to open the Open-With dialog: {e}"))?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn open_with_dialog(path: &str) -> Result<(), String> {
    let _ = path;
    Err("当前平台没有系统「打开方式」对话框".into())
}

pub fn mime_guess(name: &str) -> Option<String> {
    let ext = std::path::Path::new(name).extension()?.to_str()?;
    match ext.to_lowercase().as_str() {
        "pdf" => Some("application/pdf".into()),
        "txt" | "md" | "rs" | "ts" | "tsx" | "js" | "json" | "yaml" | "toml" => Some("text/plain".into()),
        "png" => Some("image/png".into()), "jpg" | "jpeg" => Some("image/jpeg".into()),
        "gif" => Some("image/gif".into()), "svg" => Some("image/svg+xml".into()),
        "html" | "htm" => Some("text/html".into()), "css" => Some("text/css".into()),
        "mp4" => Some("video/mp4".into()), "mp3" => Some("audio/mpeg".into()),
        "zip" => Some("application/zip".into()), "gz" => Some("application/gzip".into()),
        _ => None,
    }
}
