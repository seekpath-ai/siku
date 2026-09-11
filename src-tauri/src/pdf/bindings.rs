use std::sync::OnceLock;

use pdfium_render::prelude::{Pdfium, PdfiumError};

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
