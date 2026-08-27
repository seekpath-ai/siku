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

#[cfg(test)]
mod tests {
    use super::resolve_path;

    /// Sandboxed resolution must reject every `..` escape shape — whether the
    /// target exists, only the parent exists, or nothing exists.
    #[test]
    fn rejects_dotdot_escape_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("sandbox");
        std::fs::create_dir_all(base.join("a")).unwrap();
        let base_s = base.to_str().unwrap();

        // Missing tail with .. that climbs out of the sandbox.
        assert!(resolve_path(Some(base_s), "a/../../b").is_err());
        assert!(resolve_path(Some(base_s), "nonexist/../../outside.txt").is_err());
        assert!(resolve_path(Some(base_s), "../escape.txt").is_err());
        // Existing file reached via ...
        let outside = dir.path().join("secret.txt");
        std::fs::write(&outside, b"x").unwrap();
        assert!(resolve_path(Some(base_s), "a/../../secret.txt").is_err());
        // Absolute path outside the sandbox.
        assert!(resolve_path(Some(base_s), outside.to_str().unwrap()).is_err());
    }

    #[test]
    fn allows_legitimate_paths() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("sandbox");
        std::fs::create_dir_all(base.join("a")).unwrap();
        std::fs::write(base.join("a/f.txt"), b"x").unwrap();
        let base_s = base.to_str().unwrap();

        // Existing file, plain and via an internal `a/../` detour.
        assert!(resolve_path(Some(base_s), "a/f.txt").is_ok());
        assert!(resolve_path(Some(base_s), "a/../a/f.txt").is_ok());
        // New file to create inside the sandbox.
        let new_file = resolve_path(Some(base_s), "a/new.txt").unwrap();
        assert!(new_file.starts_with(base.canonicalize().unwrap()));
        // Deeply nested new file.
        assert!(resolve_path(Some(base_s), "x/y/z.txt").is_ok());
    }
}

