//! Registry of the system prompts behind the batch features ("领域智能体").
//!
//! Until now these lived as `format!` literals inside `review_gen` / `ai_table`
//! / `ai_classify` / `research_service`: they decide how a 综述 is written, what
//! an AI 表格列 cell contains, how papers get classified — and the user had no
//! way to see or tune them. This module makes them **one list** that the
//! settings page can render and the call sites can read through
//! [`effective`], with a settings override (`prompt.<id>`) that falls back to
//! the built-in default.
//!
//! Design rules:
//! * The default text here is byte-for-byte what the code used before — moving
//!   a prompt into the registry must not change behaviour.
//! * Only variables that are safe to relocate are placeholders (`{dimension}`,
//!   `{max_groups}`, …). JSON schema fragments are NOT placeholders: a user can
//!   rewrite them, and `affects_parsing` warns that doing so breaks parsing.
//! * [`render`] leaves unknown `{…}` sequences untouched, so JSON braces in the
//!   prompt survive and a deleted placeholder never silently blanks content.

use serde::Serialize;
use sqlx::SqlitePool;

pub const REVIEW_OUTLINE: &str = "review.outline";
pub const REVIEW_SECTION: &str = "review.section";
pub const AI_TABLE_EXTRACT: &str = "ai_table.extract";
pub const CLASSIFY_DRAFT: &str = "classify.draft";
pub const CLASSIFY_MERGE: &str = "classify.merge";
pub const CLASSIFY_HIERARCHIZE: &str = "classify.hierarchize";
pub const CLASSIFY_ASSIGN: &str = "classify.assign";
pub const CLASSIFY_CLUSTER_NAMING: &str = "classify.cluster_naming";
pub const RESEARCH_RELEVANCE: &str = "research.relevance";
pub const RESEARCH_KEYWORDS: &str = "research.keywords";

/// A variable the feature substitutes into the prompt at call time.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Placeholder {
    /// Token name, i.e. `dimension` for `{dimension}`.
    pub name: &'static str,
    pub description: &'static str,
}

pub struct TaskPrompt {
    pub id: &'static str,
    /// Feature the prompt belongs to ("AI 综述" / "AI 表格列" / …).
    pub group: &'static str,
    pub name: &'static str,
    /// When this prompt is used (每篇 / 每节 / 每批 …).
    pub description: &'static str,
    pub default: &'static str,
    pub placeholders: &'static [Placeholder],
    /// True when rewriting the JSON contract in this prompt breaks the feature.
    pub affects_parsing: bool,
}

/// Hard ceiling for any per-call output budget. Providers cap lower on their
/// own; beyond this the feature stops raising and the user should switch models.
pub const OUTPUT_CAP: u32 = 32768;

/// A feature group that has its own output-budget knob ("单轮输出上限").
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptGroup {
    pub id: &'static str,
    pub name: &'static str,
    /// What the budget governs, shown under the input.
    pub description: &'static str,
    /// The budget the code uses when the user leaves the field empty.
    pub builtin_default: u32,
}

pub const GROUPS: &[PromptGroup] = &[
    PromptGroup {
        id: "review",
        name: "AI 综述",
        description: "每节一次调用的输出预算；实际额度取「按目标字数算出的值」与此值的较大者",
        builtin_default: 7000,
    },
    PromptGroup {
        id: "ai_table",
        name: "AI 表格列",
        description: "每篇一次调用要输出整行所有列；实际额度取 6000 与此值的较大者",
        builtin_default: 6000,
    },
    PromptGroup {
        id: "classify",
        name: "AI 文献分类",
        description: "起草/归并/判定每次调用的输出预算；实际额度取 12000 与此值的较大者",
        builtin_default: 12000,
    },
    PromptGroup {
        id: "research",
        name: "科研追踪",
        description: "后台自动发现时的输出预算（模型端默认太小时可调大）",
        builtin_default: 4096,
    },
];

/// Settings key holding a user's output-budget floor for a feature group.
pub fn budget_key(id: &str) -> String {
    format!("budget.{id}")
}

/// Parse a stored budget value. Out-of-range values are clamped, junk ignored.
fn parse_budget(raw: Option<&str>) -> Option<u32> {
    let n: u32 = raw?.trim().parse().ok()?;
    if n == 0 {
        return None;
    }
    Some(n.clamp(256, OUTPUT_CAP))
}

