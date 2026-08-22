use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::Manager;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::core::models::AppSettings;

struct CurrentFile {
    file: File,
    path: PathBuf,
    size: u64,
}

/// Size-based rolling file appender.
///
/// Active log is written to `{prefix}.log`. When it exceeds `max_size`, it is
/// renamed to `{prefix}.{timestamp}.log` so the `.log` extension stays at the
/// end for Windows file associations.
pub struct SizeRollingFileAppender {
    dir: PathBuf,
    prefix: String,
    max_size: u64,
    max_files: usize,
    current: Mutex<CurrentFile>,
}

impl SizeRollingFileAppender {
    pub fn new(
        dir: impl AsRef<Path>,
        prefix: impl Into<String>,
        max_size: u64,
        max_files: usize,
    ) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let prefix = prefix.into();
        fs::create_dir_all(&dir)?;
        let current = Self::open_current(&dir, &prefix)?;
        Ok(Self {
            dir,
            prefix,
            max_size,
            max_files,
            current: Mutex::new(current),
        })
    }

    fn open_current(dir: &Path, prefix: &str) -> io::Result<CurrentFile> {
        let path = dir.join(format!("{}.log", prefix));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let size = file.metadata()?.len();
        Ok(CurrentFile { file, path, size })
    }

    fn rotate(&self) -> io::Result<()> {
        let timestamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string();
        let rotated_name = format!("{}.{}{}", self.prefix, timestamp, ".log");
        let rotated_path = self.dir.join(rotated_name);

        {
            let mut current = self.current.lock().unwrap();
            current.file.flush()?;
            fs::rename(&current.path, &rotated_path)?;
            *current = Self::open_current(&self.dir, &self.prefix)?;
        }

        if self.max_files > 0 {
            self.cleanup_old_files()?;
        }

        Ok(())
    }

    fn cleanup_old_files(&self) -> io::Result<()> {
        let mut files: Vec<_> = fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let current = format!("{}.log", self.prefix);
                name.starts_with(&format!("{}.", self.prefix))
                    && name.ends_with(".log")
                    && name != current
            })
            .collect();

        files.sort_by_key(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH)
        });

        if files.len() > self.max_files {
            for file in files.iter().take(files.len() - self.max_files) {
                let _ = fs::remove_file(file.path());
            }
        }

        Ok(())
    }
}

impl Write for SizeRollingFileAppender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.max_size > 0 && !buf.is_empty() {
            let should_rotate = {
                let current = self.current.lock().unwrap();
                current.size.saturating_add(buf.len() as u64) > self.max_size
                    && current.size > 0
            };
            if should_rotate {
                self.rotate()?;
            }
        }

        let mut current = self.current.lock().unwrap();
        let written = current.file.write(buf)?;
        current.size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut current = self.current.lock().unwrap();
        current.file.flush()
    }
}

pub fn init(app_handle: &tauri::AppHandle, settings: &AppSettings) -> anyhow::Result<()> {
    let app_data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| anyhow::anyhow!("failed to get app data dir: {}", e))?;

    let log_dir = app_data_dir.join("logs");

    let max_size = (settings.log_max_size_mb.max(1) as u64) * 1024 * 1024;
    let max_files = settings.log_max_files.max(0) as usize;
    let file_appender = SizeRollingFileAppender::new(&log_dir, "siku", max_size, max_files)?;
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    std::mem::forget(_guard);

    // On Windows, set console code page to UTF-8 to avoid garbled Chinese text
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        unsafe {
            // SetConsoleOutputCP(65001) = UTF-8
            extern "system" {
                fn SetConsoleOutputCP(code_page: u32) -> i32;
            }
            SetConsoleOutputCP(65001);
        }
    }

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(non_blocking);

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(std::io::stderr);

    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with(file_layer)
        .with(stderr_layer)
        .init();

    Ok(())
}
