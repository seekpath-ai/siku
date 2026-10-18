use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::ai::llm::LlmClient;
use crate::core::review_store::{
    RegisteredEvidence, ReviewEvidence, ReviewPaperCard, ReviewSource, ReviewSynthesis, SourceChunk,
    StoredSource,
};

const MIN_FULLTEXT_CHARS: usize = 200;
const CARD_CANDIDATES: usize = 12;

pub fn classify_source(chunk_count: usize, fulltext_chars: usize, abstract_chars: usize) -> &'static str {
    if chunk_count > 0 && fulltext_chars >= MIN_FULLTEXT_CHARS {
        "fulltext"
    } else if abstract_chars > 0 {
        "abstract"
    } else {
        "unusable"
    }
}

pub async fn freeze_sources(db: &SqlitePool, paper_ids: &[String]) -> Result<Vec<StoredSource>, String> {
    let mut seen = HashSet::new();
    for id in paper_ids {
        if id.trim().is_empty() {
            return Err("文献 ID 不能为空".to_string());
        }
        if !seen.insert(id) {
            return Err("文献列表包含重复项".to_string());
        }
    }
    let mut out = Vec::with_capacity(paper_ids.len());
    for (index, paper_id) in paper_ids.iter().enumerate() {
        let paper = sqlx::query_as::<_, (String, String, String, Option<i64>, Option<String>, Option<String>, Option<String>, Option<String>)>(
            "SELECT id, title, authors, year, journal, doi, url, abstract FROM papers WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(paper_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        let Some((id, title, authors, year, journal, doi, url, abstract_text)) = paper else {
            out.push(StoredSource {
                source: ReviewSource {
                    paper_id: paper_id.clone(),
                    paper_index: index + 1,
                    title: String::new(),
                    authors: "[]".into(),
                    year: None,
                    source_kind: "unusable".into(),
                    status: "failed".into(),
                    chunk_count: 0,
                    abstract_chars: 0,
                    card: None,
                    error: Some("文献不存在或已删除".into()),
                },
                journal: None,
                doi: None,
                url: None,
                abstract_text: String::new(),
                chunks: Vec::new(),
                cache_key: String::new(),
                index_fingerprint: String::new(),
            });
            continue;
        };
        let abstract_text = abstract_text.unwrap_or_default();
        let rows = sqlx::query_as::<_, (String, i32, String, Option<i32>, Option<i32>)>(
            "SELECT id, chunk_index, content, page_start, page_end FROM chunks
             WHERE paper_id = ? AND is_tail = 0 ORDER BY chunk_index, id",
        )
        .bind(&id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        let chunks: Vec<SourceChunk> = rows
            .into_iter()
            .filter(|(_, _, content, _, _)| !content.trim().is_empty())
            .map(|(id, chunk_index, content, page_start, page_end)| SourceChunk {
                id,
                chunk_index,
                content,
                page_start,
                page_end,
            })
            .collect();
        let fulltext_chars = chunks.iter().map(|c| c.content.chars().count()).sum();
        let abstract_chars = abstract_text.trim().chars().count();
        let source_kind = classify_source(chunks.len(), fulltext_chars, abstract_chars).to_string();
        let fingerprint = index_fingerprint(&chunks, &abstract_text, &source_kind);
        let status = if source_kind == "unusable" { "done" } else { "pending" };
        out.push(StoredSource {
            source: ReviewSource {
                paper_id: id,
                paper_index: index + 1,
                title,
                authors,
                year,
                source_kind,
                status: status.into(),
                chunk_count: chunks.len(),
                abstract_chars,
                card: None,
                error: None,
            },
            journal,
            doi,
            url,
            abstract_text,
            chunks,
            cache_key: String::new(),
            index_fingerprint: fingerprint,
        });
    }
    Ok(out)
}

fn index_fingerprint(chunks: &[SourceChunk], abstract_text: &str, source_kind: &str) -> String {
    let mut hasher = Sha256::new();
    for chunk in chunks {
        hasher.update(chunk.id.as_bytes());
        hasher.update(chunk.chunk_index.to_le_bytes());
        hasher.update(chunk.content.as_bytes());
        hasher.update(chunk.page_start.unwrap_or(-1).to_le_bytes());
        hasher.update(chunk.page_end.unwrap_or(-1).to_le_bytes());
    }
    if source_kind == "abstract" {
        hasher.update(abstract_text.as_bytes());
    }
    hex::encode(hasher.finalize())
}

pub fn card_cache_key(question: &str, scope: &str, fingerprint: &str, prompt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"review.card\0");
    hasher.update(question.trim().as_bytes());
    hasher.update(b"\0");
    hasher.update(scope.trim().as_bytes());
    hasher.update(b"\0");
    hasher.update(fingerprint.as_bytes());
    hasher.update(b"\0");
    hasher.update(prompt.as_bytes());
    hex::encode(hasher.finalize())
}

