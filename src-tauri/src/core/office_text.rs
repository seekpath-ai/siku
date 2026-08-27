//! Text extraction from OOXML office documents (.docx/.xlsx/.pptx).
//!
//! These formats are zip archives of XML, so extraction reuses the project's
//! existing `zip` + `quick-xml` dependencies — no new crate. Legacy binary
//! formats (.doc/.xls/.ppt, OLE2) are NOT supported here; they fall back to
//! "open with system application" in the UI.
//!
//! Scope is text extraction for preview / search / agent context — layout,
//! images and styling are intentionally dropped.

use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::{Cursor, Read};

/// Cap on extracted text, matching the plain-text preview cap.
pub const MAX_EXTRACTED: u64 = 2 * 1024 * 1024;
/// Cap on the raw office file size we attempt to parse (zip bombs / huge
/// decks just fail preview instead of eating memory).
const MAX_SOURCE: u64 = 256 * 1024 * 1024;

/// Whether the filename carries a supported OOXML extension.
pub fn is_office_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".docx") || lower.ends_with(".xlsx") || lower.ends_with(".pptx")
}

/// Extract plain text from an office document. `name` supplies the extension.
/// Returns the text and a truncation flag (output hit MAX_EXTRACTED).
pub fn extract_text(bytes: &[u8], name: &str) -> Result<(String, bool), String> {
    if bytes.len() as u64 > MAX_SOURCE {
        return Err("file too large to parse".to_string());
    }
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("invalid office file: {e}"))?;
    let lower = name.to_ascii_lowercase();
    let mut out = String::new();
    if lower.ends_with(".docx") {
        extract_docx(&mut archive, &mut out)?;
    } else if lower.ends_with(".xlsx") {
        extract_xlsx(&mut archive, &mut out)?;
    } else if lower.ends_with(".pptx") {
        extract_pptx(&mut archive, &mut out)?;
    } else {
        return Err("unsupported office format".to_string());
    }
    let truncated = out.len() as u64 > MAX_EXTRACTED;
    if truncated {
        // Cut on a char boundary.
        let mut end = MAX_EXTRACTED as usize;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    Ok((out, truncated))
}

/// Local-name match so documents with unusual namespace prefixes still parse.
fn local_is(name: quick_xml::name::QName, local: &[u8]) -> bool {
    let raw = name.as_ref();
    raw == local || (raw.len() > local.len() && raw.ends_with(local) && raw[raw.len() - local.len() - 1] == b':')
}

/// Unescape a text node (Office XML is always UTF-8).
fn text_content(t: &quick_xml::events::BytesText) -> Option<String> {
    t.unescape().ok().map(|c| c.into_owned())
}

fn push_capped(out: &mut String, s: &str) {
    if (out.len() as u64) < MAX_EXTRACTED {
        out.push_str(s);
    }
}

/// docx: word/document.xml — w:t text, paragraph/tab/break structure.
fn extract_docx<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    out: &mut String,
) -> Result<(), String> {
    let xml = read_entry(archive, "word/document.xml")?;
    let mut reader = Reader::from_reader(xml.as_slice());
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if local_is(e.name(), b"t") => {
                if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                    if let Some(s) = text_content(&t) {
                        push_capped(out, &s);
                    }
                }
            }
            Ok(Event::End(e)) if local_is(e.name(), b"p") => push_capped(out, "\n"),
            Ok(Event::Empty(e)) if local_is(e.name(), b"br") => push_capped(out, "\n"),
            Ok(Event::Empty(e)) if local_is(e.name(), b"tab") => push_capped(out, "\t"),
            Ok(Event::Eof) => break,
            Err(_) => break, // malformed XML: return what we have
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

/// xlsx: shared strings + every worksheet, rows joined by newline, cells by
/// tab. Sheet names are not resolved (workbook.xml rels) — sheet order is
/// file order, which matches what the user sees in practice.
fn extract_xlsx<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    out: &mut String,
) -> Result<(), String> {
    let shared = read_entry(archive, "xl/sharedStrings.xml")
        .map(|xml| parse_shared_strings(&xml))
        .unwrap_or_default();

    let mut sheets: Vec<String> = archive
        .file_names()
        .filter(|n| n.starts_with("xl/worksheets/") && n.ends_with(".xml"))
        .map(|s| s.to_string())
        .collect();
    sheets.sort();

    for sheet in sheets {
        let xml = read_entry(archive, &sheet)?;
        extract_sheet(&xml, &shared, out);
    }
    Ok(())
}

