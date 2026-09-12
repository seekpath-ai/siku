use tauri::State;
use tracing::instrument;
use crate::AppState;

#[tauri::command]
#[instrument(skip(state))]
pub async fn search_hybrid(
    state: State<'_, AppState>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<crate::core::search_service::SearchResult>, String> {
    crate::core::search_service::search(&state.db, &query, limit.unwrap_or(10)).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn search_generate_embeddings(
    state: State<'_, AppState>,
    paper_id: String,
) -> Result<usize, String> {
    crate::ai::embedder::generate_embeddings_for_paper(&state.db, &paper_id).await
}

/// What the vector leg is doing: backend, active model, coverage, leftovers.
#[tauri::command]
#[instrument(skip(state))]
pub async fn search_embedding_status(
    state: State<'_, AppState>,
) -> Result<crate::ai::embedder::EmbeddingStatus, String> {
    crate::ai::embedder::embedding_status(&state.db).await
}

/// Probe the configured embeddings endpoint once. Failures come back as a
/// report, not as a rejected command.
#[tauri::command]
#[instrument]
pub async fn search_test_embedding_endpoint() -> crate::ai::embedder::EmbeddingProbe {
    crate::ai::embedder::test_embedding_endpoint().await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn search_rag_query(
    state: State<'_, AppState>,
    query: String,
    top_k: Option<usize>,
) -> Result<String, String> {
    crate::ai::rag::pipeline::rag_query(&state.db, &query, top_k.unwrap_or(5)).await
}
