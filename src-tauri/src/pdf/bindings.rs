use std::sync::{Mutex, MutexGuard, OnceLock};

use pdfium_render::prelude::{Pdfium, PdfiumError};

/// Serializes every pdfium call in the process.
///
/// pdfium keeps process-global state (font and loader caches) and is not safe
/// to drive from several threads at once: two extractions running concurrently
/// corrupt the heap. That shows up as SIGSEGV or `free(): invalid pointer`,
/// reproduced by running the corpus regression tests in parallel — which is
/// exactly what importing two PDFs at once, or rendering a thumbnail while an
/// import is running, does in the app.
///
/// The guard must cover the whole load + read, not just the binding: the
/// corruption comes from concurrent document access.
///
/// NEVER call a function that takes this guard from inside a critical section —
/// `std::sync::Mutex` is not reentrant.
static PDFIUM_LOCK: Mutex<()> = Mutex::new(());

/// Hold this for the entire duration of any pdfium work.
pub fn pdfium_guard() -> MutexGuard<'static, ()> {
    PDFIUM_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Lazily bound, process-wide Pdfium instance.
///
/// pdfium-render only allows binding the dynamic library ONCE per process
/// (`bind_to_library` fails with `PdfiumLibraryBindingsAlreadyInitialized` on
/// the second call). Both the text extractor and the thumbnail renderer must
/// share this single instance instead of binding independently.
///
/// Lookup order — the bare library name alone only searches OS loader paths,
/// which never find our bundled copy:
/// 1. `SIKU_PDFIUM_PATH` env override (exact file path);
/// 2. next to the executable (installed app: pdfium.dll is bundled there as a
///    Tauri resource);
/// 3. `<cwd>/bin/<lib>` (cargo dev/test run with src-tauri as CWD, where
///    bin/pdfium.dll / bin/libpdfium.so live);
/// 4. the OS default search (system-wide installs).
pub fn pdfium() -> Result<&'static Pdfium, String> {
    static INSTANCE: OnceLock<Result<Pdfium, PdfiumError>> = OnceLock::new();
    INSTANCE
        .get_or_init(|| {
            let lib_name = Pdfium::pdfium_platform_library_name();
            let mut candidates: Vec<std::path::PathBuf> = Vec::new();
            if let Ok(p) = std::env::var("SIKU_PDFIUM_PATH") {
                candidates.push(p.into());
            }
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    candidates.push(dir.join(&lib_name));
                }
            }
            if let Ok(cwd) = std::env::current_dir() {
                candidates.push(cwd.join("bin").join(&lib_name));
            }
            for path in &candidates {
                if path.exists() {
                    if let Ok(b) = Pdfium::bind_to_library(path) {
                        return Ok(Pdfium::new(b));
                    }
                }
            }
            let bindings = Pdfium::bind_to_library(lib_name)?;
            Ok(Pdfium::new(bindings))
        })
        .as_ref()
        .map_err(|e| format!("failed to bind pdfium library: {e}"))
}
