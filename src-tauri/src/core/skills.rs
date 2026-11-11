use std::path::Path;

use serde::Serialize;
use sqlx::SqlitePool;

/// Where a skill comes from. Built-in skills ship inside the app bundle
/// (read-only, trusted, review-exempt); user skills live in the user skills
/// directory and go through import/review/delete as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    Builtin,
    User,
}

/// Dependencies declared in SKILL.md frontmatter (inline bracket lists only):
///
/// ```yaml
/// requires:
///   binaries: [python3, soffice]
///   pip: [python-pptx]
/// ```
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRequires {
    pub binaries: Vec<String>,
    pub pip: Vec<String>,
}

impl SkillRequires {
    pub fn is_empty(&self) -> bool {
        self.binaries.is_empty() && self.pip.is_empty()
    }
}

/// A loaded inline skill (Kimi Code style): a `<skills_dir>/<name>/SKILL.md`
/// with YAML-ish frontmatter (`name`, `description`, optional `requires`) and
/// a markdown body. `dir` is the skill's own directory — auxiliary files
/// (scripts, templates) live there and the SKILL.md body references them by
/// relative path.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub content: String,
    pub dir: std::path::PathBuf,
    pub source: SkillSource,
    pub requires: SkillRequires,
    /// 技能运行需要的 agent 工具白名单（前言 `tools: [bash, file_write]`）。
    /// 挂载该技能的会话在原有工具白名单基础上并集放行这些工具——
    /// 挂载即授权，卸载即回收。
    pub tools: Vec<String>,
}

