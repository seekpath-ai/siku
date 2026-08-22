use sqlx::SqlitePool;
use tracing::{info, instrument, warn};

/// Default embedding model (stored in the embeddings table as metadata).
pub const DEFAULT_MODEL: &str = "BAAI/bge-small-zh-v1.5";
pub const DEFAULT_DIMENSIONS: usize = 512;

/// Generate embeddings for text chunks and store them in the database.
/// The active backend comes from app settings: "api" uses an OpenAI-compatible
/// embeddings endpoint (e.g. OpenAI / Ollama / local server), "hash" (default)
/// uses the built-in lexical fallback.
#[instrument(skip(db))]
pub async fn generate_embeddings_for_paper(
    db: &SqlitePool,
    paper_id: &str,
) -> Result<usize, String> {
    // Get chunks without embeddings
    let chunks: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.id, c.content FROM chunks c LEFT JOIN embeddings e ON c.id = e.chunk_id WHERE c.paper_id = ? AND e.chunk_id IS NULL"
    )
    .bind(paper_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    if chunks.is_empty() {
        return Ok(0);
    }

    let count = chunks.len();
    info!(paper_id, count, "generating embeddings");

    let vectors = embed_texts(db, &chunks.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>()).await?;
    let model = embedding_model_label();

    for ((chunk_id, _), vector) in chunks.iter().zip(vectors.iter()) {
        let vector_blob = vector_to_blob(vector);
        let now = crate::core::time::now_iso();

        // A chunk may have been deleted by a concurrent index rebuild after
        // we selected it; skip it instead of aborting the whole batch.
        if let Err(e) = sqlx::query(
            "INSERT OR REPLACE INTO embeddings (chunk_id, model, dimensions, vector, created_at) VALUES (?, ?, ?, ?, ?)"
        )
        .bind(chunk_id)
        .bind(&model)
        .bind(vector.len() as i32)
        .bind(&vector_blob)
        .bind(&now)
        .execute(db)
        .await
        {
            warn!(chunk_id, error = %e, "skipping embedding for vanished chunk");
            continue;
        }
    }

    info!(paper_id, count, "embeddings generated");
    Ok(count)
}

/// Embed a batch of texts with the configured backend. Falls back to the
/// hash embedding when the API backend is unset or fails.
pub async fn embed_texts(db: &SqlitePool, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    let settings = crate::core::settings_service::cached_settings();
    let device_settings = crate::core::settings_service::cached_device_settings();
    if settings.embedding_backend == "api"
        && !settings.embedding_base_url.trim().is_empty()
    {
        match api_embed_texts(&settings.embedding_base_url, &device_settings.embedding_api_key, &settings.embedding_model, texts).await {
            Ok(vectors) if vectors.len() == texts.len() => return Ok(vectors),
            Ok(_) => warn!("embedding API returned mismatched count, falling back to hash"),
            Err(e) => warn!(error = %e, "embedding API failed, falling back to hash"),
        }
    }
    Ok(texts.iter().map(|t| generate_fallback_embedding(t)).collect())
}

/// Model label recorded in the embeddings table for the active backend.
fn embedding_model_label() -> String {
    let settings = crate::core::settings_service::cached_settings();
    if settings.embedding_backend == "api" {
        settings.embedding_model.clone()
    } else {
        DEFAULT_MODEL.to_string()
    }
}

/// Call an OpenAI-compatible `/embeddings` endpoint.
async fn api_embed_texts(
    base_url: &str,
    api_key: &str,
    model: &str,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    let url = format!("{}/embeddings", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut builder = client.post(&url).json(&serde_json::json!({
        "model": model,
        "input": texts,
    }));
    if !api_key.trim().is_empty() {
        builder = builder.header("Authorization", format!("Bearer {}", api_key.trim()));
    }
    let resp = builder
        .send()
        .await
        .map_err(|e| format!("embedding request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("embedding API status {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| format!("embedding json: {e}"))?;
    let data = body["data"]
        .as_array()
        .ok_or_else(|| "embedding response missing data".to_string())?;
    let mut out = Vec::with_capacity(data.len());
    for item in data {
        let emb = item["embedding"]
            .as_array()
            .ok_or_else(|| "embedding item missing vector".to_string())?
            .iter()
            .filter_map(|v| v.as_f64())
            .map(|v| v as f32)
            .collect::<Vec<f32>>();
        out.push(emb);
    }
    Ok(out)
}

/// Generate a fallback embedding vector from text.
/// This is a simple TF-IDF-like hash-based embedding.
/// Replace with fastembed-rs ONNX inference in production.
pub fn generate_fallback_embedding(text: &str) -> Vec<f32> {
    let mut vec = vec![0.0f32; DEFAULT_DIMENSIONS];

    // Simple character n-gram hashing to produce a pseudo-embedding
    // This is NOT for production use — it's a placeholder until fastembed is wired up
    let text_lower = text.to_lowercase();
    let chars: Vec<char> = text_lower.chars().collect();

    // Unigram features
    for (i, ch) in chars.iter().enumerate() {
        let idx = (*ch as usize) % DEFAULT_DIMENSIONS;
        vec[idx] += 1.0 / (i as f32 + 1.0).sqrt();
    }

    // Bigram features
    for window in chars.windows(2) {
        let hash = (window[0] as usize * 31 + window[1] as usize) % DEFAULT_DIMENSIONS;
        vec[hash] += 0.5;
    }

    // Normalize
    let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut vec {
            *v /= norm;
        }
    }

    vec
}

/// Convert float vector to blob for SQLite storage
fn vector_to_blob(vec: &[f32]) -> Vec<u8> {
    vec.iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

/// Convert blob back to float vector
pub fn blob_to_vector(blob: &[u8]) -> Vec<f32> {
    blob.chunks(4)
        .filter_map(|chunk| {
            if chunk.len() == 4 {
                Some(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            } else {
                None
            }
        })
        .collect()
}

/// Generate an embedding for a single query text using the active backend.
pub async fn embed_query(db: &SqlitePool, text: &str) -> Vec<f32> {
    match embed_texts(db, &[text.to_string()]).await {
        Ok(mut v) if !v.is_empty() => v.remove(0),
        _ => generate_fallback_embedding(text),
    }
}

/// Cosine similarity between two vectors
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;

    for i in 0..n {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a.sqrt() * norm_b.sqrt())
}
