use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Resolve a tool path against the sandbox root (`working_dir`).
///
/// - `working_dir = None` → full disk access; the path is used as-is.
/// - `working_dir = Some(base)` → relative paths resolve under `base`;
///   absolute paths are canonicalized and must stay inside `base`.
///
/// Non-existent targets (e.g. a file to create) are handled by canonicalizing
/// the deepest existing ancestor and re-appending the missing parts.
pub fn resolve_path(base: Option<&str>, path: &str) -> Result<PathBuf, String> {
    let raw = Path::new(path);
    let Some(base) = base else {
        return Ok(raw.to_path_buf());
    };
    if base.trim().is_empty() {
        return Ok(raw.to_path_buf());
    }

    let base_path = Path::new(base);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        base_path.join(raw)
    };

    let canon_base = base_path
        .canonicalize()
        .map_err(|e| format!("working directory error: {e}"))?;

    // Canonicalize the deepest existing ancestor, then re-append missing parts.
    let mut missing: Vec<OsString> = Vec::new();
    let mut probe = joined.as_path();
    let canon = loop {
        match probe.canonicalize() {
            Ok(c) => break c,
            Err(_) => match probe.file_name() {
                Some(n) => {
                    missing.push(n.to_os_string());
                    probe = probe.parent().unwrap_or(probe);
                }
                None => return Err(format!("path error: {path}")),
            },
        }
    };

    let mut result = canon;
    for part in missing.iter().rev() {
        result.push(part);
    }

    if result.starts_with(&canon_base) {
        Ok(result)
    } else {
        Err(format!("path outside working directory: {path}"))
    }
}

/// Read `working_dir` injected into tool args by the registry.
pub fn working_dir_from_args(args: &serde_json::Value) -> Option<String> {
    args.get("_working_dir")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}