fn quote_anchor(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(120).collect::<String>().trim().to_string()
}

fn abstract_candidate(source: &StoredSource) -> ReviewEvidence {
    ReviewEvidence {
        id: "E1".into(),
        chunk_id: None,
        page: None,
        quote: quote_anchor(&source.abstract_text),
        content: source.abstract_text.trim().to_string(),
    }
}

fn chunk_candidate(chunk: &SourceChunk, index: usize) -> ReviewEvidence {
    ReviewEvidence {
        id: format!("E{}", index + 1),
        chunk_id: Some(chunk.id.clone()),
        page: chunk.page_start,
        quote: quote_anchor(&chunk.content),
        content: chunk.content.clone(),
    }
}

fn all_candidates(source: &StoredSource) -> Vec<ReviewEvidence> {
    if source.source.source_kind == "abstract" {
        return vec![abstract_candidate(source)];
    }
    source
        .chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| chunk_candidate(chunk, i))
        .collect()
}

async fn card_candidates(
    db: &SqlitePool,
    source: &StoredSource,
    question: &str,
    scope: &str,
) -> Result<Vec<ReviewEvidence>, String> {
    if source.source.source_kind == "abstract" {
        return Ok(vec![abstract_candidate(source)]);
    }
    if source.source.source_kind != "fulltext" {
        return Ok(Vec::new());
    }
    let query = format!("{} {} {}", question, scope, source.source.title);
    let hits = crate::ai::retriever::hybrid_search_scoped(
        db,
        &query,
        CARD_CANDIDATES,
        false,
        Some(&[source.source.paper_id.clone()]),
    )
    .await?;
    let mut selected: Vec<SourceChunk> = Vec::new();
    for hit in hits {
        if let Some(chunk) = source.chunks.iter().find(|c| c.id == hit.chunk_id) {
            if !selected.iter().any(|c| c.id == chunk.id) {
                selected.push(chunk.clone());
            }
        }
        if selected.len() >= CARD_CANDIDATES {
            break;
        }
    }
    for chunk in &source.chunks {
        if selected.len() >= CARD_CANDIDATES.min(6) {
            break;
        }
        if !selected.iter().any(|c| c.id == chunk.id) {
            selected.push(chunk.clone());
        }
    }
    Ok(selected.iter().enumerate().map(|(i, c)| chunk_candidate(c, i)).collect())
}

