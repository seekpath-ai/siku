//! Retrieval-term expansion for AI 表格列.
//!
//! The retrieval index is script-aware: `query::analyze_query` sends CJK terms
//! to a bigram index and ASCII terms to a trigram index. A Chinese column
//! definition ("有效性威胁方面的威胁与作者的处理") therefore matched **nothing**
//! in an English paper — the two FTS legs both returned zero hits — and the
//! model was handed only the abstract. Every field that happened to be empty was
//! usually a retrieval miss, not a missing fact.
//!
//! So each column gets a set of search terms up front:
//! * a built-in mapping from the common column slots to their English anchors
//!   (free, deterministic, covers the built-in templates),
//! * otherwise one LLM call (cached per column key, reused across papers),
//! * otherwise the ASCII words already present in the column text.
//!
//! Terms are prepared once per run, never per paper.

use std::collections::HashMap;

use serde::Deserialize;
use sqlx::SqlitePool;

use crate::ai::llm::LlmClient;

/// Column-slot → English anchors. Matching is a substring test on the column
/// key + prompt, so a user's own wording ("效度威胁", "是否开源") still lands in
/// the right slot.
const SLOTS: &[(&[&str], &[&str])] = &[
    (
        &["研究问题", "假设", "问题是什么", "研究目标"],
        &["research question", "we investigate", "we aim to", "we address", "RQ1", "RQ2", "hypothesis"],
    ),
    (
        &["研究对象", "系统", "工具", "工件", "开发者", "方法名称"],
        &["system", "tool", "framework", "approach", "developer", "artifact", "implementation"],
    ),
    (
        &["数据", "样本", "来源", "抽样", "语料", "数据集"],
        &["dataset", "benchmark", "sample", "participants", "corpus", "data collection", "repository"],
    ),
    (
        &["分析", "方法", "设计", "实验", "度量", "指标"],
        &["methodology", "experimental setup", "evaluation", "metrics", "statistical test", "experiment", "measurement"],
    ),
    (
        &["结果", "发现", "结论", "效果", "性能"],
        &["results", "findings", "improvement", "performance", "accuracy", "outperform", "we observe"],
    ),
    (
        &["工业", "实践", "启示", "落地", "应用价值"],
        &["implications", "practice", "practitioners", "industrial", "deployment", "real-world"],
    ),
    (
        &["威胁", "效度", "局限", "不足"],
        &["threats to validity", "limitations", "internal validity", "external validity", "construct validity", "generalizability"],
    ),
    (
        &["复现", "开源", "公开", "可重复", "可用性"],
        &["replication package", "artifact", "open source", "code available", "data available", "reproducibility"],
    ),
    (
        &["背景", "动机", "相关工作", "缺口"],
        &["background", "motivation", "related work", "research gap", "prior work"],
    ),
    (
        &["贡献", "创新", "洞察"],
        &["contribution", "novelty", "key insight", "we propose"],
    ),
];

/// Terms implied by the column's own wording. Empty when nothing matched.
pub fn slot_terms(column_key: &str, prompt: &str) -> Vec<String> {
    let haystack = format!("{column_key} {prompt}").to_lowercase();
    // Best match wins, not first match: "可复现性 / 是否公开数据" mentions both
    // 复现 and 数据, and the reproduction slot is the one it is about.
    let mut best: Option<(usize, &[&str])> = None;
    for (needles, anchors) in SLOTS {
        let hits = needles
            .iter()
            .filter(|n| haystack.contains(&n.to_lowercase()))
            .count();
        if hits == 0 {
            continue;
        }
        if best.map(|(b, _)| hits > b).unwrap_or(true) {
            best = Some((hits, anchors));
        }
    }
    best.map(|(_, anchors)| anchors.iter().map(|a| (*a).to_string()).collect())
        .unwrap_or_default()
}