/// The user's output-budget floor for a group, if they set one.
pub async fn budget_floor(db: &SqlitePool, group_id: &str) -> Option<u32> {
    let raw = crate::core::settings_service::get_setting(db, &budget_key(group_id))
        .await
        .ok()
        .flatten();
    parse_budget(raw.as_deref())
}

/// Effective budget for one call: never lower than what the feature computed,
/// never above the hard cap. A floor only *raises* the budget — lowering it
/// would silently truncate sections and cells.
pub fn apply_floor(computed: u32, floor: Option<u32>) -> u32 {
    let base = computed.min(OUTPUT_CAP);
    match floor {
        Some(f) => base.max(f.min(OUTPUT_CAP)),
        None => base,
    }
}

/// Settings key holding a user override for this prompt.
pub fn setting_key(id: &str) -> String {
    format!("prompt.{id}")
}

pub const PROMPTS: &[TaskPrompt] = &[
    TaskPrompt {
        id: REVIEW_OUTLINE,
        group: "AI 综述",
        name: "大纲设计",
        description: "生成综述目录时调用一次",
        default: "你是学术写作助手，擅长设计文献综述的目录框架。只输出 JSON 数组，每个元素为 {\"title\": \"章节标题\", \"points\": \"该节应覆盖的要点（一句话）\", \"subsections\": [\"小节标题\", ...]}；内容较多的章节设计 2-4 个小节，引言、总结/展望等简短章节的小节数组可为空。不要输出 JSON 以外的内容。",
        placeholders: &[],
        affects_parsing: true,
    },
    TaskPrompt {
        id: REVIEW_SECTION,
        group: "AI 综述",
        name: "分节撰写",
        description: "撰写每一节时调用一次（正文流式输出）",
        default: "你是学术写作助手，正在撰写文献综述的一节。规则：\n\
1. 只依据「参考段落」写作，不得编造文献中没有的事实；\n\
2. 引用事实必须在句末标注【证据编号】，格式如 [E3]（E 后面的数字就是「参考段落」前的编号）；同一处事实可引多条证据，写成 [E3][E7]；只能使用「参考段落」中出现过的编号，绝不自行编号或改写编号体系；\n\
3. 使用{language}写作，学术语体，本节约 {target_words} 字；\n\
4. 直接输出本节正文，不要输出章节标题，不要解释。{subsection_rule}",
        placeholders: &[
            Placeholder { name: "language", description: "写作语言（中文 / English）" },
            Placeholder { name: "target_words", description: "本节的建议字数" },
            Placeholder { name: "subsection_rule", description: "有小节时追加的组织规则；没有小节时为空" },
        ],
        affects_parsing: false,
    },
    TaskPrompt {
        id: AI_TABLE_EXTRACT,
        group: "AI 表格列",
        name: "字段抽取",
        description: "每篇文献调用一次，一次返回所有列",
        default: "你是学术文献信息抽取引擎。根据给定的文献信息与段落，按用户列出的字段逐一抽取。只输出一个 JSON 对象：键为字段名，值为字符串；{detail_rule}。若某个字段的抽取说明另有长度或详略要求，以该说明为准。文献中确实没有的字段值用空字符串。不要输出 JSON 以外的任何内容。",
        placeholders: &[
            Placeholder { name: "detail_rule", description: "「详细程度」下拉对应的长度规则（简洁 / 标准 / 详细）" },
        ],
        affects_parsing: true,
    },
    TaskPrompt {
        id: CLASSIFY_DRAFT,
        group: "AI 文献分类",
        name: "批量起草",
        description: "起草分类体系时，每 24 篇调用一次",
        default: "你是学术文献分类助手。请把给定的一批文献按「{dimension}」这个维度分成若干互斥的类目，类目数量控制在 3~{max_groups} 个之间；同类目必须能用一句话说清界定。只输出一个 JSON 对象，格式：{\"groups\":[{\"name\":\"类目名（简短，不超过 12 字）\",\"description\":\"一句话界定\",\"papers\":[{\"paper\":\"文献 id\",\"confidence\":0.0~1.0,\"reason\":\"归类依据（不超过 20 字）\"}]}]}。每篇文献必须且只能出现在一个类目里；不要输出 JSON 以外的任何内容。",
        placeholders: &[
            Placeholder { name: "dimension", description: "分类维度（研究主题 / 研究方法 …）" },
            Placeholder { name: "max_groups", description: "类目数上限" },
        ],
        affects_parsing: true,
    },
    TaskPrompt {
        id: CLASSIFY_MERGE,
        group: "AI 文献分类",
        name: "类目归并",
        description: "类目数超过上限时调用一次，合并近义类目",
        default: "你是学术文献分类专家。下面是一批由不同批次分别提出的类目，存在近义与重叠。请归并成不超过 {max_groups} 个类目，且不丢失任何原类目的语义。只输出 JSON：{\"groups\":[{\"name\":\"归并后的类目名\",\"description\":\"一句话界定\",\"from\":[\"被归并的原类目名\",……]}]}。每个原类目都必须出现在某个 from 里。",
        placeholders: &[Placeholder { name: "max_groups", description: "归并后的类目数上限" }],
        affects_parsing: true,
    },
    TaskPrompt {
        id: CLASSIFY_HIERARCHIZE,
        group: "AI 文献分类",
        name: "二级归纳",
        description: "开启「两级类目」时调用一次，把扁平类目归纳成父子结构",
        default: "你是学术文献分类专家。下面是同一维度下的一批扁平类目，请把它们归纳成不超过 {max_parents} 个上位父类，每个父类下挂 2~5 个子类；父类名也要简短（不超过 10 字）。每个子类必须且只能出现在一个父类里，且必须使用原文的类目名（一字不差）。只输出 JSON：{\"parents\":[{\"name\":\"父类名\",\"description\":\"一句话界定\",\"children\":[\"子类名\",……]}]}。",
        placeholders: &[Placeholder { name: "max_parents", description: "父类数量上限（当前固定为 6）" }],
        affects_parsing: true,
    },
    TaskPrompt {
        id: CLASSIFY_ASSIGN,
        group: "AI 文献分类",
        name: "闭集判定",
        description: "把文献落到已确认的类目上，每 20 篇调用一次",
        default: "你是学术文献分类助手。{label_spec}。类目只能取自下面的固定清单，不得新增、不得改名。只输出 JSON：{output_spec}。\n\n【固定类目清单】（分类维度：{dimension}）\n{taxonomy}",
        placeholders: &[
            Placeholder { name: "label_spec", description: "单标签/多标签的规则说明（按归类目标自动切换）" },
            Placeholder { name: "output_spec", description: "该模式下的 JSON 输出格式" },
            Placeholder { name: "dimension", description: "分类维度" },
            Placeholder { name: "taxonomy", description: "已确认的类目清单（编号列表）" },
        ],
        affects_parsing: true,
    },
    TaskPrompt {
        id: CLASSIFY_CLUSTER_NAMING,
        group: "AI 文献分类",
        name: "语义簇命名",
        description: "语义聚类模式下调用一次，为每个簇起类目名",
        default: "你是学术文献分类专家。下面是把文献按语义向量聚成若干簇后，每个簇里最具代表性的文献标题。请为每个簇起一个「{dimension}」维度的类目名（简短，不超过 12 字），并给一句话界定。只输出 JSON：{\"clusters\":[{\"index\":<簇编号>,\"name\":\"类目名\",\"description\":\"一句话界定\"}]}。不要输出 JSON 以外的任何内容。",
        placeholders: &[Placeholder { name: "dimension", description: "分类维度" }],
        affects_parsing: true,
    },
    TaskPrompt {
        id: RESEARCH_RELEVANCE,
        group: "科研追踪",
        name: "相关性评估",
        description: "自动发现新文献时，为一批候选打分",
        default: "你是文献相关性评估助手，只输出 JSON。",
        placeholders: &[],
        affects_parsing: true,
    },
    TaskPrompt {
        id: RESEARCH_KEYWORDS,
        group: "科研追踪",
        name: "检索关键词",
        description: "为课题生成英文检索词时调用一次",
        default: "你是文献检索助手，只输出检索关键词。",
        placeholders: &[],
        affects_parsing: false,
    },
];

