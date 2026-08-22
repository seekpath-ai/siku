use std::sync::OnceLock;

use pdfium_render::prelude::{Pdfium, PdfiumError};

/// Lazily bound, process-wide Pdfium instance.
///
/// pdfium-render only allows binding the dynamic library ONCE per process
/// (`bind_to_library` fails with `PdfiumLibraryBindingsAlreadyInitialized` on
/// the second call). Both the text extractor and the thumbnail renderer must
/// share this single instance instead of binding independently.
pub fn pdfium() -> Result<&'static Pdfium, String> {
    static INSTANCE: OnceLock<Result<Pdfium, PdfiumError>> = OnceLock::new();
    INSTANCE
        .get_or_init(|| {
            let bindings =
                Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name())?;
            Ok(Pdfium::new(bindings))
        })
        .as_ref()
        .map_err(|e| format!("failed to bind pdfium library: {e}"))
}