fn parse_shared_strings(xml: &[u8]) -> Vec<String> {
    let mut strings = Vec::new();
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut in_si = false;
    let mut current = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if local_is(e.name(), b"si") => {
                in_si = true;
                current.clear();
            }
            Ok(Event::End(e)) if local_is(e.name(), b"si") => {
                in_si = false;
                strings.push(std::mem::take(&mut current));
            }
            // Concatenate every run's text inside the si (rich text splits).
            Ok(Event::Start(e)) if in_si && local_is(e.name(), b"t") => {
                if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                    if let Some(s) = text_content(&t) {
                        current.push_str(&s);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    strings
}

fn extract_sheet(xml: &[u8], shared: &[String], out: &mut String) {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut cell_is_shared = false;
    let mut cell_text = String::new();
    let mut in_cell = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if local_is(e.name(), b"c") => {
                in_cell = true;
                cell_text.clear();
                cell_is_shared = e
                    .attributes()
                    .flatten()
                    .any(|a| a.key.as_ref() == b"t" && &*a.value == b"s");
            }
            Ok(Event::End(e)) if local_is(e.name(), b"c") => {
                in_cell = false;
                if cell_is_shared {
                    if let Ok(idx) = cell_text.trim().parse::<usize>() {
                        if let Some(s) = shared.get(idx) {
                            push_capped(out, s);
                        }
                    }
                } else {
                    push_capped(out, cell_text.trim());
                }
                push_capped(out, "\t");
            }
            Ok(Event::End(e)) if local_is(e.name(), b"row") => {
                if out.ends_with('\t') {
                    out.pop();
                }
                push_capped(out, "\n");
            }
            // v = value, t = inline string text.
            Ok(Event::Start(e)) if in_cell && (local_is(e.name(), b"v") || local_is(e.name(), b"t")) => {
                if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                    if let Some(s) = text_content(&t) {
                        cell_text.push_str(&s);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
}

/// pptx: ppt/slides/slideN.xml in numeric order — every a:t run, one line
/// per slide separator so multi-deck text stays navigable.
fn extract_pptx<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    out: &mut String,
) -> Result<(), String> {
    let mut slides: Vec<(u32, String)> = archive
        .file_names()
        .filter_map(|n| {
            let num = n
                .strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?
                .parse::<u32>()
                .ok()?;
            Some((num, n.to_string()))
        })
        .collect();
    slides.sort_by_key(|(num, _)| *num);

    for (num, name) in slides {
        let xml = read_entry(archive, &name)?;
        push_capped(out, &format!("—— 第 {num} 页 ——\n"));
        let mut reader = Reader::from_reader(xml.as_slice());
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) if local_is(e.name(), b"t") => {
                    if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                        if let Some(s) = text_content(&t) {
                            push_capped(out, &s);
                        }
                    }
                }
                // a:p ends a paragraph inside a shape.
                Ok(Event::End(e)) if local_is(e.name(), b"p") => push_capped(out, "\n"),
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        push_capped(out, "\n");
    }
    Ok(())
}

fn read_entry<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Vec<u8>, String> {
    let mut entry = archive
        .by_name(name)
        .map_err(|_| format!("missing {name}"))?;
    let mut buf = Vec::new();
    entry
        .read_to_end(&mut buf)
        .map_err(|e| format!("read {name}: {e}"))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, content) in entries {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(content.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn docx_extracts_paragraph_text() {
        let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body><w:p><w:r><w:t>你好</w:t></w:r><w:r><w:t>世界</w:t></w:r></w:p><w:p><w:r><w:t>第二段</w:t></w:r></w:p></w:body>
</w:document>"#;
        let zip = build_zip(&[("word/document.xml", doc)]);
        let (text, truncated) = extract_text(&zip, "a.docx").unwrap();
        assert_eq!(text, "你好世界\n第二段\n");
        assert!(!truncated);
    }

    #[test]
    fn xlsx_resolves_shared_strings() {
        let shared = r#"<?xml version="1.0"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<si><t>名称</t></si><si><r><t>思</t></r><r><t>库</t></r></si></sst>"#;
        let sheet = r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData><row><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1"><v>42</v></c></row></sheetData>
</worksheet>"#;
        let zip = build_zip(&[("xl/sharedStrings.xml", shared), ("xl/worksheets/sheet1.xml", sheet)]);
        let (text, _) = extract_text(&zip, "a.xlsx").unwrap();
        assert_eq!(text, "名称\t思库\t42\n");
    }

    #[test]
    fn pptx_extracts_slides_in_numeric_order() {
        let slide = |t: &str| format!(
            r#"<?xml version="1.0"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
<p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
        );
        let zip = build_zip(&[
            ("ppt/slides/slide10.xml", &slide("第十页")),
            ("ppt/slides/slide2.xml", &slide("第二页")),
        ]);
        let (text, _) = extract_text(&zip, "a.pptx").unwrap();
        let pos2 = text.find("第二页").unwrap();
        let pos10 = text.find("第十页").unwrap();
        assert!(pos2 < pos10);
        assert!(text.contains("—— 第 2 页 ——"));
    }

    #[test]
    fn rejects_non_zip() {
        assert!(extract_text(b"not a zip", "a.docx").is_err());
    }

    #[test]
    fn is_office_name_matches_ooxml_only() {
        assert!(is_office_name("报告.DOCX"));
        assert!(is_office_name("data.xlsx"));
        assert!(is_office_name("slides.pptx"));
        assert!(!is_office_name("old.doc"));
        assert!(!is_office_name("plain.txt"));
    }
}
