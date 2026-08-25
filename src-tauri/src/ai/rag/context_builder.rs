use crate::ai::retriever::SearchResult;
use crate::core::settings_service::cached_settings;

/// Build a context string from search results for the LLM prompt
pub fn build_context(results: &[SearchResult], query: &str) -> String {
    if results.is_empty() {
        return format!(
            "No relevant documents found in the library for the query: \"{}\"\n\nPlease answer based on your general knowledge, and let the user know that no specific documents were found.",
            query
        );
    }

    let mut context = String::from("以下是从文献库中检索到的相关内容：\n\n");
    let mut token_used = 0;

    let chunk_limit = cached_settings().rag_chunk_max_chars.max(1) as usize;
    for (i, result) in results.iter().enumerate() {
        let citation = format!(
            "[{}] 来源: {} (页码: {}-{})\n{}\n\n",
            i + 1,
            result.paper_title,
            result.page_start.unwrap_or(0),
            result.page_end.unwrap_or(0),
            result.content.chars().take(chunk_limit).collect::<String>(),
        );

        let max_context_tokens = cached_settings().rag_max_context_tokens.max(500) as usize;
        let est_tokens = citation.chars().count() / 4;
        if token_used + est_tokens > max_context_tokens {
            context.push_str(&format!(
                "... 共检索到 {} 个结果，已截断以适配上下文窗口。\n",
                results.len()
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
