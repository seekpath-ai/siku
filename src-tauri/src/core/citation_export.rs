//! Citation export: BibTeX / RIS / CSL-JSON serialization for papers.
//! The inverse of the existing BibTeX *parser* — lets users get their data
//! out in the formats reference managers (and Zotero) exchange.

use crate::core::models::Paper;

/// Parse a paper's `authors` JSON array into display-name strings.
pub fn parse_authors(authors_json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(authors_json).unwrap_or_default()
}

/// Citation key for a paper: the stored key, else a derived one
/// (first author's last token + year), else a fallback based on the id.
pub fn citation_key_for(p: &Paper) -> String {
    if let Some(k) = p.citation_key.as_deref().filter(|k| !k.is_empty()) {
        return k.to_string();
    }
    let authors = parse_authors(&p.authors);
    let base = authors
        .first()
        .and_then(|a| a.split_whitespace().last().map(|s| s.to_string()))
        .unwrap_or_else(|| "anon".to_string());
    let year = p.year.map(|y| y.to_string()).unwrap_or_else(|| "n.d.".to_string());
    format!("{base}{year}")
}

/// Reverse-map our item_type to a BibTeX entry type.
fn bibtex_type(item_type: Option<&str>) -> &'static str {
    match item_type.unwrap_or("journal") {
        "journal" => "article",
        "book" => "book",
        "bookSection" => "incollection",
        "conference" => "inproceedings",
        "thesis" => "phdthesis",
        "report" => "techreport",
        "patent" => "patent",
        _ => "misc",
    }
}

fn esc(s: &str) -> String {
    // Keep it simple and robust: strip braces (they would otherwise act as
    // BibTeX grouping) and normalize whitespace.
    s.replace(['{', '}'], "").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Serialize one paper as a BibTeX entry.
pub fn bibtex_for_paper(p: &Paper) -> String {
    macro_rules! push_field {
        ($out:expr, $k:expr, $v:expr) => {
            if let Some(v) = $v.filter(|v| !v.is_empty()) {
                $out.push_str(&format!("  {} = {{{}}},\n", $k, esc(v)));
            }
        };
    }
    let mut out = format!("@{}{{{},\n", bibtex_type(p.item_type.as_deref()), citation_key_for(p));
    push_field!(out, "title", Some(p.title.as_str()));
    let authors = parse_authors(&p.authors);
    if !authors.is_empty() {
        out.push_str(&format!("  author = {{{}}},\n", esc(&authors.join(" and "))));
    }
    push_field!(out, "year", p.year.map(|y| y.to_string()).as_deref());
    push_field!(out, "journal", p.journal.as_deref());
    push_field!(out, "volume", p.volume.as_deref());
    push_field!(out, "number", p.issue.as_deref());
    push_field!(out, "pages", p.pages.as_deref());
    push_field!(out, "doi", p.doi.as_deref());
    push_field!(out, "url", p.url.as_deref());
    push_field!(out, "abstract", p.abstract_text.as_deref());
    push_field!(out, "keywords", Some(p.keywords.as_str()));
    push_field!(out, "publisher", p.publisher.as_deref());
    push_field!(out, "address", p.place.as_deref());
    push_field!(out, "series", p.series.as_deref());
    push_field!(out, "edition", p.edition.as_deref());
    push_field!(out, "isbn", p.isbn.as_deref());
    push_field!(out, "issn", p.issn.as_deref());
    let editors = parse_authors(&p.editor);
    if !editors.is_empty() {
        out.push_str(&format!("  editor = {{{}}},\n", esc(&editors.join(" and "))));
    }
    out.push_str("}\n");
    out
}

/// Reverse-map our item_type to a RIS type code.
fn ris_type(item_type: Option<&str>) -> &'static str {
    match item_type.unwrap_or("journal") {
        "journal" => "JOUR",
        "book" => "BOOK",
        "bookSection" => "CHAP",
        "conference" => "CONF",
        "thesis" => "THES",
        "report" => "RPRT",
        "patent" => "PAT",
        "webpage" => "ELEC",
        _ => "GEN",
    }
}

