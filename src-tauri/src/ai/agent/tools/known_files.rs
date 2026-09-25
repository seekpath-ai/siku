//! Which file contents an agent has actually seen.
//!
//! `file_write` in `overwrite` mode replaces a whole file, so before doing it
//! the tool asks here whether the caller has seen the file's *current* state —
//! either by reading it (`file_read`) or by writing it itself. Without that
//! gate a model can destroy a file it never looked at, and, just as bad, it can
//! overwrite a file that changed on disk after it read it (a build step, the
//! user's editor, an earlier turn of a parallel agent).
//!
//! State lives in a process-wide map keyed by canonical path, holding the
//! length + mtime observed when the file was last read or written. That is
//! deliberately cheap: the write path compares metadata only, and a stale
//! entry (file touched by something else) simply forces one `file_read` before
//! the overwrite. Keys are canonicalized so `a/../b` and `b` are the same file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use once_cell::sync::Lazy;

/// Enough to notice an out-of-band change without re-reading the file.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Snapshot {
    len: u64,
    mtime: Option<SystemTime>,
}

impl Snapshot {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        Some(Self {
            len: meta.len(),
            mtime: meta.modified().ok(),
        })
    }
}

static KNOWN: Lazy<Mutex<HashMap<PathBuf, Snapshot>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Canonical when possible (so different spellings of one path share an
/// entry), the raw path otherwise. Only existing paths are ever recorded.
fn key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Record that the caller has seen the current content of `path`: it read the
/// file, or wrote it (in which case it knows what it put there).
pub fn remember(path: &Path) {
    if let Some(snapshot) = Snapshot::of(path) {
        if let Ok(mut known) = KNOWN.lock() {
            known.insert(key(path), snapshot);
        }
    }
}

/// Whether the caller has seen `path` in exactly its current state. `false`
/// also covers "the file does not exist" and "it changed since it was seen".
pub fn is_current(path: &Path) -> bool {
    let Some(now) = Snapshot::of(path) else {
        return false;
    };
    KNOWN
        .lock()
        .map(|known| known.get(&key(path)).copied() == Some(now))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_file_is_not_current_until_seen() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "one").unwrap();

        assert!(!is_current(&file));
        remember(&file);
        assert!(is_current(&file));
    }

    /// An out-of-band change invalidates the record — that is the whole point
    /// of storing length + mtime instead of just "was read".
    #[test]
    fn changing_the_file_on_disk_invalidates_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("b.txt");
        std::fs::write(&file, "one").unwrap();
        remember(&file);
        assert!(is_current(&file));

        std::fs::write(&file, "a much longer content").unwrap();
        assert!(!is_current(&file));

        remember(&file);
        assert!(is_current(&file));
    }

    #[test]
    fn different_spellings_of_one_path_share_the_record() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let file = dir.path().join("sub/c.txt");
        std::fs::write(&file, "one").unwrap();

        remember(&dir.path().join("sub").join("..").join("sub/c.txt"));
        assert!(is_current(&file));
    }

    #[test]
    fn missing_and_directory_targets_are_never_current() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_current(&dir.path().join("nope.txt")));
        assert!(!is_current(dir.path()));
    }
}
