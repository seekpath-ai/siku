//! Path resolution for the file tools: a *default write directory*, not a
//! sandbox.
//!
//! The containment check that used to live here was removed on purpose. `bash`
//! is unconfined by design (it only gets a cwd; `cd`, redirects and scripts go
//! wherever they like), so a file-tool sandbox was never a boundary — it only
//! produced `outside working directory` errors that cost the model a turn
//! whenever it legitimately needed a temp dir, another repository, or the
//! user's Downloads. Approval is the real gate; being honest about that beats
//! pretending.
//!
//! What survives is the useful half: a deterministic base for relative paths,
//! so `notes/x.md` lands somewhere meaningful instead of in the process cwd
//! (on Windows, `C:\Windows\System32` for a GUI-launched app).
//!
//! - absolute path → used as given, anywhere on disk
//! - relative path → joined onto the session's write directory
//! - relative path with no write directory → refused with an actionable error
//!   (the app always supplies one — a project folder or the session workspace —
//!   so this only fires for a misconfigured caller, and it must never fall back
//!   to the process cwd)
//!
//! The result is canonicalized as far as the path exists (best effort), so
//! callers report an absolute path with `..` and symlinks already resolved.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub fn resolve_path(base: Option<&str>, path: &str) -> Result<PathBuf, String> {
    let raw = Path::new(path);
    let joined = match base.filter(|b| !b.trim().is_empty()) {
        Some(base) if !raw.is_absolute() => Path::new(base).join(raw),
        Some(_) => raw.to_path_buf(),
        None if raw.is_absolute() => raw.to_path_buf(),
        None => return Err(no_working_dir_error(path)),
    };
    Ok(canonical_best_effort(&joined))
}

/// Canonicalize the deepest existing ancestor and re-append the parts that do
/// not exist yet (the target of a create, typically). Never fails: when even
/// the root cannot be resolved the joined path is returned unchanged.
fn canonical_best_effort(path: &Path) -> PathBuf {
    let mut missing: Vec<OsString> = Vec::new();
    let mut probe = path;
    let mut canonical = None;
    loop {
        match probe.canonicalize() {
            Ok(c) => {
                canonical = Some(c);
                break;
            }
            Err(_) => match probe.file_name() {
                Some(name) => {
                    missing.push(name.to_os_string());
                    match probe.parent() {
                        Some(parent) => probe = parent,
                        None => break,
                    }
                }
                None => break,
            },
        }
    }

    let Some(mut result) = canonical else {
        return path.to_path_buf();
    };
    for part in missing.iter().rev() {
        result.push(part);
    }
    result
}

/// Read the write directory injected into tool args by the registry.
pub fn working_dir_from_args(args: &serde_json::Value) -> Option<String> {
    args.get("_working_dir")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// Actionable refusal for a relative path with no write directory. The process
/// cwd is named because seeing `C:\Windows\System32` in the message is what
/// makes the rule obvious to the caller (model or human).
fn no_working_dir_error(path: &str) -> String {
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    format!(
        "relative path '{path}' has no base: no write directory is configured for this session, and the \
         process cwd ({cwd}) is not a usable base for it. Pass an absolute path, or set a write \
         directory for the session."
    )
}

#[cfg(test)]
mod tests {
    use super::resolve_path;

    fn write_dir_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("write-dir");
        std::fs::create_dir_all(base.join("a")).unwrap();
        std::fs::write(base.join("a/f.txt"), b"x").unwrap();
        (dir, base)
    }

    #[test]
    fn relative_paths_resolve_under_the_base() {
        let (_dir, base) = write_dir_fixture();
        let base_s = base.to_str().unwrap();

        // Existing file, plain and via an internal `a/../` detour.
        assert_eq!(
            resolve_path(Some(base_s), "a/f.txt").unwrap(),
            base.join("a/f.txt").canonicalize().unwrap()
        );
        assert_eq!(
            resolve_path(Some(base_s), "a/../a/f.txt").unwrap(),
            base.join("a/f.txt").canonicalize().unwrap()
        );
        // A file to create: absolute, and still under the base.
        let new_file = resolve_path(Some(base_s), "x/y/z.txt").unwrap();
        assert!(new_file.is_absolute());
        assert!(new_file.starts_with(base.canonicalize().unwrap()));
    }

    /// The containment check is gone: `..` and absolute paths may leave the
    /// write directory, and they resolve to the truth rather than erroring.
    #[test]
    fn paths_outside_the_base_resolve_instead_of_failing() {
        let (dir, base) = write_dir_fixture();
        let base_s = base.to_str().unwrap();
        let outside = dir.path().join("secret.txt");
        std::fs::write(&outside, b"x").unwrap();
        let outside_canon = outside.canonicalize().unwrap();

        assert_eq!(
            resolve_path(Some(base_s), "a/../../secret.txt").unwrap(),
            outside_canon
        );
        assert_eq!(
            resolve_path(Some(base_s), outside.to_str().unwrap()).unwrap(),
            outside_canon
        );
        // Non-existent escape target: parent canonicalized, tail re-appended.
        assert_eq!(
            resolve_path(Some(base_s), "../escape.txt").unwrap(),
            dir.path().canonicalize().unwrap().join("escape.txt")
        );
    }

    /// No write directory means "no default", not "no file access".
    #[test]
    fn absolute_paths_are_used_as_given_without_a_write_dir() {
        let abs = std::env::temp_dir();
        assert_eq!(
            resolve_path(None, abs.to_str().unwrap()).unwrap(),
            abs.canonicalize().unwrap()
        );
        assert_eq!(
            resolve_path(Some("   "), abs.to_str().unwrap()).unwrap(),
            abs.canonicalize().unwrap()
        );
    }

    /// The process cwd is not a usable base for a relative path, so it must be
    /// refused (with the cwd named) instead of silently resolving there.
    #[test]
    fn relative_paths_are_refused_without_a_write_dir() {
        for base in [None, Some(""), Some("  ")] {
            for path in ["x.txt", "./x.txt", "sub/x.txt", ".", ".."] {
                let err =
                    resolve_path(base, path).expect_err("a relative path needs a write directory");
                assert!(err.contains("no write directory"), "{err}");
                assert!(err.contains("absolute path"), "must say what to do: {err}");
                assert!(err.contains(path), "must name the path: {err}");
            }
        }
    }

    /// A write directory that does not exist (moved vault, fresh sync) is not
    /// an error any more: the path resolves under the deepest existing
    /// ancestor, and the writer creates the rest.
    #[test]
    fn a_missing_base_still_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone").join("sub");
        let resolved = resolve_path(Some(missing.to_str().unwrap()), "x.txt").unwrap();
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("gone/sub/x.txt"), "{resolved:?}");
    }
}