/// ASCII words already present in the column text — the last-resort fallback so
/// the trigram leg still gets something ("RQ2", "Diff BLEU", "LLM").
fn ascii_tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
        let w = word.trim();
        if w.len() >= 2 && w.chars().any(|c| c.is_ascii_alphabetic()) {
            let lower = w.to_lowercase();
            if !out.iter().any(|x| x == &lower) {
                out.push(lower);
            }
        }
    }
    out.truncate(8);
    out
}

#[derive(Deserialize)]
struct RawTerms {
    #[serde(default)]
    terms: Vec<String>,
}

fn parse_terms(content: &str) -> Vec<String> {
    let t = content.trim();
    let clean = |v: Vec<String>| -> Vec<String> {
        v.into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    // Object form: {"terms":[...]}
    if let (Some(s), Some(e)) = (t.find('{'), t.rfind('}')) {
        if e > s {
            if let Ok(raw) = serde_json::from_str::<RawTerms>(&t[s..=e]) {
                let terms = clean(raw.terms);
                if !terms.is_empty() {
                    return terms;
                }
            }
        }
    }
    // Bare array form: ["a","b"]
    if let (Some(s), Some(e)) = (t.find('['), t.rfind(']')) {
        if e > s {
            if let Ok(list) = serde_json::from_str::<Vec<String>>(&t[s..=e]) {
                return clean(list);
            }
        }
    }
    Vec::new()
}

/// A column's stored search terms plus where they came from, so the dialog can
/// show "内置映射 / AI 生成 / 自定义" instead of a mystery list.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnTerms {
    pub column_key: String,
    pub terms: Vec<String>,
    /// slot | ai | ascii | manual ("" when nothing is stored yet)
    pub source: String,
    pub updated_at: String,
}

/// Source that survives everything: a hand-written term set is the user's final
/// word, so no automatic path may replace it.
const MANUAL: &str = "manual";

async fn cached(db: &SqlitePool, key: &str) -> Option<(Vec<String>, String)> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT terms_json, source FROM ai_table_term_cache WHERE column_key = ?",
    )
    .bind(key)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let (json, source) = row?;
    let terms: Vec<String> = serde_json::from_str(&json).ok()?;
    if terms.is_empty() {
        None
    } else {
        Some((terms, source))
    }
}

async fn store(db: &SqlitePool, key: &str, terms: &[String], source: &str) {
    let Ok(json) = serde_json::to_string(terms) else {
        return;
    };
    // The `WHERE` clause keeps a manual override in place when a later run
    // recomputes the slot/AI terms for the same column key.
    let _ = sqlx::query(
        "INSERT INTO ai_table_term_cache (column_key, terms_json, source, updated_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (column_key) DO UPDATE SET terms_json = excluded.terms_json,
           source = excluded.source, updated_at = excluded.updated_at
         WHERE ai_table_term_cache.source <> 'manual'",
    )
    .bind(key)
    .bind(&json)
    .bind(source)
    .bind(crate::core::time::now_iso())
    .execute(db)
    .await;
}

/// Stored terms for a set of columns, read-only (never calls the LLM) — this is
/// what the column editor shows before a run.
pub async fn terms_of(db: &SqlitePool, keys: &[String]) -> Vec<ColumnTerms> {
    let mut out = Vec::new();
    for key in keys {
        match cached(db, key).await {
            Some((terms, source)) => out.push(ColumnTerms {
                column_key: key.clone(),
                terms,
                source,
                updated_at: sqlx::query_scalar::<_, String>(
                    "SELECT updated_at FROM ai_table_term_cache WHERE column_key = ?",
                )
                .bind(key)
                .fetch_optional(db)
                .await
                .ok()
                .flatten()
                .unwrap_or_default(),
            }),
            None => out.push(ColumnTerms {
                column_key: key.clone(),
                terms: Vec::new(),
                source: String::new(),
                updated_at: String::new(),
            }),
        }
    }
    out
}

