use async_trait::async_trait;
use std::path::Path;

use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use super::path::{resolve_path, working_dir_from_args};

/// `mode` values advertised in the JSON Schema (see `Tool::param_enums`), so
/// the model reads the accepted strings instead of guessing them from prose.
const MODE_VALUES: &[&str] = &["overwrite", "append"];
const PARAM_ENUMS: &[(&str, &[&str])] = &[("mode", MODE_VALUES)];

/// What a call does to the target file. Omitting `mode` is deliberately NOT
/// "overwrite by default" — see `resolve_mode`.
#[derive(Clone, Copy)]
enum Mode {
    /// Create a new file; refuse to clobber an existing non-empty one.
    Create,
    Overwrite,
    Append,
}

/// Map the `mode` argument to an intent, failing closed.
///
/// Absent, `null` and blank all mean "unspecified" → `Create`: the destructive
/// interpretation must never be the default, so a model that forgot the
/// argument — or filled it with a placeholder to satisfy the schema — cannot
/// silently destroy a file. Only an explicit `overwrite` replaces content.
///
/// A non-string value is rejected loudly. The previous
/// `args["mode"].as_str().unwrap_or("overwrite")` turned `{"mode": null}` (and
/// any number/object/array) into a silent whole-file overwrite, which is
/// exactly the failure the surrounding comment claimed to prevent.
fn resolve_mode(args: &serde_json::Value) -> Result<Mode, String> {
    match args.get("mode") {
        None | Some(serde_json::Value::Null) => Ok(Mode::Create),
        Some(serde_json::Value::String(s)) => match s.trim() {
            "" => Ok(Mode::Create),
            "overwrite" => Ok(Mode::Overwrite),
            "append" => Ok(Mode::Append),
            other => Err(format!(
                "unknown mode '{other}' (expected overwrite or append; omit mode to create a new file)"
            )),
        },
        Some(other) => Err(format!(
            "mode must be a string, got {other} (expected overwrite or append; omit mode to create a new file)"
        )),
    }
}

/// Byte count for tool output. Bytes, not characters: the model cross-checks
/// sizes against `ls -l` / `wc -c`, and a CJK file would never match a
/// character count.
fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

fn line_hint(text: &str) -> String {
    let n = text.lines().count();
    format!("{n} line{}", if n == 1 { "" } else { "s" })
}

/// Whether the file already ends with a newline (true when we cannot tell —
/// then no separator is added, and the write itself will surface the error).
fn ends_with_newline(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut f) = std::fs::File::open(path) else {
        return true;
    };
    let Ok(len) = f.metadata().map(|m| m.len()) else {
        return true;
    };
    if len == 0 || f.seek(SeekFrom::End(-1)).is_err() {
        return true;
    }
    let mut last = [0u8; 1];
    f.read_exact(&mut last).is_ok() && last[0] == b'\n'
}

pub struct FileWriteTool;

