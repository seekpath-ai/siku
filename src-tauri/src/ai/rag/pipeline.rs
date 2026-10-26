use sqlx::SqlitePool;
use tracing::instrument;

use crate::ai::llm;
use crate::ai::rag::context_builder;
use crate::ai::retriever;

/// Run the full RAG pipeline: retrieve → build context → generate
#[instrument(skip(db))]
pub async fn rag_query(
    db: &SqlitePool,
    query: &str,
    top_k: usize,
) -> Result<String, String> {
    // 1. Retrieve, then pull in the neighbouring chunks of every hit: a
    //    ~512-token boundary regularly cuts a sentence in half.
    let mut results = retriever::hybrid_search(db, query, top_k).await?;
    retriever::expand_with_neighbors(db, &mut results, false).await?;

    // 2. Build context messages
    let messages = context_builder::build_rag_messages(&results, query, None);

    // 3. Load LLM config
    let llm_config = crate::core::settings_service::load_llm_config(db).await?;

    if llm_config.api_key.is_empty() && llm_config.provider != llm::LlmProvider::Ollama {
        return Err("API key not configured.".to_string());
    }

    // 4. Generate response
    let client = llm::client::create_llm_client(&llm_config)
        .map_err(|e| format!("LLM client: {e}"))?;

    let resp = client.chat_completion(&messages, &[]).await
        .map_err(|e| format!("generation: {e}"))?;

    // 空回答直接报错：思考模型可能把 max_tokens 全部用于推理（finish_reason="length"），
    // 此时 content 为空，不能把空串当答案交给上层。
    if resp.content.trim().is_empty() {
        return Err(format!(
            "模型未返回答案（finish_reason={}）：可能是思考模型的推理耗尽了输出额度，请调高该模型的 max_tokens 后重试",
            resp.finish_reason.as_deref().unwrap_or("未知"),
        ));
    }

    Ok(resp.content)
}