pub fn get(id: &str) -> Option<&'static TaskPrompt> {
    PROMPTS.iter().find(|p| p.id == id)
}

/// Effective prompt: settings override (non-empty) else the built-in default.
/// A DB error degrades to the default rather than failing the feature.
pub async fn effective(db: &SqlitePool, id: &str) -> String {
    let default = get(id).map(|p| p.default.to_string()).unwrap_or_default();
    match crate::core::settings_service::get_setting(db, &setting_key(id)).await {
        Ok(Some(v)) if !v.trim().is_empty() => v,
        _ => default,
    }
}

/// Substitute `{name}` placeholders. Unknown `{…}` sequences (JSON braces!) are
/// left alone, and a placeholder the user deleted simply never matches.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in vars {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_prompt_is_well_formed() {
        let mut ids = std::collections::HashSet::new();
        for p in PROMPTS {
            assert!(!p.id.is_empty());
            assert!(ids.insert(p.id), "duplicate prompt id {}", p.id);
            assert!(!p.name.is_empty(), "{} has no name", p.id);
            assert!(!p.group.is_empty(), "{} has no group", p.id);
            assert!(!p.default.trim().is_empty(), "{} has an empty default", p.id);
            // Every declared placeholder must actually appear in the text.
            for ph in p.placeholders {
                assert!(
                    p.default.contains(&format!("{{{}}}", ph.name)),
                    "{} declares placeholder {} but does not use it",
                    p.id,
                    ph.name
                );
            }
        }
    }

    #[test]
    fn parsing_sensitive_prompts_mention_json() {
        // The UI relies on this to warn before a user deletes the contract.
        for p in PROMPTS.iter().filter(|p| p.affects_parsing) {
            assert!(
                p.default.contains("JSON"),
                "{} is marked affects_parsing but never mentions JSON",
                p.id
            );
        }
    }

    #[test]
    fn render_substitutes_known_tokens_only() {
        let out = render("维度「{dimension}」，上限 {max_groups}，JSON：{\"a\":1}", &[
            ("dimension", "研究主题"),
            ("max_groups", "12"),
        ]);
        assert_eq!(out, "维度「研究主题」，上限 12，JSON：{\"a\":1}");
    }

    #[test]
    fn render_leaves_deleted_placeholders_alone() {
        // A user removed `{max_groups}` from the template: nothing to replace,
        // and no stray blank appears where it used to be.
        let out = render("维度「{dimension}」", &[("dimension", "方法"), ("max_groups", "8")]);
        assert_eq!(out, "维度「方法」");
    }

    #[test]
    fn render_does_not_touch_json_braces() {
        let out = render("{\"groups\":[{\"name\":\"x\"}]}", &[("dimension", "d")]);
        assert_eq!(out, "{\"groups\":[{\"name\":\"x\"}]}");
    }

    #[test]
    fn budget_parsing_clamps_and_ignores_junk() {
        assert_eq!(parse_budget(Some("16000")), Some(16000));
        assert_eq!(parse_budget(Some("  8000 ")), Some(8000));
        assert_eq!(parse_budget(Some("0")), None);
        assert_eq!(parse_budget(Some("abc")), None);
        assert_eq!(parse_budget(None), None);
        // Below the floor / above the cap.
        assert_eq!(parse_budget(Some("10")), Some(256));
        assert_eq!(parse_budget(Some("999999")), Some(OUTPUT_CAP));
    }

    #[test]
    fn floor_only_raises_the_budget() {
        assert_eq!(apply_floor(7000, None), 7000);
        assert_eq!(apply_floor(7000, Some(16000)), 16000);
        // A floor below the computed need never shrinks it.
        assert_eq!(apply_floor(20000, Some(8000)), 20000);
        // The hard cap wins over both.
        assert_eq!(apply_floor(40000, Some(99999)), OUTPUT_CAP);
    }

    #[test]
    fn budget_keys_are_namespaced() {
        assert_eq!(budget_key("review"), "budget.review");
    }

    #[test]
    fn setting_keys_are_namespaced() {
        assert_eq!(setting_key(CLASSIFY_DRAFT), "prompt.classify.draft");
    }
}
