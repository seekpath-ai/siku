use crate::ai::retriever::SearchResult;
use crate::core::settings_service::cached_settings;

/// Estimate tokens the way the *model* will: CJK is roughly one token per
/// character, Latin text about four characters per token.
///
/// The previous `chars / 4` undercounted Chinese by 3–4×, so a context budgeted
/// at 4000 "tokens" could really be 12k+ and quietly overflow the window.
pub fn estimate_tokens(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut other = 0usize;
    for c in text.chars() {
        if crate::ai::query::is_cjk(c) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + other.div_ceil(4)
}

/// Drop redundant hits and order the rest so each chunk is followed by its
/// neighbours.
///
/// Two kinds of redundancy occur in practice: the same passage retrieved by
/// several legs (identical ids) and adjacent chunks of one paper whose contents
/// overlap because of the chunker's overlap window — the shorter one then adds
/// nothing.
pub fn dedup_and_order(results: &[SearchResult]) -> Vec<SearchResult> {
    let mut kept: Vec<SearchResult> = Vec::new();
    for r in results {
        let redundant = kept.iter().any(|k| {
            if k.chunk_id == r.chunk_id {
                return true;
            }
            if k.paper_id != r.paper_id || (k.chunk_index - r.chunk_index).abs() > 1 {
                return false;
            }
            let (short, long) = if r.content.len() <= k.content.len() { (r, k) } else { (k, r) };
            long.content.contains(short.content.trim())
        });
        if !redundant {
            kept.push(r.clone());
        }
    }
    kept.sort_by(|a, b| {
        a.paper_id
            .cmp(&b.paper_id)
            .then(a.chunk_index.cmp(&b.chunk_index))
            .then(b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
    });
    kept
}

/// Build a context string from search results for the LLM prompt
pub fn build_context(results: &[SearchResult], query: &str) -> String {
    if results.is_empty() {
        return format!(
            "No relevant documents found in the library for the query: \"{}\"\n\nPlease answer based on your general knowledge, and let the user know that no specific documents were found.",
            query
        );
    }

    let mut context = String::from("以下是从文献库中检索到的相关内容：\n\n");
    let mut token_used = 0usize;

    let max_context_tokens = cached_settings().rag_max_context_tokens.max(500) as usize;
    let chunk_limit = cached_settings().rag_chunk_max_chars.max(1) as usize;
    let kept = dedup_and_order(results);

    for (i, result) in kept.iter().enumerate() {
        let section = result
            .section_path
            .clone()
            .or_else(|| result.section.clone())
            .map(|s| format!(" · {s}"))
            .unwrap_or_default();
        let kind = if result.is_neighbor { " · 相邻块" } else { "" };
        let citation = format!(
            "[{}] 来源: {} (页码: {}-{}{section}{kind})\n{}\n\n",
            i + 1,
            result.paper_title,
            result.page_start.unwrap_or(0),
            result.page_end.unwrap_or(0),
            result.content.chars().take(chunk_limit).collect::<String>(),
        );

        let est_tokens = estimate_tokens(&citation);
        if token_used + est_tokens > max_context_tokens {
            context.push_str(&format!(
                "... 共检索到 {} 个结果，已截断以适配上下文窗口。\n",
                kept.len()
            ));
            break;
        }

        token_used += est_tokens;
        context.push_str(&citation);
    }

    context.push_str(&format!(
        "用户问题: {}\n\n请基于以上文献内容回答问题，使用 [n] 标注引用来源。",
        query
    ));

    context
}

/// Build a RAG system prompt with retrieved context
pub fn build_rag_messages(
    results: &[SearchResult],
    query: &str,
    system_prompt: Option<&str>,
) -> Vec<crate::ai::llm::ChatMessage> {
    let context = build_context(results, query);

    vec![
        crate::ai::llm::ChatMessage {
            role: "system".to_string(),
            content: system_prompt.unwrap_or(
                "You are a research assistant. Answer questions based on the provided literature excerpts. Always cite sources using [n] notation."
            ).to_string(),
            attachments: None, tool_calls: None, tool_call_id: None, name: None,
        },
        crate::ai::llm::ChatMessage {
            role: "user".to_string(),
            content: context,
            attachments: None, tool_calls: None, tool_call_id: None, name: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: &str, paper: &str, index: i32, content: &str, neighbor: bool) -> SearchResult {
        SearchResult {
            chunk_id: id.into(),
            paper_id: paper.into(),
            chunk_index: index,
            content: content.into(),
            page_start: Some(1),
            page_end: Some(1),
            section: None,
            section_path: None,
            block_type: "prose".into(),
            is_tail: false,
            is_neighbor: neighbor,
            paper_title: "T".into(),
            score: 1.0,
            source: "fts5".into(),
        }
    }

    #[test]
    fn token_estimate_counts_cjk_per_character() {
        assert_eq!(estimate_tokens("这是一段中文测试文本内容"), 12);
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        assert_eq!(estimate_tokens("中文abcd"), 3);
    }

    #[test]
    fn drops_overlapping_neighbour_chunks() {
        let anchor = hit("c1", "p1", 3, "A full paragraph about prehospital stroke assessment and its stages.", false);
        let overlapping = hit("c2", "p1", 4, "prehospital stroke assessment and its stages.", true);
        let distinct = hit("c3", "p1", 4, "A different paragraph entirely about the scoring scale.", true);
        let kept = dedup_and_order(&[anchor, overlapping, distinct]);
        let ids: Vec<&str> = kept.iter().map(|r| r.chunk_id.as_str()).collect();
        assert!(ids.contains(&"c1"));
        assert!(!ids.contains(&"c2"), "the shorter overlap must be dropped: {ids:?}");
        assert!(ids.contains(&"c3"));
    }

    #[test]
    fn keeps_identical_ids_once() {
        let a = hit("c1", "p1", 1, "text", false);
        let b = hit("c1", "p1", 1, "text", false);
        assert_eq!(dedup_and_order(&[a, b]).len(), 1);
    }

    #[test]
    fn orders_by_paper_and_chunk() {
        let a = hit("c3", "p1", 3, "third", false);
        let b = hit("c1", "p1", 1, "first", false);
        let c = hit("c2", "p1", 2, "second", false);
        let kept = dedup_and_order(&[a, b, c]);
        let ids: Vec<&str> = kept.iter().map(|r| r.chunk_id.as_str()).collect();
        assert_eq!(ids, vec!["c1", "c2", "c3"]);
    }
}
