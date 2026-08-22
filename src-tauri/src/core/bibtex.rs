use std::collections::HashMap;

/// A parsed BibTeX entry.
pub struct BibtexEntry {
    pub entry_type: String,
    pub cite_key: String,
    pub fields: HashMap<String, String>,
}

impl BibtexEntry {
    pub fn field(&self, keys: &[&str]) -> Option<&str> {
        keys.iter()
            .find_map(|k| self.fields.get(*k).map(|s| s.as_str()))
            .filter(|s| !s.is_empty())
    }
}

/// Read a `{ ... }` value handling nested braces and `\` escapes.
/// `chars[*i]` must be `{`; on return `*i` is past the closing `}`.
fn read_braced(chars: &[u8], i: &mut usize) -> String {
    *i += 1;
    let mut depth = 1usize;
    let mut s = String::new();
    while *i < chars.len() {
        let c = chars[*i];
        if c == b'\\' {
            s.push('\\');
            *i += 1;
            if *i < chars.len() {
                s.push(chars[*i] as char);
                *i += 1;
            }
            continue;
        }
        match c {
            b'{' => {
                depth += 1;
                s.push('{');
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    *i += 1;
                    return s;
                }
                s.push('}');
            }
            _ => s.push(c as char),
        }
        *i += 1;
    }
    s
}

/// Read a quoted `"..."` value.
fn read_quoted(chars: &[u8], i: &mut usize) -> String {
    *i += 1; // skip opening quote
    let mut s = String::new();
    while *i < chars.len() {
        let c = chars[*i];
        if c == b'\\' {
            s.push('\\');
            *i += 1;
            if *i < chars.len() {
                s.push(chars[*i] as char);
                *i += 1;
            }
            continue;
        }
        if c == b'"' {
            *i += 1;
            return s;
        }
        s.push(c as char);
        *i += 1;
    }
    s
}

/// Read a bare (unbraced, unquoted) value until `,` or the closing brace.
fn read_bare(chars: &[u8], i: &mut usize) -> String {
    let mut s = String::new();
    while *i < chars.len() {
        let c = chars[*i];
        if c == b',' || c == b'}' || c == b')' {
            break;
        }
        s.push(c as char);
        *i += 1;
    }
    s.trim().to_string()
}