fn candidate_prompt(evidence: &[ReviewEvidence], source_type: &str) -> String {
    evidence
        .iter()
        .map(|ev| {
            let locator = if source_type == "abstract" {
                "摘要（无页码）".to_string()
            } else {
                format!(
                    "chunkId={}，page={}",
                    ev.chunk_id.as_deref().unwrap_or(""),
                    ev.page.map(|p| p.to_string()).unwrap_or_else(|| "无".into())
                )
            };
            format!("[{}] {}\n{}", ev.id, locator, ev.content)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn extract_object(content: &str) -> Result<&str, String> {
    let trimmed = content.trim();
    let start = trimmed.find('{').ok_or_else(|| "模型未返回 JSON 对象".to_string())?;
    let end = trimmed.rfind('}').ok_or_else(|| "模型返回的 JSON 不完整".to_string())?;
    if end < start {
        return Err("模型返回的 JSON 不完整".to_string());
    }
    Ok(&trimmed[start..=end])
}

pub fn parse_synthesis(content: &str) -> Result<ReviewSynthesis, String> {
    serde_json::from_str(extract_object(content)?).map_err(|e| format!("跨文献综合 JSON 无效：{e}"))
}

pub fn validate_synthesis(
    synthesis: &mut ReviewSynthesis,
    allowed_paper_ids: &HashSet<String>,
) -> Result<(), String> {
    for items in [
        &mut synthesis.themes,
        &mut synthesis.agreements,
        &mut synthesis.conflicts,
        &mut synthesis.gaps,
    ] {
        for item in items.iter_mut() {
            if item.paper_ids.iter().any(|id| !allowed_paper_ids.contains(id)) {
                return Err(format!("跨文献综合包含未知 paperId：{}", item.paper_ids.join(",")));
            }
            let mut seen = HashSet::new();
            item.paper_ids.retain(|id| seen.insert(id.clone()));
            item.title = item.title.trim().to_string();
            item.detail = item.detail.trim().to_string();
        }
        items.retain(|item| !item.title.is_empty() || !item.detail.is_empty());
    }
    Ok(())
}

pub fn normalize_card(
    mut raw: ReviewPaperCard,
    source: &StoredSource,
    candidates: &[ReviewEvidence],
    preserve_evidence_ids: bool,
) -> Result<ReviewPaperCard, String> {
    if source.source.source_kind == "unusable" {
        return Err("不可用文献不能生成证据卡".to_string());
    }
    let mut evidence = Vec::new();
    let mut id_map: HashMap<String, String> = HashMap::new();
    let mut seen_chunks = HashSet::new();
    for old in raw.evidence.drain(..) {
        let candidate = if source.source.source_kind == "abstract" {
            candidates.first()
        } else if old.chunk_id.is_some() {
            candidates.iter().find(|c| c.chunk_id == old.chunk_id)
        } else {
            candidates.iter().find(|c| c.id == old.id)
        };
        let Some(candidate) = candidate else { continue };
        if old.id.trim().is_empty() {
            continue;
        }
        if source.source.source_kind == "fulltext"
            && !preserve_evidence_ids
            && old.chunk_id.is_some()
            && old.chunk_id != candidate.chunk_id
        {
            continue;
        }
        let chunk_key = candidate.chunk_id.clone().unwrap_or_else(|| "abstract".into());
        if !seen_chunks.insert(chunk_key) {
            continue;
        }
        let new_id = if preserve_evidence_ids { old.id.clone() } else { candidate.id.clone() };
        id_map.insert(old.id.clone(), new_id.clone());
        let supplied_quote = old.quote.trim();
        let quote = if !supplied_quote.is_empty() && candidate.content.contains(supplied_quote) {
            supplied_quote.to_string()
        } else {
            candidate.quote.clone()
        };
        evidence.push(ReviewEvidence {
            id: new_id,
            chunk_id: candidate.chunk_id.clone(),
            page: if source.source.source_kind == "abstract" { None } else { candidate.page },
            quote,
            content: candidate.content.clone(),
        });
    }
    if evidence.is_empty() {
        return Err("卡片没有可验证的原文证据".to_string());
    }
    let valid_ids: HashSet<String> = evidence.iter().map(|e| e.id.clone()).collect();
    for finding in &mut raw.findings {
        finding.claim = finding.claim.trim().to_string();
        finding.evidence_ids = finding
            .evidence_ids
            .iter()
            .filter_map(|id| {
                if preserve_evidence_ids {
                    valid_ids.contains(id).then_some(id.clone())
                } else {
                    id_map.get(id).cloned()
                }
            })
            .collect();
        let mut seen = HashSet::new();
        finding.evidence_ids.retain(|id| seen.insert(id.clone()));
    }
    raw.findings.retain(|f| !f.claim.is_empty());
    raw.paper_id = source.source.paper_id.clone();
    raw.research_question = raw.research_question.trim().to_string();
    raw.methods = raw.methods.trim().to_string();
    raw.data_and_sample = raw.data_and_sample.trim().to_string();
    raw.limitations = raw.limitations.trim().to_string();
    raw.relevance = raw.relevance.trim().to_string();
    raw.evidence = evidence;
    raw.source_type = source.source.source_kind.clone();
    Ok(raw)
}

pub async fn generate_card(
    db: &SqlitePool,
    llm: &dyn LlmClient,
    source: &StoredSource,
    question: &str,
    scope: &str,
    prompt: &str,
) -> Result<ReviewPaperCard, String> {
    let candidates = card_candidates(db, source, question, scope).await?;
    if candidates.is_empty() {
        return Err("没有可用于卡片生成的原文证据".to_string());
    }
    let user = format!(
        "研究问题：{question}\n范围限定：{scope}\n文献 paperId：{}\n标题：{}\n来源类型：{}\n\n候选原文：\n{}",
        source.source.paper_id,
        source.source.title,
        source.source.source_kind,
        candidate_prompt(&candidates, &source.source.source_kind)
    );
    let messages = vec![
        crate::core::review_gen::chat_msg("system", prompt.to_string()),
        crate::core::review_gen::chat_msg("user", user),
    ];
    let response = llm.chat_completion(&messages, &[]).await?;
    let raw: ReviewPaperCard = serde_json::from_str(extract_object(&response.content)?)
        .map_err(|e| format!("证据卡 JSON 无效：{e}"))?;
    let mut card = normalize_card(raw, source, &candidates, false)?;
    card.edited = false;
    Ok(card)
}

pub async fn normalize_cached_card(
    db: &SqlitePool,
    run_id: &str,
    paper_id: &str,
    cache_key: &str,
    source: &StoredSource,
) -> Result<Option<ReviewPaperCard>, String> {
    let cached = crate::core::review_store::find_cached_card(db, paper_id, cache_key).await?;
    let Some(mut cached) = cached else { return Ok(None) };
    if cached.paper_id != paper_id {
        return Ok(None);
    }
    cached.edited = false;
    let candidates = all_candidates(source);
    match normalize_card(cached, source, &candidates, false) {
        Ok(card) => Ok(Some(card)),
        Err(e) => {
            tracing::warn!(run_id, paper_id, error = %e, "ignoring incompatible review card cache");
            Ok(None)
        }
    }
}

pub fn normalize_edited_card(
    card: ReviewPaperCard,
    source: &StoredSource,
) -> Result<ReviewPaperCard, String> {
    let mut card = normalize_card(card, source, &all_candidates(source), true)?;
    card.edited = true;
    Ok(card)
}

pub fn assign_global_evidence(
    mut card: ReviewPaperCard,
    source: &StoredSource,
    registry: &[RegisteredEvidence],
    preserve_existing: bool,
) -> (ReviewPaperCard, Vec<RegisteredEvidence>) {
    let mut registry = registry.to_vec();
    let mut next = registry
        .iter()
        .filter_map(|e| e.id.strip_prefix('E')?.parse::<usize>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    let mut id_map = HashMap::new();
    let old_evidence = std::mem::take(&mut card.evidence);
    for old in old_evidence {
        let existing = if preserve_existing {
            registry.iter().find(|registered| {
                registered.id == old.id
                    && registered.paper_id == source.source.paper_id
                    && registered.chunk_id == old.chunk_id
            })
        } else {
            None
        };
        let id = if let Some(existing) = existing {
            existing.id.clone()
        } else {
            let id = format!("E{next}");
            next += 1;
            registry.push(RegisteredEvidence {
                id: id.clone(),
                paper_id: source.source.paper_id.clone(),
                paper_index: source.source.paper_index,
                source_type: source.source.source_kind.clone(),
                chunk_id: old.chunk_id.clone(),
                page: old.page,
                quote: old.quote.clone(),
                content: old.content.clone(),
            });
            id
        };
        id_map.insert(old.id.clone(), id.clone());
        card.evidence.push(ReviewEvidence { id, ..old });
    }
    for finding in &mut card.findings {
        finding.evidence_ids = finding
            .evidence_ids
            .iter()
            .filter_map(|id| id_map.get(id).cloned())
            .collect();
    }
    (card, registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(kind: &str) -> StoredSource {
        StoredSource {
            source: ReviewSource {
                paper_id: "p1".into(),
                paper_index: 1,
                title: "标题".into(),
                authors: "[]".into(),
                year: None,
                source_kind: kind.into(),
                status: "pending".into(),
                chunk_count: 1,
                abstract_chars: 20,
                card: None,
                error: None,
            },
            journal: None,
            doi: None,
            url: None,
            abstract_text: "这是完整摘要，只能作为摘要级证据。".into(),
            chunks: vec![SourceChunk {
                id: "c1".into(),
                chunk_index: 0,
                content: "这是正文 chunk 中的真实原文，包含方法、样本和研究发现。".into(),
                page_start: Some(3),
                page_end: Some(3),
            }],
            cache_key: String::new(),
            index_fingerprint: String::new(),
        }
    }

    #[test]
    fn source_classification_uses_body_text_and_abstract_fallback() {
        assert_eq!(classify_source(2, 1000, 0), "fulltext");
        assert_eq!(classify_source(1, 20, 100), "abstract");
        assert_eq!(classify_source(0, 0, 100), "abstract");
        assert_eq!(classify_source(0, 0, 0), "unusable");
    }

    #[test]
    fn abstract_evidence_never_gets_a_chunk_or_page() {
        let stored = source("abstract");
        let raw = ReviewPaperCard {
            paper_id: "wrong".into(),
            evidence: vec![ReviewEvidence {
                id: "E1".into(),
                chunk_id: Some("fake-pdf-chunk".into()),
                page: Some(99),
                quote: "完整摘要".into(),
                content: "伪造内容".into(),
            }],
            findings: vec![crate::core::review_store::ReviewFinding {
                claim: "发现".into(),
                evidence_ids: vec!["E1".into()],
            }],
            source_type: "fulltext".into(),
            ..Default::default()
        };
        let card = normalize_card(raw, &stored, &all_candidates(&stored), false).unwrap();
        assert_eq!(card.paper_id, "p1");
        assert_eq!(card.source_type, "abstract");
        assert_eq!(card.evidence[0].chunk_id, None);
        assert_eq!(card.evidence[0].page, None);
        assert_eq!(card.evidence[0].content, stored.abstract_text);
    }

    #[test]
    fn unknown_or_mismatched_evidence_cannot_enter_a_card() {
        let stored = source("fulltext");
        let raw = ReviewPaperCard {
            evidence: vec![ReviewEvidence {
                id: "E1".into(),
                chunk_id: Some("other-chunk".into()),
                page: Some(99),
                quote: "真实原文".into(),
                content: "伪造内容".into(),
            }],
            ..Default::default()
        };
        assert!(normalize_card(raw, &stored, &all_candidates(&stored), false).is_err());
    }

    #[test]
    fn global_evidence_numbers_are_run_wide_and_stable_on_edit() {
        let stored = source("fulltext");
        let raw = ReviewPaperCard {
            evidence: vec![ReviewEvidence {
                id: "E1".into(),
                chunk_id: Some("c1".into()),
                page: Some(3),
                quote: "真实原文".into(),
                content: String::new(),
            }],
            findings: vec![crate::core::review_store::ReviewFinding {
                claim: "发现".into(),
                evidence_ids: vec!["E1".into()],
            }],
            ..Default::default()
        };
        let card = normalize_card(raw, &stored, &all_candidates(&stored), false).unwrap();
        let (card, registry) = assign_global_evidence(card, &stored, &[], false);
        assert_eq!(card.evidence[0].id, "E1");
        assert_eq!(card.findings[0].evidence_ids, vec!["E1"]);
        let (edited, registry2) = assign_global_evidence(card.clone(), &stored, &registry, true);
        assert_eq!(edited.evidence[0].id, "E1");
        assert_eq!(registry2.len(), 1);

        let (other, registry3) = assign_global_evidence(card, &stored, &registry, false);
        assert_eq!(other.evidence[0].id, "E2");
        assert_eq!(registry3.len(), 2);
    }

    #[test]
    fn cache_key_changes_with_question_scope_index_and_prompt() {
        let key = card_cache_key("q", "s", "f", "p");
        assert_ne!(key, card_cache_key("q2", "s", "f", "p"));
        assert_ne!(key, card_cache_key("q", "s2", "f", "p"));
        assert_ne!(key, card_cache_key("q", "s", "f2", "p"));
        assert_ne!(key, card_cache_key("q", "s", "f", "p2"));
        assert_eq!(key, card_cache_key("q", "s", "f", "p"));
    }

    #[test]
    fn synthesis_rejects_unknown_paper_ids() {
        let mut synthesis = parse_synthesis(
            "{\"themes\":[{\"title\":\"主题\",\"detail\":\"细节\",\"paperIds\":[\"p1\",\"hallucinated\"]}]}",
        )
        .unwrap();
        assert!(validate_synthesis(&mut synthesis, &HashSet::from(["p1".to_string()])).is_err());
        let mut valid = parse_synthesis(
            "{\"gaps\":[{\"title\":\"缺口\",\"detail\":\"细节\",\"paperIds\":[\"p1\",\"p1\"]}]}",
        )
        .unwrap();
        validate_synthesis(&mut valid, &HashSet::from(["p1".to_string()])).unwrap();
        assert_eq!(valid.gaps[0].paper_ids, vec!["p1"]);
    }

    #[tokio::test]
    async fn database_precheck_freezes_real_sources_and_metadata() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(crate::core::db::SCHEMA_INIT_SQL).execute(&db).await.unwrap();
        for (id, title, abstract_text) in [
            ("p-full", "全文文献", None),
            ("p-abstract", "摘要文献", Some("只有摘要可用于阅读。")),
            ("p-empty", "不可用文献", None),
        ] {
            sqlx::query("INSERT INTO papers (id, title, abstract, created_at, updated_at) VALUES (?, ?, ?, 't', 't')")
                .bind(id)
                .bind(title)
                .bind(abstract_text)
                .execute(&db)
                .await
                .unwrap();
        }
        for (id, paper_id, content) in [
            ("c-full", "p-full", "真实正文。".repeat(80)),
            ("c-tiny", "p-abstract", "太短".to_string()),
        ] {
            sqlx::query(
                "INSERT INTO chunks (id, paper_id, content, chunk_index, page_start, page_end, created_at)
                 VALUES (?, ?, ?, 0, 2, 2, 't')",
            )
            .bind(id)
            .bind(paper_id)
            .bind(content)
            .execute(&db)
            .await
            .unwrap();
        }
        let ids = vec!["p-full".to_string(), "p-abstract".to_string(), "p-empty".to_string()];
        let sources = freeze_sources(&db, &ids).await.unwrap();
        assert_eq!(sources[0].source.source_kind, "fulltext");
        assert_eq!(sources[1].source.source_kind, "abstract");
        assert_eq!(sources[1].source.chunk_count, 1);
        assert_eq!(sources[2].source.source_kind, "unusable");
        assert_eq!(sources[2].source.status, "done");

        sqlx::query("UPDATE papers SET title = '后来修改的标题' WHERE id = 'p-full'").execute(&db).await.unwrap();
        sqlx::query("UPDATE chunks SET content = '后来修改的正文' WHERE id = 'c-full'").execute(&db).await.unwrap();
        assert_eq!(sources[0].source.title, "全文文献");
        assert!(sources[0].chunks[0].content.contains("真实正文"));
        assert_ne!(sources[0].chunks[0].content, "后来修改的正文");

        sqlx::query("UPDATE papers SET abstract = '更新后的摘要' WHERE id = 'p-abstract'").execute(&db).await.unwrap();
        let refreshed = freeze_sources(&db, &ids).await.unwrap();
        assert_ne!(refreshed[1].index_fingerprint, sources[1].index_fingerprint);
    }
}
