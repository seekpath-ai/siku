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
    // 1. Retrieve
    let results = retriever::hybrid_search(db, query, top_k).await?;

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

    Ok(resp.content)
}