/// Hand-written terms for a column. Empty list resets to automatic behaviour.
pub async fn set_terms(db: &SqlitePool, key: &str, terms: &[String]) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("列名为空".to_string());
    }
    let clean: Vec<String> = terms
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if clean.is_empty() {
        return reset_terms(db, key).await;
    }
    let json = serde_json::to_string(&clean).map_err(|e| format!("json: {e}"))?;
    sqlx::query(
        "INSERT INTO ai_table_term_cache (column_key, terms_json, source, updated_at)
         VALUES (?, ?, 'manual', ?)
         ON CONFLICT (column_key) DO UPDATE SET terms_json = excluded.terms_json,
           source = 'manual', updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(&json)
    .bind(crate::core::time::now_iso())
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// Drop the stored terms so the next run recomputes them from scratch.
pub async fn reset_terms(db: &SqlitePool, key: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM ai_table_term_cache WHERE column_key = ?")
        .bind(key.trim())
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// Recompute terms now (one LLM call when no slot mapping matches).
pub async fn regenerate(
    db: &SqlitePool,
    llm: Option<&dyn LlmClient>,
    key: &str,
    prompt: &str,
) -> Result<Vec<String>, String> {
    reset_terms(db, key).await?;
    Ok(column_terms(db, llm, key.trim(), prompt, true).await)
}

/// Search terms for one column, in priority order: the user's own override, the
/// user-visible slot mapping, the cache, one LLM call (when allowed), the
/// column's own ASCII tokens.
pub async fn column_terms(
    db: &SqlitePool,
    llm: Option<&dyn LlmClient>,
    column_key: &str,
    prompt: &str,
    allow_ai: bool,
) -> Vec<String> {
    if let Some((terms, source)) = cached(db, column_key).await {
        if source == MANUAL {
            return terms;
        }
    }
    let slot = slot_terms(column_key, prompt);
    if !slot.is_empty() {
        store(db, column_key, &slot, "slot").await;
        return slot;
    }
    if let Some((terms, _)) = cached(db, column_key).await {
        return terms;
    }
    if allow_ai {
        if let Some(client) = llm {
            let system = "你是学术检索助手。给定一个文献抽取字段，给出 3~6 个用于在该领域英文论文中检索相关段落的英文关键词/短语。\
                 只输出 JSON：{\"terms\":[\"…\"]}，不要解释。";
            let user = format!("字段名：{column_key}\n字段说明：{}", prompt.trim());
            let messages = vec![
                crate::ai::llm::ChatMessage {
                    reasoning_content: None,
                    role: "system".into(),
                    content: system.into(),
                    attachments: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                crate::ai::llm::ChatMessage {
                    reasoning_content: None,
                    role: "user".into(),
                    content: user,
                    attachments: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            ];
            if let Ok(resp) = client.chat_completion(&messages, &[]).await {
                let terms = parse_terms(&resp.content);
                if !terms.is_empty() {
                    store(db, column_key, &terms, "ai").await;
                    return terms;
                }
            }
        }
    }
    let fallback = ascii_tokens(&format!("{column_key} {prompt}"));
    if !fallback.is_empty() {
        store(db, column_key, &fallback, "ascii").await;
    }
    fallback
}

/// Terms for the whole column set, keyed by column key. One preparation pass per
/// run (the AI call happens once per column, not once per paper).
pub async fn prepare(
    db: &SqlitePool,
    llm: Option<&dyn LlmClient>,
    columns: &[(String, String)],
    allow_ai: bool,
) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    for (key, prompt) in columns {
        let terms = column_terms(db, llm, key, prompt, allow_ai).await;
        out.insert(key.clone(), terms);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_mapping_covers_the_builtin_templates() {
        assert!(slot_terms("有效性威胁", "构念效度、内部效度、外部效度方面的威胁")
            .iter()
            .any(|t| t == "threats to validity"));
        assert!(slot_terms("可复现性", "是否公开数据/脚本/工具")
            .iter()
            .any(|t| t.contains("replication package")));
        assert!(slot_terms("数据与样本", "数据来源、样本量与抽样方式")
            .iter()
            .any(|t| t == "dataset"));
        assert!(slot_terms("研究问题", "RQ1/RQ2 原文表述")
            .iter()
            .any(|t| t == "research question"));
    }

    #[test]
    fn unknown_columns_have_no_slot_terms() {
        assert!(slot_terms("我的自定义列", "随便写点什么").is_empty());
    }

    #[test]
    fn ascii_tokens_pick_real_words_only() {
        let t = ascii_tokens("用 Diff BLEU 与 RQ2 评估 (LLM)");
        assert!(t.contains(&"diff".to_string()), "{t:?}");
        assert!(t.contains(&"bleu".to_string()), "{t:?}");
        assert!(t.contains(&"rq2".to_string()), "{t:?}");
        assert!(!t.contains(&"llm".to_string()) || t.iter().all(|x| x.len() >= 2));
    }

    #[test]
    fn parse_terms_accepts_object_and_array() {
        assert_eq!(parse_terms("{\"terms\":[\"a b\",\"c\"]}"), vec!["a b", "c"]);
        assert_eq!(parse_terms("[\"a\",\"b\"]"), vec!["a", "b"]);
        assert!(parse_terms("没有 JSON").is_empty());
    }

    async fn test_db() -> SqlitePool {
        use sqlx::sqlite::SqlitePoolOptions;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        sqlx::raw_sql(
            "CREATE TABLE ai_table_term_cache (
                column_key TEXT PRIMARY KEY NOT NULL,
                terms_json TEXT NOT NULL DEFAULT '[]',
                source TEXT NOT NULL DEFAULT 'ai',
                updated_at TEXT NOT NULL
            );",
        )
        .execute(&pool)
        .await
        .expect("schema");
        pool
    }

    #[tokio::test]
    async fn manual_terms_beat_the_slot_mapping_and_ai_cache() {
        let db = test_db().await;
        // A column the built-in mapping knows about.
        assert!(column_terms(&db, None, "有效性威胁", "构念效度", false)
            .await
            .iter()
            .any(|t| t == "threats to validity"));

        // A hand-written set wins over that mapping on the very next run…
        set_terms(&db, "有效性威胁", &["my own term".into(), "  second  ".into()])
            .await
            .unwrap();
        assert_eq!(
            column_terms(&db, None, "有效性威胁", "构念效度", true).await,
            vec!["my own term".to_string(), "second".to_string()]
        );
        // …and keeps winning even after a later run recomputes the slot terms.
        store(&db, "有效性威胁", &["threats to validity".into()], "slot").await;
        assert_eq!(
            column_terms(&db, None, "有效性威胁", "构念效度", true).await,
            vec!["my own term".to_string(), "second".to_string()]
        );
        // The dialog sees the origin label, not a mystery list.
        let info = terms_of(&db, &["有效性威胁".to_string()]).await;
        assert_eq!(info[0].source, "manual");
        assert_eq!(info[0].terms.len(), 2);
    }

    #[tokio::test]
    async fn clearing_terms_restores_automatic_behaviour() {
        let db = test_db().await;
        set_terms(&db, "自定义列", &["alpha".into()]).await.unwrap();
        assert_eq!(
            column_terms(&db, None, "自定义列", "", false).await,
            vec!["alpha".to_string()]
        );
        // Empty list is a reset, not an empty term set.
        set_terms(&db, "自定义列", &[]).await.unwrap();
        assert!(terms_of(&db, &["自定义列".to_string()]).await[0]
            .terms
            .is_empty());
        // Slot columns fall back to their built-in mapping again.
        set_terms(&db, "可复现性", &["x".into()]).await.unwrap();
        reset_terms(&db, "可复现性").await.unwrap();
        assert!(column_terms(&db, None, "可复现性", "是否公开数据/脚本", false)
            .await
            .iter()
            .any(|t| t.contains("replication package")));
    }
}