/// Parse BibTeX text into entries. Handles nested braces, escapes, quoted
/// values, `%` comments, and multiple entries (returns all of them).
pub fn parse_bibtex(text: &str) -> Vec<BibtexEntry> {
    let chars = text.as_bytes();
    let n = chars.len();
    let mut entries = Vec::new();
    let mut i = 0usize;

    while i < n {
        let c = chars[i];
        // Skip comments (% to end of line) and whitespace outside entries.
        if c == b'%' {
            while i < n && chars[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c != b'@' {
            i += 1;
            continue;
        }
        i += 1; // past '@'

        // Entry type.
        let mut entry_type = String::new();
        while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == b'_' || chars[i] == b'-') {
            entry_type.push(chars[i] as char);
            i += 1;
        }
        if entry_type.is_empty() {
            continue;
        }
        // Opening bracket.
        while i < n && chars[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= n || (chars[i] != b'{' && chars[i] != b'(') {
            continue;
        }
        let close = if chars[i] == b'{' { b'}' } else { b')' };
        i += 1;

        // Cite key.
        let mut cite_key = String::new();
        while i < n && chars[i] != b',' && chars[i] != close {
            cite_key.push(chars[i] as char);
            i += 1;
        }
        if i < n && chars[i] == b',' {
            i += 1;
        }

        // Fields.
        let mut fields = HashMap::new();
        loop {
            while i < n && (chars[i].is_ascii_whitespace() || chars[i] == b',') {
                i += 1;
            }
            if i >= n || chars[i] == close {
                if i < n {
                    i += 1;
                }
                break;
            }
            // Field key.
            let mut key = String::new();
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == b'_' || chars[i] == b'-') {
                key.push(chars[i] as char);
                i += 1;
            }
            while i < n && (chars[i].is_ascii_whitespace() || chars[i] == b'=') {
                i += 1;
            }
            if i >= n {
                break;
            }
            // Value.
            let value = if chars[i] == b'{' {
                read_braced(chars, &mut i)
            } else if chars[i] == b'"' {
                read_quoted(chars, &mut i)
            } else {
                read_bare(chars, &mut i)
            };
            if !key.is_empty() {
                fields.insert(key.to_lowercase(), value.trim().to_string());
            }
        }

        entries.push(BibtexEntry {
            entry_type: entry_type.to_lowercase(),
            cite_key: cite_key.trim().to_string(),
            fields,
        });
    }

    entries
}

/// Split an author/editor list on `and`.
pub fn split_authors(s: &str) -> Vec<String> {
    s.split(" and ")
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect()
}

/// Map a BibTeX entry type to our item_type.
pub fn map_item_type(entry_type: &str) -> &'static str {
    match entry_type {
        "article" => "journal",
        "book" => "book",
        "inbook" | "incollection" => "bookSection",
        "inproceedings" | "conference" => "conference",
        "phdthesis" | "mastersthesis" | "thesis" => "thesis",
        "techreport" | "report" => "report",
        "webpage" | "online" | "electronic" => "webpage",
        "newspaper" => "newspaper",
        "patent" => "patent",
        "misc" | "unpublished" | "manual" => "other",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_entry() {
        let bib = r#"
@article{smith2024deep,
  title = {Deep {N}etworks for {NLP}},
  author = {Smith, John and Doe, Jane},
  year = {2024},
  journal = {Journal of AI},
  volume = {12},
  number = {3},
  pages = {100--120},
  doi = {10.1000/xyz}
}
"#;
        let entries = parse_bibtex(bib);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.entry_type, "article");
        assert_eq!(e.cite_key, "smith2024deep");
        assert_eq!(e.field(&["title"]).unwrap(), "Deep {N}etworks for {NLP}");
        assert_eq!(split_authors(e.field(&["author"]).unwrap()).len(), 2);
        assert_eq!(e.field(&["pages"]).unwrap(), "100--120");
        assert_eq!(map_item_type(&e.entry_type), "journal");
    }

    #[test]
    fn handles_nested_braces_quotes_and_multiple_entries() {
        let bib = r#"
@book{key1,
  title = "A {Book} Title",
  publisher = {Some {Press} Inc.},
  address = {New York}
}
@inproceedings{key2,
  title = {Conf Paper},
  booktitle = {Proc. Conf}
}
"#;
        let entries = parse_bibtex(bib);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].field(&["title"]).unwrap(), "A {Book} Title");
        assert_eq!(entries[0].field(&["publisher"]).unwrap(), "Some {Press} Inc.");
        assert_eq!(entries[1].field(&["booktitle"]).unwrap(), "Proc. Conf");
        assert_eq!(map_item_type(&entries[1].entry_type), "conference");
    }

    #[test]
    fn ignores_comments() {
        let bib = "% a comment\n@misc{k, title = {T}}\n% another";
        let entries = parse_bibtex(bib);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].field(&["title"]).unwrap(), "T");
    }

    #[test]
    fn parses_arxiv_entry() {
        let bib = r#"
@misc{wang2025networkarenabenchmarkingai,
      title={A Network Arena for Benchmarking AI Agents on Network Troubleshooting},
      author={Zhihao Wang and Alessandro Cornacchia and Alessio Sacco and Franco Galante and Marco Canini and Dingde Jiang},
      year={2025},
      eprint={2512.16381},
      archivePrefix={arXiv},
      primaryClass={cs.NI},
      url={https://arxiv.org/abs/2512.16381},
}
"#;
        let entries = parse_bibtex(bib);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.entry_type, "misc");
        assert_eq!(e.cite_key, "wang2025networkarenabenchmarkingai");
        assert_eq!(e.field(&["title"]).unwrap(), "A Network Arena for Benchmarking AI Agents on Network Troubleshooting");
        assert_eq!(split_authors(e.field(&["author"]).unwrap()).len(), 6);
        assert_eq!(e.field(&["year"]).unwrap(), "2025");
        assert_eq!(e.field(&["url"]).unwrap(), "https://arxiv.org/abs/2512.16381");
        assert_eq!(e.field(&["archiveprefix"]).unwrap(), "arXiv");
        assert_eq!(e.field(&["eprint"]).unwrap(), "2512.16381");
    }
}