/// Parse `SKILL.md`: optional `---` frontmatter block with `name:` and
/// `description:` fields, followed by the markdown body.
fn parse_skill_md(text: &str) -> Option<(String, String, String, SkillRequires, Vec<String>)> {
    let body = text.strip_prefix("---")?;
    let end = body.find("---")?;
    let front = &body[..end];
    let content = body[end + 3..].trim().to_string();

    let mut name = String::new();
    let mut description = String::new();
    let mut requires = SkillRequires::default();
    let mut tools: Vec<String> = Vec::new();
    let mut in_requires = false;
    for line in front.lines() {
        let trimmed = line.trim();
        if let Some(v) = trimmed.strip_prefix("name:") {
            if !line.starts_with([' ', '\t']) {
                in_requires = false;
            }
            name = v.trim().trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(v) = trimmed.strip_prefix("description:") {
            description = v.trim().trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(v) = trimmed.strip_prefix("tools:") {
            if !line.starts_with([' ', '\t']) {
                in_requires = false;
                tools = parse_inline_list(v);
            }
        } else if trimmed == "requires:" {
            in_requires = true;
        } else if in_requires && line.starts_with([' ', '\t']) {
            if let Some(v) = trimmed.strip_prefix("binaries:") {
                requires.binaries = parse_inline_list(v);
            } else if let Some(v) = trimmed.strip_prefix("pip:") {
                requires.pip = parse_inline_list(v);
            }
        } else if !line.starts_with([' ', '\t']) {
            in_requires = false;
        }
    }
    if name.is_empty() {
        return None;
    }
    Some((name, description, content, requires, tools))
}

/// Parse an inline `[a, b, c]` list from a frontmatter value.
fn parse_inline_list(value: &str) -> Vec<String> {
    let v = value.trim();
    let inner = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(v);
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
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
pub fn scan(dir: &Path, source: SkillSource) -> Vec<Skill> {
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
        if let Some((name, description, content, requires, tools)) = parse_skill_md(&text) {
            let Some(name) = sanitize_name(&name) else {
                tracing::warn!(path = %md.display(), "skill skipped: name sanitizes to empty");
                continue;
            };
            skills.push(Skill {
                name,
                description,
                content,
                dir: path.clone(),
                source,
                requires,
                tools,
            });
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Built-in skills bundled with the app. Resolution mirrors the pdfium
/// lookup (production layouts first, dev fallback last):
/// `<resource_dir>/skills`, exe-adjacent layouts incl. macOS
/// `.app/Contents/Resources/skills`, then the source tree for cargo dev.
pub fn builtin_skills_dir(resource_dir: Option<&Path>) -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(rd) = resource_dir {
        candidates.push(rd.join("skills"));
        candidates.push(rd.join("resources").join("skills"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("skills"));
            candidates.push(dir.join("resources").join("skills"));
            if let Some(res) = dir
                .parent()
                .and_then(|contents| contents.parent())
                .map(|contents| contents.join("Resources"))
            {
                candidates.push(res.join("skills"));
            }
        }
    }
    candidates.push(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("skills"),
    );
    candidates.into_iter().find(|p| p.is_dir())
}

/// Merged scan of the built-in and user skill directories. On a name
/// collision the BUILT-IN skill wins (built-ins are irreplaceable; imports
/// colliding with them are rejected up front, so a collision here means
/// someone dropped a folder into the user dir by hand — warn and ignore it).
pub fn scan_all(builtin_dir: Option<&Path>, user_dir: &Path) -> Vec<Skill> {
    let mut by_name: std::collections::HashMap<String, Skill> =
        scan(user_dir, SkillSource::User)
            .into_iter()
            .map(|s| (s.name.clone(), s))
            .collect();
    for s in builtin_dir.map(|d| scan(d, SkillSource::Builtin)).unwrap_or_default() {
        if by_name.insert(s.name.clone(), s.clone()).is_some() {
            tracing::warn!(skill = %s.name, "用户目录中的同名技能被内置技能覆盖（内置不可替换）");
        }
    }
    let mut skills: Vec<Skill> = by_name.into_values().collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// One-level-ish file listing of a skill directory (relative paths, dirs
/// suffixed with `/`, SKILL.md itself omitted — the model already has its
/// content). Lets the model discover scripts/templates without a probing
/// file_list call. Capped so a bloated skill can't flood the context.
pub fn list_skill_files(dir: &Path) -> Vec<String> {
    const MAX_ENTRIES: usize = 50;
    fn walk(base: &Path, rel: &Path, out: &mut Vec<String>, depth: usize) {
        if out.len() >= MAX_ENTRIES || depth > 2 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(base.join(rel)) else { return };
        let mut names: Vec<_> = entries.flatten().collect();
        names.sort_by_key(|e| e.file_name());
        for entry in names {
            if out.len() >= MAX_ENTRIES {
                return;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || (rel.as_os_str().is_empty() && name == "SKILL.md") {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let child = rel.join(&name);
            out.push(format!("{}{}", child.to_string_lossy(), if is_dir { "/" } else { "" }));
            if is_dir {
                walk(base, &child, out, depth + 1);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, Path::new(""), &mut out, 1);
    out
}

// ── Plugins dialog: listing, detail, import, delete ─────────────────────
//
// There are no built-in skills and no global enable switch: external skills
// live in the user skills directory and are mounted per session
// (chat_sessions.selected_skills). An unmounted skill never reaches the
// model — its description costs no context.

/// One row of the plugins dialog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// Directory holding SKILL.md.
    pub path: String,
    /// Latest review badge; `stale` = the skill changed since the review.
    /// Always None for built-in skills (first-party, review-exempt).
    pub review: Option<crate::core::skill_review::ReviewBadge>,
    pub source: SkillSource,
    /// Declared dependencies (unprobed — see `probe_dependencies`).
    pub requires: SkillRequires,
}

/// All skills (built-in + user), sorted by name. Built-ins skip both the
/// content hash and the review lookup; user skills only pay the hash when a
/// review record exists (an unreviewed skill has nothing to go stale
/// against — and hashing big skill dirs is what made the dialog slow).
pub async fn list_all(db: &SqlitePool, builtin_dir: Option<&Path>, user_dir: &Path) -> Vec<SkillInfo> {
    let mut out = Vec::new();
    for s in scan_all(builtin_dir, user_dir) {
        let review = match s.source {
            SkillSource::Builtin => None,
            SkillSource::User => match crate::core::skill_review::get_review(db, &s.name).await {
                None => None,
                Some(rec) => {
                    let hash = crate::core::skill_review::content_hash(&s.dir);
                    Some(crate::core::skill_review::ReviewBadge {
                        verdict: rec.verdict,
                        stale: rec.content_hash != hash,
                    })
                }
            },
        };
        out.push(SkillInfo {
            path: s.dir.to_string_lossy().to_string(),
            name: s.name,
            description: s.description,
            review,
            source: s.source,
            requires: s.requires,
        });
    }
    out
}

/// Full skill (incl. SKILL.md body) for the detail dialog.
pub fn get(builtin_dir: Option<&Path>, user_dir: &Path, name: &str) -> Option<(Skill, String)> {
    scan_all(builtin_dir, user_dir)
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| {
            let path = s.dir.to_string_lossy().to_string();
            (s, path)
        })
}

/// Copy a directory tree recursively.
fn copy_dir(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("创建目录失败：{e}"))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("读取目录失败：{e}"))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| format!("复制 {} 失败：{e}", from.display()))?;
        }
    }
    Ok(())
}

/// Install a skill from a folder containing SKILL.md (copied into the user
/// skills directory under its sanitized name). Errors on name conflicts
/// (with user skills OR built-ins — shadowing a built-in would be confusing).
pub fn import_folder(
    builtin_dir: Option<&Path>,
    user_dir: &Path,
    src: &Path,
) -> Result<SkillInfo, String> {
    let md = src.join("SKILL.md");
    let text = std::fs::read_to_string(&md)
        .map_err(|_| "所选文件夹中没有可读取的 SKILL.md".to_string())?;
    let (name, _, _, requires, _) = parse_skill_md(&text).ok_or("SKILL.md 缺少 name 前言字段")?;
    let name = sanitize_name(&name).ok_or("技能名无法转换为合法标识")?;
    reject_builtin_conflict(builtin_dir, &name)?;
    let dest = user_dir.join(&name);
    if dest.exists() {
        return Err(format!("已存在同名技能「{name}」，请先删除或改名"));
    }
    copy_dir(src, &dest)?;
    Ok(SkillInfo {
        path: dest.to_string_lossy().to_string(),
        description: parse_skill_md(&text).map(|(_, d, _, _, _)| d).unwrap_or_default(),
        name,
        review: None,
        source: SkillSource::User,
        requires,
    })
}

/// Built-in names are reserved: a user skill shadowing a built-in would make
/// the plugins dialog show two cards for one tool name.
fn reject_builtin_conflict(builtin_dir: Option<&Path>, name: &str) -> Result<(), String> {
    if let Some(d) = builtin_dir {
        if d.join(name).join("SKILL.md").is_file() {
            return Err(format!("「{name}」与内置技能重名，请修改 SKILL.md 的 name 后再导入"));
        }
    }
    Ok(())
}

/// Safety caps for zip imports (a skill is documentation + small scripts).
const ZIP_MAX_ENTRIES: usize = 512;
const ZIP_MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;

/// Install a skill from a zip archive. The SKILL.md may sit at the archive
/// root or inside a single top-level folder; everything alongside it is
/// extracted. Zip-slip entries are rejected.
pub fn import_zip(
    builtin_dir: Option<&Path>,
    user_dir: &Path,
    zip_path: &Path,
) -> Result<SkillInfo, String> {
    use std::io::Read;
    let file = std::fs::File::open(zip_path).map_err(|e| format!("无法打开压缩包：{e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("无法读取压缩包：{e}"))?;
    if archive.len() > ZIP_MAX_ENTRIES {
        return Err(format!("压缩包条目过多（{}），上限 {ZIP_MAX_ENTRIES}", archive.len()));
    }

    // Locate SKILL.md at root or one folder deep.
    let mut prefix: Option<String> = None;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = entry.enclosed_name().map(|p| p.to_string_lossy().to_string()) else {
            continue;
        };
        if name.starts_with("__MACOSX/") {
            continue;
        }
        let depth = name.matches('/').count();
        if name.ends_with("SKILL.md") && depth <= 1 {
            prefix = Some(name.trim_end_matches("SKILL.md").to_string());
            break;
        }
    }
    let prefix = prefix.ok_or("压缩包中没有找到 SKILL.md（根目录或一层子目录内）")?;

    // Read + parse SKILL.md first so we know the target folder name.
    let md_rel = format!("{prefix}SKILL.md");
    let mut text = String::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        if entry.enclosed_name().map(|p| p.to_string_lossy().to_string()).as_deref() == Some(md_rel.as_str()) {
            entry.read_to_string(&mut text).map_err(|e| format!("读取 SKILL.md 失败：{e}"))?;
            break;
        }
    }
    let (name, description, _, requires, _) =
        parse_skill_md(&text).ok_or("SKILL.md 缺少 name 前言字段")?;
    let name = sanitize_name(&name).ok_or("技能名无法转换为合法标识")?;
    reject_builtin_conflict(builtin_dir, &name)?;
    let dest = user_dir.join(&name);
    if dest.exists() {
        return Err(format!("已存在同名技能「{name}」，请先删除或改名"));
    }

    // Extract everything under the prefix.
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(rel) = entry.enclosed_name().map(|p| p.to_string_lossy().to_string()) else {
            return Err("压缩包含有非法路径条目，已中止导入".to_string());
        };
        if rel.starts_with("__MACOSX/") {
            continue;
        }
        let Some(rel) = rel.strip_prefix(&prefix) else { continue };
        if rel.is_empty() {
            continue;
        }
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        total += entry.size();
        if total > ZIP_MAX_TOTAL_BYTES {
            let _ = std::fs::remove_dir_all(&dest);
            return Err("压缩包解压后超过 50MB，已中止导入".to_string());
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut w = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut w).map_err(|e| e.to_string())?;
    }
    Ok(SkillInfo {
        path: dest.to_string_lossy().to_string(),
        description,
        name,
        review: None,
        source: SkillSource::User,
        requires,
    })
}

/// Delete an installed USER skill (removes its directory). Built-in skills
/// are read-only parts of the app bundle and cannot be deleted.
pub fn delete(builtin_dir: Option<&Path>, user_dir: &Path, name: &str) -> Result<(), String> {
    let safe = sanitize_name(name).ok_or("非法技能名")?;
    if safe != name {
        return Err("非法技能名".to_string());
    }
    let dir = user_dir.join(name);
    if !dir.is_dir() {
        if let Some(b) = builtin_dir {
            if b.join(name).join("SKILL.md").is_file() {
                return Err("内置技能不可删除".to_string());
            }
        }
        return Err(format!("技能「{name}」不存在"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("删除失败：{e}"))
}

/// One dependency's probe result for the detail dialog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DepStatus {
    /// `binary` (system program) or `pip` (Python package).
    pub kind: String,
    pub name: String,
    pub available: bool,
    /// 缺失时的补充指引（如 pip 依赖所在的隔离环境状态）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Probe a skill's declared dependencies (process spawns — call from the
/// detail dialog, not the list path).
///
/// pip 依赖探测的是隔离 conda 环境（siku-office），不是系统 Python——技能
/// 脚本只在隔离环境里跑，系统环境装没装与可用性无关。
pub fn probe_dependencies(app_data_dir: &Path, requires: &SkillRequires) -> Vec<DepStatus> {
    let mut out = Vec::new();
    if requires.is_empty() {
        return out;
    }
    for b in &requires.binaries {
        out.push(DepStatus {
            kind: "binary".to_string(),
            name: b.clone(),
            available: crate::core::skill_review::binary_available(b),
            hint: None,
        });
    }
    if !requires.pip.is_empty() {
        let env = crate::core::skill_env::resolve(app_data_dir);
        let missing_hint = if env.python.is_some() {
            None
        } else if env.conda.is_some() {
            Some("隔离环境 siku-office 尚未创建".to_string())
        } else {
            Some("隔离环境 siku-office 尚未安装（将先装 Miniconda）".to_string())
        };
        for p in &requires.pip {
            let module = crate::core::skill_review::pip_to_module(p);
            let available = env
                .python
                .as_deref()
                .map(|py| crate::core::skill_env::python_module_available_in(py, &module))
                .unwrap_or(false);
            out.push(DepStatus {
                kind: "pip".to_string(),
                name: p.clone(),
                available,
                hint: if available { None } else { missing_hint.clone() },
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const SKILL_MD: &str = "---\nname: demo-skill\ndescription: 演示\n---\n\n# 正文\n";

    #[test]
    fn parse_requires_inline_lists() {
        let text = "---\nname: pptx\ndescription: 演示\ntools: [bash, file_write]\nrequires:\n  binaries: [python3, soffice]\n  pip: [python-pptx]\n---\n\n# 正文\n";
        let (name, _, _, req, tools) = parse_skill_md(text).unwrap();
        assert_eq!(name, "pptx");
        assert_eq!(req.binaries, vec!["python3", "soffice"]);
        assert_eq!(req.pip, vec!["python-pptx"]);
        assert_eq!(tools, vec!["bash", "file_write"]);
        // Frontmatter without requires parses to empty deps.
        let (_, _, _, req2, _) = parse_skill_md(SKILL_MD).unwrap();
        assert!(req2.is_empty());
    }

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

    #[test]
    fn import_folder_installs_and_delete_removes() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), SKILL_MD).unwrap();
        let lib = tmp.path().join("lib");
        let info = import_folder(None, &lib, &src).unwrap();
        assert_eq!(info.name, "demo-skill");
        assert_eq!(info.description, "演示");
        assert!(lib.join("demo-skill/SKILL.md").is_file());
        // Name conflict is an error, not an overwrite.
        assert!(import_folder(None, &lib, &src).is_err());
        delete(None, &lib, "demo-skill").unwrap();
        assert!(!lib.join("demo-skill").exists());
        // A built-in with the same name blocks deletion of the "missing"
        // user skill and rejects imports.
        let builtin = tmp.path().join("builtin");
        std::fs::create_dir_all(builtin.join("demo-skill")).unwrap();
        std::fs::write(builtin.join("demo-skill/SKILL.md"), SKILL_MD).unwrap();
        assert!(delete(Some(&builtin), &lib, "demo-skill").is_err());
        assert!(import_folder(Some(&builtin), &lib, &src).is_err());
    }

    fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, content) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(content.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn import_zip_installs_skill_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let data = build_zip(&[
            ("pkg/SKILL.md", SKILL_MD),
            ("pkg/scripts/run.sh", "echo hi"),
        ]);
        let zip_path = tmp.path().join("skill.zip");
        std::fs::write(&zip_path, data).unwrap();
        let lib = tmp.path().join("lib");
        let info = import_zip(None, &lib, &zip_path).unwrap();
        assert_eq!(info.name, "demo-skill");
        assert!(lib.join("demo-skill/SKILL.md").is_file());
        assert!(lib.join("demo-skill/scripts/run.sh").is_file());
    }

    #[test]
    fn import_zip_rejects_zip_slip() {
        let tmp = tempfile::tempdir().unwrap();
        let data = build_zip(&[("SKILL.md", SKILL_MD), ("../evil.txt", "x")]);
        let zip_path = tmp.path().join("evil.zip");
        std::fs::write(&zip_path, data).unwrap();
        let lib = tmp.path().join("lib");
        assert!(import_zip(None, &lib, &zip_path).is_err());
        assert!(!tmp.path().join("evil.txt").exists());
    }

    #[test]
    fn list_skill_files_shows_relative_paths_and_skips_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("SKILL.md"), SKILL_MD).unwrap();
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/run.sh"), "echo hi").unwrap();
        std::fs::write(dir.join(".hidden"), "x").unwrap();
        let files = list_skill_files(dir);
        assert!(files.contains(&"scripts/".to_string()));
        assert!(files.contains(&std::path::Path::new("scripts").join("run.sh").to_string_lossy().to_string()));
        assert!(!files.iter().any(|f| f.contains("SKILL.md") || f.contains("hidden")));
    }
}