/// Serialize one paper as an RIS record.
pub fn ris_for_paper(p: &Paper) -> String {
    let mut out = String::new();
    out.push_str(&format!("TY  - {}\n", ris_type(p.item_type.as_deref())));
    for a in parse_authors(&p.authors) {
        out.push_str(&format!("AU  - {}\n", a));
    }
    if let Some(t) = Some(&p.title).filter(|t| !t.is_empty()) {
        out.push_str(&format!("TI  - {}\n", t));
    }
    if let Some(y) = p.year {
        out.push_str(&format!("PY  - {y}\n"));
    }
    if let Some(j) = p.journal.as_deref().filter(|j| !j.is_empty()) {
        out.push_str(&format!("JO  - {j}\n"));
    }
    if let Some(v) = p.volume.as_deref().filter(|v| !v.is_empty()) {
        out.push_str(&format!("VL  - {v}\n"));
    }
    if let Some(i) = p.issue.as_deref().filter(|i| !i.is_empty()) {
        out.push_str(&format!("IS  - {i}\n"));
    }
    if let Some(pages) = p.pages.as_deref().filter(|p| !p.is_empty()) {
        if let Some((sp, ep)) = pages.split_once('-') {
            out.push_str(&format!("SP  - {}\nEP  - {}\n", sp.trim(), ep.trim()));
        } else {
            out.push_str(&format!("SP  - {pages}\n"));
        }
    }
    if let Some(d) = p.doi.as_deref().filter(|d| !d.is_empty()) {
        out.push_str(&format!("DO  - {d}\n"));
    }
    if let Some(u) = p.url.as_deref().filter(|u| !u.is_empty()) {
        out.push_str(&format!("UR  - {u}\n"));
    }
    if let Some(a) = p.abstract_text.as_deref().filter(|a| !a.is_empty()) {
        out.push_str(&format!("AB  - {a}\n"));
    }
    if let Some(pb) = p.publisher.as_deref().filter(|pb| !pb.is_empty()) {
        out.push_str(&format!("PB  - {pb}\n"));
    }
    out.push_str("ER  - \n\n");
    out
}

/// Reverse-map our item_type to a CSL-JSON type.
fn csl_type(item_type: Option<&str>) -> &'static str {
    match item_type.unwrap_or("journal") {
        "journal" => "article-journal",
        "book" => "book",
        "bookSection" => "chapter",
        "conference" => "paper-conference",
        "thesis" => "thesis",
        "report" => "report",
        "patent" => "patent",
        "webpage" => "webpage",
        "newspaper" => "article-newspaper",
        _ => "article",
    }
}

fn csl_author(name: &str) -> serde_json::Value {
    // Names are not structurally parsed yet — emit CSL `literal` so we never
    // mis-split CJK / corporate / compound names.
    serde_json::json!({ "literal": name })
}