impl FileWriteTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Create, overwrite, or append a text file. Relative paths land in the session's write directory; absolute paths may point anywhere, and writes outside that directory are highlighted to the user for approval. mode: overwrite or append — omit it (or pass \"\") to create a new file, which FAILS if the target already exists with content, so replacing a file is always explicit. mode=overwrite additionally requires the file to have been read (file_read) or written by you in this session. Creates parent directories automatically; the write itself is atomic. To modify an existing file, prefer file_edit (targeted replacement) over overwriting the whole file. Requires approval."
    }

    fn param_enums(&self) -> &'static [(&'static str, &'static [&'static str])] {
        PARAM_ENUMS
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "path".into(),
                param_type: "string".into(),
                description: "File path (absolute anywhere, or relative to the session's write directory)".into(),
                required: true,
            },
            ToolParameter {
                name: "content".into(),
                param_type: "string".into(),
                description: "Text content to write".into(),
                required: true,
            },
            ToolParameter {
                name: "mode".into(),
                param_type: "string".into(),
                description: "overwrite or append. Omit it to create a new file (fails if the file already exists with content).".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path = args["path"].as_str().ok_or("path required")?;
        // content is required: silently defaulting to "" would TRUNCATE an
        // existing file when the model omits the argument.
        let content = args["content"]
            .as_str()
            .ok_or("content required (refusing to write an empty file by default)")?;
        // Validate the mode BEFORE any side effect: a malformed mode must not
        // create directories on its way out, and must never fall through to a
        // silent overwrite (see `resolve_mode`).
        let mode = resolve_mode(&args)?;
        let wd = working_dir_from_args(&args);

        // Always absolute: with a working directory this is the canonical path
        // under it, and without one a relative path is refused outright, so
        // the result can never report a location the caller has to guess at.
        let resolved = resolve_path(wd.as_deref(), path)?;
        let existing = std::fs::metadata(&resolved).ok();
        let before_len = existing.as_ref().map(|m| m.len()).unwrap_or(0);

        // Fail-closed guard for the unspecified mode. A missing file, or an
        // existing empty one, carries no content to lose, so creating over it
        // stays frictionless; anything non-empty needs an explicit mode.
        if matches!(mode, Mode::Create) {
            match &existing {
                Some(meta) if meta.is_dir() => {
                    return Err(format!("{} is a directory, not a file", resolved.display()));
                }
                Some(meta) if meta.len() > 0 => {
                    return Err(format!(
                        "{} already exists ({} bytes) and no mode was given; refusing to overwrite it. \
                         Use file_edit for a targeted replacement, or pass mode=\"overwrite\" to replace the whole file.",
                        resolved.display(),
                        meta.len()
                    ));
                }
                _ => {}
            }
        }

        // The only destructive mode left. Require that the caller has seen the
        // file's current content — a read, or a write of its own — so a model
        // cannot replace content it never looked at, nor silently discard
        // changes made on disk after it read the file.
        if matches!(mode, Mode::Overwrite)
            && before_len > 0
            && !super::known_files::is_current(&resolved)
        {
            return Err(format!(
                "{} already exists ({}) and has not been read by file_read in this session, or has changed \
                 on disk since it was. Read it first, then retry mode=\"overwrite\" — or use file_edit for a \
                 targeted replacement.",
                resolved.display(),
                human_bytes(before_len)
            ));
        }

        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }

        match mode {
            Mode::Append => {
                // Never glue the first appended line onto the existing last
                // line: that silently corrupts scripts, configs and CSVs.
                let separator = if before_len > 0 && !ends_with_newline(&resolved) {
                    "\n"
                } else {
                    ""
                };
                // Whether the caller already knows this file's content; if it
                // appended blind, the snapshot must stay stale so a later
                // overwrite still demands a read.
                let was_current = existing.is_none() || super::known_files::is_current(&resolved);

                {
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&resolved)
                        .map_err(|e| format!("open failed: {e}"))?;
                    f.write_all(separator.as_bytes())
                        .and_then(|_| f.write_all(content.as_bytes()))
                        .map_err(|e| format!("write failed: {e}"))?;
                }

                if was_current {
                    super::known_files::remember(&resolved);
                }
                let after = std::fs::metadata(&resolved)
                    .map(|m| m.len())
                    .unwrap_or(before_len + content.len() as u64);

                let sep_note = if separator.is_empty() {
                    ""
                } else {
                    " (inserted a newline separator first)"
                };
                Ok(format!(
                    "Appended {} ({}) to {} (now {}){sep_note}",
                    human_bytes(content.len() as u64),
                    line_hint(content),
                    resolved.display(),
                    human_bytes(after)
                ))
            }
            Mode::Create | Mode::Overwrite => {
                super::write_atomically(&resolved, content)?;
                // The caller authored this content, so it is now known.
                super::known_files::remember(&resolved);
                let after = std::fs::metadata(&resolved)
                    .map(|m| m.len())
                    .unwrap_or(content.len() as u64);
                let size = format!("{}, {}", human_bytes(after), line_hint(content));

                match mode {
                    Mode::Create if existing.is_some() => Ok(format!(
                        "Created {} ({size}; the file existed but was empty)",
                        resolved.display()
                    )),
                    Mode::Create => Ok(format!("Created {} ({size})", resolved.display())),
                    _ => Ok(format!(
                        "Overwrote {} (was {}, now {size})",
                        resolved.display(),
                        human_bytes(before_len)
                    )),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::agent::tools::file_ops::FileReadTool;

    const KEEP: &str = "original content that must survive";

    /// Base args for a call inside `dir`; `mode` is added per test.
    fn args(dir: &Path, path: &str, content: &str) -> serde_json::Value {
        serde_json::json!({
            "path": path,
            "content": content,
            "_working_dir": dir.to_str().unwrap(),
        })
    }

    async fn write(
        dir: &Path,
        path: &str,
        content: &str,
        mode: Option<serde_json::Value>,
    ) -> Result<String, String> {
        let mut a = args(dir, path, content);
        if let Some(m) = mode {
            a["mode"] = m;
        }
        FileWriteTool::new().execute(a).await
    }

    /// Read through the real tool: this is what marks a file as seen.
    async fn read(dir: &Path, path: &str) {
        FileReadTool::new()
            .execute(serde_json::json!({
                "path": path,
                "_working_dir": dir.to_str().unwrap(),
            }))
            .await
            .expect("file_read must succeed");
    }

    fn sibling_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn creates_a_new_file_without_mode() {
        let dir = tempfile::tempdir().unwrap();
        let out = write(dir.path(), "sub/new.txt", "hello", None)
            .await
            .expect("creating a new file must not need a mode");
        assert!(out.starts_with("Created "), "{out}");
        assert!(out.contains("5 B"), "must report bytes: {out}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sub/new.txt")).unwrap(),
            "hello"
        );
    }

    /// The regression that motivated the rework: `mode` absent, `null` or
    /// blank must never destroy an existing file, and must leave it untouched
    /// on disk.
    #[tokio::test]
    async fn unspecified_mode_refuses_to_clobber_a_non_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("keep.txt");

        for mode in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!("")),
        ] {
            std::fs::write(&target, KEEP).unwrap();
            let err = write(dir.path(), "keep.txt", "replacement", mode.clone())
                .await
                .unwrap_err();
            assert!(err.contains("already exists"), "mode={mode:?}: {err}");
            assert!(err.contains("file_edit"), "error must guide: {err}");
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                KEEP,
                "mode={mode:?} must leave the file untouched"
            );
        }
    }

    /// `{"mode": 1}` / `{"mode": {}}` used to fall through to a silent
    /// overwrite; they must be rejected, with the file left alone.
    #[tokio::test]
    async fn non_string_mode_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("keep.txt");

        for mode in [
            serde_json::json!(1),
            serde_json::json!({}),
            serde_json::json!(["append"]),
        ] {
            std::fs::write(&target, KEEP).unwrap();
            let err = write(dir.path(), "keep.txt", "replacement", Some(mode.clone()))
                .await
                .unwrap_err();
            assert!(
                err.contains("mode must be a string"),
                "mode={mode:?}: {err}"
            );
            assert_eq!(std::fs::read_to_string(&target).unwrap(), KEEP);
        }
    }

    /// An unknown mode is rejected before any side effect — it must not leave
    /// the parent directories it would otherwise have created.
    #[tokio::test]
    async fn unknown_mode_is_rejected_without_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        let err = write(
            dir.path(),
            "nested/deep/file.txt",
            "x",
            Some(serde_json::json!("Append")),
        )
        .await
        .unwrap_err();
        assert!(err.contains("unknown mode 'Append'"), "{err}");
        assert!(
            !dir.path().join("nested").exists(),
            "a rejected call must not create directories"
        );
    }

    /// Whitespace around an explicit mode is tolerated, and the result names
    /// the operation and both sizes.
    #[tokio::test]
    async fn explicit_overwrite_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("keep.txt");
        std::fs::write(&target, KEEP).unwrap();
        read(dir.path(), "keep.txt").await;

        let out = write(
            dir.path(),
            "keep.txt",
            "new",
            Some(serde_json::json!(" overwrite ")),
        )
        .await
        .expect("explicit overwrite after a read must be allowed");

        assert!(out.starts_with("Overwrote "), "{out}");
        assert!(out.contains("was"), "must report the previous size: {out}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    }

    /// A whole-file overwrite needs the caller to have seen the current
    /// content: a blind one is refused, and a read unblocks it.
    #[tokio::test]
    async fn overwrite_requires_a_prior_read() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("keep.txt");
        std::fs::write(&target, KEEP).unwrap();

        let err = write(
            dir.path(),
            "keep.txt",
            "replacement",
            Some(serde_json::json!("overwrite")),
        )
        .await
        .unwrap_err();
        assert!(err.contains("file_read"), "{err}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), KEEP);

        read(dir.path(), "keep.txt").await;
        write(
            dir.path(),
            "keep.txt",
            "replacement",
            Some(serde_json::json!("overwrite")),
        )
        .await
        .expect("after a read the overwrite must go through");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "replacement");
    }

    /// A file the agent wrote itself is known content, so overwriting it does
    /// not need a read.
    #[tokio::test]
    async fn overwrite_of_its_own_write_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "own.txt", "first", None).await.unwrap();
        write(
            dir.path(),
            "own.txt",
            "second",
            Some(serde_json::json!("overwrite")),
        )
        .await
        .expect("the agent authored this file");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("own.txt")).unwrap(),
            "second"
        );
    }

    /// Content that changed on disk after the read makes the snapshot stale —
    /// the point of recording length + mtime instead of "was read once".
    #[tokio::test]
    async fn overwrite_refused_after_an_out_of_band_change() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("keep.txt");
        std::fs::write(&target, KEEP).unwrap();
        read(dir.path(), "keep.txt").await;

        // Deliberately a different length: a same-length rewrite would only
        // differ by mtime, which can land in the same timestamp tick (see the
        // precision note in `known_files`).
        std::fs::write(&target, "changed by something else entirely, and longer").unwrap();
        let err = write(
            dir.path(),
            "keep.txt",
            "replacement",
            Some(serde_json::json!("overwrite")),
        )
        .await
        .unwrap_err();
        assert!(err.contains("changed"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "changed by something else entirely, and longer"
        );

        read(dir.path(), "keep.txt").await;
        write(
            dir.path(),
            "keep.txt",
            "replacement",
            Some(serde_json::json!("overwrite")),
        )
        .await
        .expect("re-reading refreshes the snapshot");
    }

    /// Append never truncates, so a blind append stays allowed even though a
    /// blind overwrite is not.
    #[tokio::test]
    async fn append_is_allowed_without_a_prior_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("log.txt"), "line1\n").unwrap();
        write(
            dir.path(),
            "log.txt",
            "line2\n",
            Some(serde_json::json!("append")),
        )
        .await
        .expect("append destroys nothing");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("log.txt")).unwrap(),
            "line1\nline2\n"
        );
    }

    #[tokio::test]
    async fn append_adds_to_the_end_and_creates_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let first = write(
            dir.path(),
            "log.txt",
            "line1\n",
            Some(serde_json::json!("append")),
        )
        .await
        .unwrap();
        assert!(first.starts_with("Appended "), "{first}");
        write(
            dir.path(),
            "log.txt",
            "line2\n",
            Some(serde_json::json!("append")),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("log.txt")).unwrap(),
            "line1\nline2\n"
        );
    }

    /// Appending to a file whose last line has no newline must not glue the
    /// two lines together.
    #[tokio::test]
    async fn append_inserts_a_newline_separator_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("run.sh"), "echo one").unwrap();

        let out = write(
            dir.path(),
            "run.sh",
            "echo two\n",
            Some(serde_json::json!("append")),
        )
        .await
        .unwrap();

        assert!(out.contains("newline separator"), "{out}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run.sh")).unwrap(),
            "echo one\necho two\n"
        );
    }

    /// …but a file that already ends with a newline needs no separator.
    #[tokio::test]
    async fn append_keeps_the_content_verbatim_when_a_newline_is_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("run.sh"), "echo one\n").unwrap();

        let out = write(
            dir.path(),
            "run.sh",
            "echo two\n",
            Some(serde_json::json!("append")),
        )
        .await
        .unwrap();

        assert!(!out.contains("separator"), "{out}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run.sh")).unwrap(),
            "echo one\necho two\n"
        );
    }

    /// An empty existing file has nothing to lose, so the omitted mode stays
    /// frictionless there.
    #[tokio::test]
    async fn unspecified_mode_may_reuse_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("empty.txt"), "").unwrap();
        let out = write(dir.path(), "empty.txt", "filled", None)
            .await
            .expect("an empty file carries no content to protect");
        assert!(out.contains("was empty"), "{out}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("empty.txt")).unwrap(),
            "filled"
        );
    }

    #[tokio::test]
    async fn refuses_a_directory_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("adir")).unwrap();
        let err = write(dir.path(), "adir", "x", None).await.unwrap_err();
        assert!(err.contains("is a directory"), "{err}");
    }

    /// There is no sandbox: a path outside the write directory is written, not
    /// blocked. The approval card is what tells the user about it (see
    /// `ToolRegistry::writes_outside_write_dir`).
    #[tokio::test]
    async fn writes_outside_the_write_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("project");
        std::fs::create_dir_all(&nested).unwrap();

        let out = write(&nested, "../outside.txt", "x", None)
            .await
            .expect("writing outside the write directory is allowed");
        assert!(out.starts_with("Created "), "{out}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("outside.txt")).unwrap(),
            "x"
        );
    }

    /// An absolute path needs no write directory at all.
    #[tokio::test]
    async fn absolute_paths_need_no_write_dir() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("abs.txt");
        let out = FileWriteTool::new()
            .execute(serde_json::json!({
                "path": target.to_str().unwrap(),
                "content": "hi",
            }))
            .await
            .expect("an absolute path is self-contained");
        assert!(out.contains("Created "), "{out}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hi");
    }

    /// A relative path with no write directory has no base — refused rather
    /// than silently resolved against the process cwd.
    #[tokio::test]
    async fn refuses_relative_paths_without_a_working_dir() {
        let err = FileWriteTool::new()
            .execute(serde_json::json!({ "path": "x.txt", "content": "hi" }))
            .await
            .unwrap_err();
        assert!(err.contains("no write directory"), "{err}");
        assert!(err.contains("absolute path"), "{err}");
    }

    /// Result text reports the operation, bytes and lines — the numbers the
    /// model cross-checks against `ls -l` / `wc -l`.
    #[tokio::test]
    async fn result_reports_operation_bytes_and_lines() {
        let dir = tempfile::tempdir().unwrap();
        let created = write(dir.path(), "a.txt", "one\ntwo", None).await.unwrap();
        assert!(created.contains("2 lines"), "{created}");

        let appended = write(
            dir.path(),
            "a.txt",
            "\nthree",
            Some(serde_json::json!("append")),
        )
        .await
        .unwrap();
        assert!(appended.contains("Appended 6 B"), "{appended}");
        // 7 B of existing content + a 1 B separator (the file had no trailing
        // newline) + the 6 B appended.
        assert!(appended.contains("(now 14 B)"), "{appended}");
    }

    /// The replacement is atomic and keeps the original permission bits: a
    /// script the agent edits must stay executable.
    #[cfg(unix)]
    #[tokio::test]
    async fn overwrite_is_atomic_and_preserves_the_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("run.sh");
        std::fs::write(&target, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        read(dir.path(), "run.sh").await;

        write(
            dir.path(),
            "run.sh",
            "#!/bin/sh\necho hi\n",
            Some(serde_json::json!("overwrite")),
        )
        .await
        .unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "permission bits must survive the replacement");
        assert_eq!(
            sibling_names(dir.path()),
            vec!["run.sh"],
            "no temp file may be left behind"
        );
    }

    /// A failed replacement must leave the original file intact — the property
    /// `fs::write` does not have, because it truncates first.
    #[cfg(unix)]
    #[tokio::test]
    async fn failed_replacement_keeps_the_original_content() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("keep.txt");
        std::fs::write(&target, KEEP).unwrap();
        read(dir.path(), "keep.txt").await;

        // Make the directory unwritable so creating the temp file fails.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        // …unless permissions are ignored (e.g. running as root): then this
        // test cannot exercise the failure path at all.
        if std::fs::write(dir.path().join("probe"), "x").is_ok() {
            let _ = std::fs::remove_file(dir.path().join("probe"));
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        let err = write(
            dir.path(),
            "keep.txt",
            "replacement that must not land",
            Some(serde_json::json!("overwrite")),
        )
        .await
        .unwrap_err();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(err.contains("write failed"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            KEEP,
            "the original content must survive a failed replacement"
        );
    }

    #[test]
    fn mode_argument_is_advertised_as_an_enum_but_stays_optional() {
        assert_eq!(PARAM_ENUMS.len(), 1);
        assert_eq!(PARAM_ENUMS[0].0, "mode");
        assert_eq!(PARAM_ENUMS[0].1, MODE_VALUES);

        let params = FileWriteTool::new().parameters();
        let mode = params
            .iter()
            .find(|p| p.name == "mode")
            .expect("mode param");
        assert!(!mode.required, "the omitted case is defined by the tool");
    }

    #[test]
    fn byte_rendering_is_readable() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
    }
}
