use sqlx::SqlitePool;

/// Built-in domain agents served by the global pet (方案2).
/// Each domain agent is scoped to a page type and has its own system prompt,
/// which can be overridden in global settings.
pub struct DomainAgent {
    pub id: &'static str,
    pub name: &'static str,
    /// Settings key holding the (overridable) prompt.
    pub prompt_setting: &'static str,
    /// Settings key controlling visibility.
    pub enabled_setting: &'static str,
    pub default_prompt: &'static str,
}

pub const NOTE_ORGANIZER_PROMPT: &str = "你是思库的内置「笔记整理」智能体，专门帮助用户整理当前这篇笔记。\
你只处理笔记相关任务：使用 note_read 读取笔记、note_write 修改笔记；不要用文件系统工具搜索笔记。\
更新已有笔记时，note_write 必须携带 note_id（即当前笔记的 id，来自用户上下文）。\
整理原则：保持原意与关键内容不丢失；改进结构（标题层级、段落、列表）；修正错别字与格式；可补充或整理标签。\
完成修改后用中文简要说明你做了哪些整理。所有修改都会记录版本历史，可随时回滚。";

pub const LITERATURE_ANALYZER_PROMPT: &str = "你是思库的内置「文献分析」智能体，专门帮助用户分析当前这篇文献。\
使用 paper_read 读取文献详情与正文、paper_search 检索文献库、translate 翻译内容。\
忠实于原文，不臆造数据；输出结构清晰的中文总结。\
需要把总结保存到笔记时使用 note_write：更新已有笔记必须携带 note_id（来自用户上下文），否则新建笔记。\
回答中涉及文献具体内容的论断时，必须在句末标注证据引用标记 [^1]、[^2]…，\
并在回复末尾输出一个 ```evidence 代码块，内容为 JSON 数组，\
每项形如 {\"n\": 编号, \"page\": 证据所在页码（整数，必填）, \"exact\": \"证据原文片段\"}。\
exact 必须是 30-80 字、逐字摘自 paper_read 返回正文的片段，不得改写或缩写，\
因为用户点击引用标记时界面会按 exact 在 PDF 原文中定位并高亮证据。\
注意：```evidence 代码块只是对话回复的专用格式，写入笔记时必须转换——\
笔记正文保留 [^1] 标记不变，但在笔记末尾为每个引用输出一条 GFM 脚注定义，形如：\
[^1]: [第 1 页](siku-reader://当前文献ID?page=1&exact=URL编码后的原文) · 「原文片段」\
其中「当前文献ID」来自用户上下文，exact 同样需要 URL 编码；\
不要把 ```evidence JSON 块写进笔记，笔记渲染端无法识别它。";

pub const RESEARCH_TRACKER_PROMPT: &str = "你是思库的内置「科研追踪」智能体，围绕当前课题工作。\
使用 paper_search 检索文献库、paper_read 阅读文献。\
帮助用户梳理课题进展、发现相关文献、总结研究现状。\
把重要发现保存到笔记时使用 note_write：更新已有笔记必须携带 note_id（来自用户上下文），否则新建笔记。";

pub const KNOWLEDGE_CURATOR_PROMPT: &str = "你是思库的内置「知识库整理」智能体，帮助用户整理当前知识条目。\
使用 knowledge_query 检索知识库、knowledge_create 创建新条目，必要时用 note_read / note_write 关联笔记。\
注意 knowledge_create 只能创建新条目、不能修改已有条目；整理已有条目时，将修订内容写入新条目并向用户说明。\
保持内容准确、结构清晰，不臆造。";

pub const CHAT_SUMMARIZER_PROMPT: &str = "你是思库的内置「对话总结」智能体，帮助用户总结当前对话。\
依据系统提示词末尾注入的最近对话内容，提炼要点、行动项与待办。\
只做总结与提炼，不要编造对话中不存在的内容。";

/// All built-in domain agents served by the global pet.
pub fn builtin_domains() -> Vec<DomainAgent> {
    vec![
        DomainAgent {
            id: "note_organizer",
            name: "笔记整理",
            prompt_setting: "pet.note_organizer.prompt",
            enabled_setting: "pet.note_organizer.enabled",
            default_prompt: NOTE_ORGANIZER_PROMPT,
        },
        DomainAgent {
            id: "literature_analyzer",
            name: "文献分析",
            prompt_setting: "pet.literature_analyzer.prompt",
            enabled_setting: "pet.literature_analyzer.enabled",
            default_prompt: LITERATURE_ANALYZER_PROMPT,
        },
        DomainAgent {
            id: "research_tracker",
            name: "科研追踪",
            prompt_setting: "pet.research_tracker.prompt",
            enabled_setting: "pet.research_tracker.enabled",
            default_prompt: RESEARCH_TRACKER_PROMPT,
        },
        DomainAgent {
            id: "knowledge_curator",
            name: "知识库整理",
            prompt_setting: "pet.knowledge_curator.prompt",
            enabled_setting: "pet.knowledge_curator.enabled",
            default_prompt: KNOWLEDGE_CURATOR_PROMPT,
        },
        DomainAgent {
            id: "chat_summarizer",
            name: "对话总结",
            prompt_setting: "pet.chat_summarizer.prompt",
            enabled_setting: "pet.chat_summarizer.enabled",
            default_prompt: CHAT_SUMMARIZER_PROMPT,
        },
    ]
}

pub fn get_domain(id: &str) -> Option<DomainAgent> {
    builtin_domains().into_iter().find(|d| d.id == id)
}

/// Effective prompt for a domain agent: settings override > built-in default.
pub async fn effective_prompt(db: &SqlitePool, id: &str) -> String {
    match get_domain(id) {
        Some(domain) => {
            match crate::core::settings_service::get_setting(db, domain.prompt_setting).await {
                Ok(Some(p)) if !p.trim().is_empty() => p,
                _ => domain.default_prompt.to_string(),
            }
        }
        None => "你是思库的智能助手，帮助用户处理当前页面上的任务。".to_string(),
    }
}

/// Whether a domain agent is enabled (defaults to enabled when unset).
pub async fn is_enabled(db: &SqlitePool, id: &str) -> bool {
    let Some(domain) = get_domain(id) else {
        return false;
    };
    match crate::core::settings_service::get_setting(db, domain.enabled_setting).await {
        Ok(Some(v)) => v != "0",
        _ => true,
    }
}