/// Serialize papers as a CSL-JSON array (the citation interchange format
/// used by Zotero / pandoc / citeproc).
pub fn csl_json_for_papers(papers: &[Paper]) -> String {
    let items: Vec<serde_json::Value> = papers
        .iter()
        .map(|p| {
            let mut item = serde_json::json!({
                "id": citation_key_for(p),
                "type": csl_type(p.item_type.as_deref()),
                "title": p.title,
            });
            let authors: Vec<serde_json::Value> =
                parse_authors(&p.authors).into_iter().map(|a| csl_author(&a)).collect();
            if !authors.is_empty() {
                item["author"] = serde_json::Value::Array(authors);
            }
            let editors: Vec<serde_json::Value> =
                parse_authors(&p.editor).into_iter().map(|a| csl_author(&a)).collect();
            if !editors.is_empty() {
                item["editor"] = serde_json::Value::Array(editors);
            }
            if let Some(y) = p.year {
                item["issued"] = serde_json::json!({ "date-parts": [[y]] });
            }
            if let Some(j) = p.journal.as_deref().filter(|j| !j.is_empty()) {
                item["container-title"] = serde_json::Value::String(j.to_string());
            }
            if let Some(v) = p.volume.as_deref().filter(|v| !v.is_empty()) {
                item["volume"] = serde_json::Value::String(v.to_string());
            }
            if let Some(i) = p.issue.as_deref().filter(|i| !i.is_empty()) {
                item["issue"] = serde_json::Value::String(i.to_string());
            }
            if let Some(pg) = p.pages.as_deref().filter(|p| !p.is_empty()) {
                item["page"] = serde_json::Value::String(pg.to_string());
            }
            if let Some(d) = p.doi.as_deref().filter(|d| !d.is_empty()) {
                item["DOI"] = serde_json::Value::String(d.to_string());
            }
            if let Some(u) = p.url.as_deref().filter(|u| !u.is_empty()) {
                item["URL"] = serde_json::Value::String(u.to_string());
            }
            if let Some(a) = p.abstract_text.as_deref().filter(|a| !a.is_empty()) {
                item["abstract"] = serde_json::Value::String(a.to_string());
            }
            if let Some(pb) = p.publisher.as_deref().filter(|pb| !pb.is_empty()) {
                item["publisher"] = serde_json::Value::String(pb.to_string());
            }
            if let Some(isbn) = p.isbn.as_deref().filter(|i| !i.is_empty()) {
                item["ISBN"] = serde_json::Value::String(isbn.to_string());
            }
            item
        })
        .collect();
    serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_paper() -> Paper {
        serde_json::from_value(serde_json::json!({
            "id": "p1",
            "title": "Attention Is All You Need",
            "authors": "[\"Albert Einstein\", \"王小明\"]",
            "year": 2026,
            "journal": "Nature",
            "doi": "10.1000/xyz",
            "volume": "1",
            "issue": "2",
            "pages": "12-34",
            "keywords": "[]",
            "editor": "[]",
            "abstract_text": null,
            "url": null,
            "citation_key": null,
            "bibtex": null,
            "file_path": null,
            "file_size": null,
            "page_count": null,
            "language": null,
            "item_type": "journal",
            "conference_name": null,
            "publisher": null,
            "place": null,
            "series": null,
            "edition": null,
            "isbn": null,
            "issn": null,
            "num_pages": null,
            "archive_location": null,
            "call_number": null,
            "rights": null,
            "deleted_at": null,
            "is_favorite": 0,
            "read_status": "unread",
            "last_read_at": null,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "imported_at": "2026-01-01T00:00:00Z"
        })).unwrap()
    }

    #[test]
    fn bibtex_round_trip_shape() {
        let p = sample_paper();
        let bib = bibtex_for_paper(&p);
        assert!(bib.starts_with("@article{Einstein2026,"), "got: {bib}");
        assert!(bib.contains("author = {Albert Einstein and 王小明}"));
        assert!(bib.contains("doi = {10.1000/xyz}"));
    }

    #[test]
    fn ris_shape() {
        let ris = ris_for_paper(&sample_paper());
        assert!(ris.contains("TY  - JOUR"));
        assert!(ris.contains("AU  - Albert Einstein"));
        assert!(ris.contains("SP  - 12\nEP  - 34"));
        assert!(ris.contains("DO  - 10.1000/xyz"));
    }

    #[test]
    fn csl_json_shape() {
        let json = csl_json_for_papers(&[sample_paper()]);
        assert!(json.contains("\"type\": \"article-journal\""));
        assert!(json.contains("\"literal\": \"Albert Einstein\""));
        assert!(json.contains("\"DOI\": \"10.1000/xyz\""));
    }
}
