use std::path::Path;

use crate::core::error::{Result, SikuError};

/// Render the first page of a PDF to a PNG thumbnail.
/// The thumbnail is saved to the given output path.
pub fn render_thumbnail(pdf_path: &Path, output_path: &Path) -> Result<()> {
    let pdfium = crate::pdf::bindings::pdfium()
        .map_err(SikuError::PdfParse)?;

    let doc = pdfium
        .load_pdf_from_file(pdf_path, None)
        .map_err(|e| SikuError::PdfParse(format!("failed to load PDF: {}", e)))?;

    let pages = doc.pages();
    if pages.is_empty() {
        return Err(SikuError::PdfParse("PDF has no pages".to_string()));
    }

    let page = pages.get(0).map_err(|e| {
        SikuError::PdfParse(format!("failed to get first page: {}", e))
    })?;

    // Render at 150 DPI for thumbnails
    let width = page.width();
    let height = page.height();

    // Scale to ~200px wide thumbnail, maintaining aspect ratio
    let target_width: i32 = 200;
    let scale = target_width as f32 / width.value as f32;
    let target_height: i32 = (height.value as f32 * scale) as i32;

    use pdfium_render::prelude::*;
    let render_config = PdfRenderConfig::new()
        .set_target_width(target_width)
        .set_target_height(target_height)
        .set_image_smoothing(true);

    let bitmap = page
        .render_with_config(&render_config)
        .map_err(|e| SikuError::PdfParse(format!("failed to render page: {}", e)))?;

    let image = bitmap
        .as_image()
        .map_err(|e| SikuError::PdfParse(format!("failed to convert bitmap: {}", e)))?;

    // Ensure parent directory exists
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    image
        .save(output_path)
        .map_err(|e| SikuError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;

    Ok(())
}
