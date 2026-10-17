//! Built-in column presets for AI 表格列.
//!
//! These are *read-only* starting points, deliberately not rows in
//! `ai_table_presets`: the user's own presets stay theirs, and a built-in can be
//! rewritten/improved in a release without a migration. Picking one just fills
//! the column editor, which the user then edits and (optionally) saves as their
//! own preset.
//!
//! The prompts are written for the extraction engine, which reads the column
//! prompt as *both* the field definition and the retrieval query inside each
//! paper — so every prompt names the concrete things to look for.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinColumn {
    pub key: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinPreset {
    pub id: &'static str,
    pub name: &'static str,
    /// One line on when this preset fits.
    pub description: &'static str,
    pub columns: Vec<BuiltinColumn>,
}

fn preset(id: &'static str, name: &'static str, description: &'static str, cols: &[(&str, &str)]) -> BuiltinPreset {
    BuiltinPreset {
        id,
        name,
        description,
        columns: cols
            .iter()
            .map(|(k, p)| BuiltinColumn { key: (*k).to_string(), prompt: (*p).to_string() })
            .collect(),
    }
}

/// CCF-A / 顶刊精读：把一篇论文按审稿人视角拆开。
const CS_TOP_VENUE: &[(&str, &str)] = &[
    (
        "研究问题",
        "这篇工作要解决的具体问题是什么？给出问题定义与形式化描述（若原文有）",
    ),
    (
        "动机缺口",
        "现有方法的具体不足（gap）：作者声称哪些做法做不到、代价是什么、为什么之前没人解决",
    ),
    (
        "核心洞察",
        "作者的关键 idea / 直觉是什么（一句话说明为什么这个做法会奏效）",
    ),
    ("方法名称", "作者给方法起的名字（缩写或全称）"),
    (
        "方法要点",
        "方法的技术路线：组件、关键设计、与前人做法在机制上的差别",
    ),
    (
        "技术贡献",
        "作者声明的贡献点，分点列出（一般 3 条），每条一句话",
    ),
    (
        "实验设置",
        "数据集/基准（名称与规模）、评测指标、训练或运行环境等关键设定",
    ),
    ("对比基线", "对比了哪些基线方法，是否有强基线或与自身变体的对比"),
    (
        "主要结果",
        "核心实验结论，务必带上具体数字（相对提升幅度、绝对值），并说明在哪些指标上最好",
    ),
    (
        "消融与有效性威胁",
        "消融实验验证了哪些组件；作者讨论的有效性威胁（threats to validity）",
    ),
    ("局限与未来工作", "作者承认的局限，以及提出的后续方向"),
    ("开源情况", "代码、数据、模型是否开源/公开，是否有链接或匿名仓库"),
    (
        "可借鉴点",
        "从写作与实验设计角度，这篇最值得借鉴的做法（图表设计、论证结构、实验安排）",
    ),
];

/// 系统与工程实证：搜 SE/系统类实证论文常看的那几项。
const SE_EMPIRICAL: &[(&str, &str)] = &[
    ("研究问题", "论文提出的研究问题（RQ1/RQ2…）原文表述"),
    (
        "研究对象",
        "研究的系统、工具、工件或开发者群体是什么（名称、版本、规模）",
    ),
    (
        "数据与样本",
        "数据来源（仓库/问卷/访谈/日志）、样本量与抽样方式、时间跨度",
    ),
    ("分析方法", "定量/定性方法、统计检验、编码方案与信度处理"),
    (
        "主要发现",
        "按研究问题给出结论，带关键统计量（比例、均值、p 值、效应量若有）",
    ),
    (
        "与工业实践的关系",
        "结论对工程实践或工具设计的具体启示，是否有落地案例或作者单位背景",
    ),
    ("有效性威胁", "构念效度、内部效度、外部效度方面的威胁与作者的处理"),
    ("可复现性", "是否公开数据/脚本/工具，是否提供复制包或在线附录"),
];

/// 通用实证（社科/医学/管理）。
const GENERAL_EMPIRICAL: &[(&str, &str)] = &[
    ("研究问题与假设", "研究问题与（若有）明确假设，含方向性预期"),
    ("研究设计", "横断面/纵向/实验/RCT/案例研究，以及设计上的关键选择"),
    (
        "样本与数据来源",
        "抽样人群或数据来源、样本量、纳排标准、时间与地区",
    ),
    ("变量与测量", "自变量、因变量、控制变量及其测量工具/量表"),
    ("分析方法", "统计或质性分析方法、模型设定、稳健性检验"),
    (
        "主要结论",
        "结果与结论，带关键系数、效应量、置信区间或显著性水平",
    ),
    ("局限", "作者指出的局限，以及结论可外推的范围"),
];

const DATA_TYPE_AI: &[(&str, &str)] = &[
    ("模型与规模", "使用了哪些模型、参数量或规模、是否微调/对齐方法"),
    ("训练数据", "训练数据来源、规模、清洗与筛选策略、是否含合成数据"),
    ("评测基准", "评测用 benchmark、指标与评测协议（prompt/温度/是否多次采样）"),
    ("主要结论", "在各类任务上的表现与关键数字，及失败模式"),
    ("资源与成本", "训练/推理的算力、显存、时延或成本数据（若有）"),
];

pub fn builtins() -> Vec<BuiltinPreset> {
    vec![
        preset(
            "builtin-cs-top-venue",
            "计算机 · 顶会顶刊精读",
            "按审稿人视角拆一篇 CCF-A / 顶刊论文：动机缺口、核心洞察、贡献、实验、有效性威胁、可借鉴点",
            CS_TOP_VENUE,
        ),
        preset(
            "builtin-se-empirical",
            "计算机 · 系统与工程实证",
            "面向软件工程/系统类实证研究：研究问题、数据与样本、分析方法、发现、有效性威胁、可复现性",
            SE_EMPIRICAL,
        ),
        preset(
            "builtin-general-empirical",
            "通用 · 实证研究（社科/医学/管理）",
            "研究设计、样本、变量与测量、分析方法、效应量与局限——跨学科的通用实证读法",
            GENERAL_EMPIRICAL,
        ),
        preset(
            "builtin-llm-data",
            "AI · 数据与评测向",
            "大模型类论文的数据视角：模型与规模、训练数据、评测基准、主要结论、资源成本",
            DATA_TYPE_AI,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_has_unique_keys_and_non_empty_prompts() {
        for p in builtins() {
            assert!(!p.name.is_empty(), "{} has no name", p.id);
            assert!(!p.columns.is_empty(), "{} has no columns", p.id);
            let mut seen = std::collections::HashSet::new();
            for c in &p.columns {
                assert!(!c.key.trim().is_empty(), "{} has a blank column key", p.id);
                assert!(!c.prompt.trim().is_empty(), "{} / {} has a blank prompt", p.id, c.key);
                assert!(seen.insert(c.key.clone()), "{} has a duplicate key {}", p.id, c.key);
            }
        }
    }

    #[test]
    fn builtin_ids_are_unique() {
        let mut ids = std::collections::HashSet::new();
        for p in builtins() {
            assert!(ids.insert(p.id), "duplicate builtin id {}", p.id);
        }
    }
}
