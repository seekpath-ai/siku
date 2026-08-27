use std::path::Path;

/// A loaded inline skill (Kimi Code style): a `<skills_dir>/<name>/SKILL.md`
/// with YAML-ish frontmatter (`name`, `description`) and a markdown body.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub content: String,
}

/// Parse `SKILL.md`: optional `---` frontmatter block with `name:` and
/// `description:` fields, followed by the markdown body.
fn parse_skill_md(text: &str) -> Option<(String, String, String)> {
    let body = text.strip_prefix("---")?;
    let end = body.find("---")?;
    let front = &body[..end];
    let content = body[end + 3..].trim().to_string();

    let mut name = String::new();
    let mut description = String::new();
    for line in front.lines() {
        if let Some(v) = line.strip_prefix("name:") {
            name = v.trim().trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(v) = line.strip_prefix("description:") {
            description = v.trim().trim_matches('"').trim_matches('\'').to_string();
        }
    }
    if name.is_empty() {
        return None;
    }
    Some((name, description, content))
}

/// Normalize a frontmatter `name` into a form that keeps the generated
/// `skill_<name>` tool name legal: providers like OpenAI require tool names
/// matching `^[a-zA-Z0-9_-]+$`. Lowercases and maps every other char
/// (spaces, CJK, ...) to `-`, collapsing repeats. Returns `None` when
/// nothing usable remains — the caller skips that skill.
fn sanitize_name(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            last_dash = false;
        } else if !out.is_empty() && !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { None } else { Some(out) }
}

/// Scan a skills directory for `<name>/SKILL.md` files.
pub fn scan(dir: &Path) -> Vec<Skill> {
    let mut skills = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return skills;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let md = path.join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&md) else {
            continue;
        };
        if let Some((name, description, content)) = parse_skill_md(&text) {
            let Some(name) = sanitize_name(&name) else {
                tracing::warn!(path = %md.display(), "skill skipped: name sanitizes to empty");
                continue;
            };
            skills.push(Skill {
                name,
                description,
                content,
            });
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

#[cfg(test)]
mod tests {
    use super::sanitize_name;

    #[test]
    fn sanitize_name_produces_legal_tool_names() {
        assert_eq!(sanitize_name("pdf-tools").as_deref(), Some("pdf-tools"));
        assert_eq!(sanitize_name("My Skill").as_deref(), Some("my-skill"));
        assert_eq!(sanitize_name("a  b__c").as_deref(), Some("a-b__c"));
        assert_eq!(sanitize_name("x 阅读 y").as_deref(), Some("x-y"));
        // Nothing usable left → skipped by the caller.
        assert_eq!(sanitize_name("中文名"), None);
        assert_eq!(sanitize_name("阅读 笔记"), None);
        assert_eq!(sanitize_name("  "), None);
    }
}
