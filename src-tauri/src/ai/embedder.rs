use sqlx::SqlitePool;
use tracing::{info, instrument, warn};

/// Model name this project recommends for a local OpenAI-compatible endpoint.
pub const RECOMMENDED_LOCAL_MODEL: &str = "BAAI/bge-small-zh-v1.5";

/// Label stored for the built-in character-histogram placeholder vectors.
///
/// Deliberately NOT a real model name. It used to be `DEFAULT_MODEL`
/// ("BAAI/bge-small-zh-v1.5"), which is exactly the model a local endpoint
/// serves — so the moment someone configured such an endpoint, the stale
/// placeholder rows carried the same label as the new vectors: the re-embed
/// query (`e.model <> ?`) skipped them as "already done", and the vector leg
/// scored real query vectors against character histograms. The labels are now
/// disjoint, and `relabel_placeholder_embeddings` fixes existing databases.
pub const PLACEHOLDER_MODEL: &str = "placeholder-hash";
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
    // The built-in "hash" backend is a character-histogram placeholder, not a
    // semantic model: retrieval refuses those vectors (`retriever::
    // vector_leg_enabled`), so generating them would only burn CPU. Configure an
    // OpenAI-compatible embeddings endpoint (or wire up `fastembed` behind the
    // `onnx` feature) to enable the vector leg.
    if !crate::ai::retriever::vector_leg_enabled() {
        tracing::debug!(
            paper_id,
            "embedding backend is the built-in placeholder — skipping embedding generation"
        );
        return Ok(0);
    }

    // Chunks that need a vector: never embedded, or embedded by a DIFFERENT
    // model. The old query only looked for missing rows, so switching the
    // embedding backend left every existing chunk in the previous vector space
    // for ever (retrieval then matched almost nothing, silently).
    let model = embedding_model_label();
    let chunks: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.id, c.content FROM chunks c \
         LEFT JOIN embeddings e ON c.id = e.chunk_id \
         WHERE c.paper_id = ? AND (e.chunk_id IS NULL OR e.model <> ?)",
    )
    .bind(paper_id)
    .bind(&model)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    if chunks.is_empty() {
        return Ok(0);
    }

    let count = chunks.len();
    info!(paper_id, count, "generating embeddings");

    let vectors = embed_texts(db, &chunks.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>()).await?;

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

/// The embedding backend as configured right now.
///
/// Passed explicitly to the `_with` helpers so tests can exercise a backend
/// without touching the process-wide settings cache.
#[derive(Debug, Clone, Default)]
pub struct EmbeddingConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl EmbeddingConfig {
    /// Read the active configuration from app + device settings.
    ///
    /// The service address IS the switch: empty means "no semantic search", and
    /// there is nothing else to turn on. There used to be a separate backend
    /// enum (`hash` / `api`) whose only job was to disable something that a
    /// missing address already disables.
    pub fn active() -> Self {
        let settings = crate::core::settings_service::cached_settings();
        let device = crate::core::settings_service::cached_device_settings();
        Self {
            base_url: settings.embedding_base_url,
            api_key: device.embedding_api_key,
            model: settings.embedding_model,
        }
    }

    /// An endpoint is configured, so vectors may take part in retrieval.
    pub fn enabled(&self) -> bool {
        !self.base_url.trim().is_empty()
    }
}

/// Whether the vector leg may contribute results for the active configuration.
pub fn vector_leg_enabled() -> bool {
    EmbeddingConfig::active().enabled()
}

/// Embed a batch of texts with the configured backend.
///
/// With an API backend configured, a failure is returned as an error — it is
/// NOT silently replaced by the hash placeholder. Falling back used to store
/// character-histogram vectors under the API model's label, so the vector leg
/// kept "working" while comparing meaningless numbers.
pub async fn embed_texts(db: &SqlitePool, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    embed_texts_with(&EmbeddingConfig::active(), db, texts).await
}

/// `embed_texts` against an explicit configuration.
pub async fn embed_texts_with(
    cfg: &EmbeddingConfig,
    db: &SqlitePool,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    if cfg.enabled() {
        let vectors = api_embed_texts(&cfg.base_url, &cfg.api_key, &cfg.model, texts).await?;
        if vectors.len() != texts.len() {
            return Err(format!(
                "embedding API returned {} vectors for {} inputs",
                vectors.len(),
                texts.len()
            ));
        }
        return Ok(vectors);
    }

    // No endpoint configured. The old code answered with the hash placeholder
    // here, which made an unconfigured install look like it had vectors.
    let _ = db;
    Err("no embeddings endpoint configured".to_string())
}

/// Model label recorded in the embeddings table for the active configuration.
///
/// Retrieval compares vectors only within the same label: rows from another
/// model live in a different vector space (and often a different dimension).
pub fn embedding_model_label() -> String {
    let cfg = EmbeddingConfig::active();
    model_label(cfg.enabled(), &cfg.model)
}

/// Label for a configuration. Split out so the "a stored label must never
/// accidentally match a real model name" rule is testable without the global
/// settings cache.
///
/// With no endpoint configured the label is `PLACEHOLDER_MODEL`: rows left over
/// from the hash-placeholder era must never look like they were produced by the
/// model the user later configures.
pub fn model_label(enabled: bool, configured_model: &str) -> String {
    if enabled {
        configured_model.to_string()
    } else {
        PLACEHOLDER_MODEL.to_string()
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
        // The most common misconfiguration by far: the base URL has no `/v1`,
        // so the request lands one path segment too high.
        let hint = if resp.status() == reqwest::StatusCode::NOT_FOUND {
            "（检查 Base URL 是否包含 /v1，例如 http://127.0.0.1:11434/v1）"
        } else {
            ""
        };
        return Err(format!("embedding API status {}{hint}", resp.status()));
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
///
/// `None` means "no semantic query vector is available": either the vector leg
/// is not configured, or the endpoint did not answer. It never falls back to
/// the hash placeholder — those vectors live in another space, and when the
/// dimensions happen to coincide (both 512 for `bge-small-zh-v1.5`) the retriever
/// would score real vectors against a character histogram and mix the garbage
/// into the fused ranking.
pub async fn embed_query(db: &SqlitePool, text: &str) -> Option<Vec<f32>> {
    embed_query_with(&EmbeddingConfig::active(), db, text).await
}

/// `embed_query` against an explicit configuration.
pub async fn embed_query_with(
    cfg: &EmbeddingConfig,
    db: &SqlitePool,
    text: &str,
) -> Option<Vec<f32>> {
    if !cfg.enabled() {
        return None;
    }
    match embed_texts_with(cfg, db, &[text.to_string()]).await {
        Ok(mut v) if v.first().is_some_and(|vec| !vec.is_empty()) => Some(v.remove(0)),
        Ok(_) => {
            warn!("embedding endpoint returned no vector; vector leg skipped");
            None
        }
        Err(e) => {
            warn!(error = %e, "query embedding failed; vector leg skipped");
            None
        }
    }
}

/// One-shot probe of the configured endpoint, for the settings UI.
///
/// Always returns a report: a configuration or transport failure is the
/// payload, not a rejected promise, so the panel can show the raw reason.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EmbeddingProbe {
    pub ok: bool,
    pub model: String,
    pub base_url: String,
    /// Dimensions returned by the endpoint, when it answered.
    pub dimensions: Option<usize>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

const PROBE_TEXT: &str = "siku embedding probe 向量探针";

/// Probe the active endpoint once.
pub async fn test_embedding_endpoint() -> EmbeddingProbe {
    probe_endpoint(&EmbeddingConfig::active()).await
}

/// Probe an endpoint described by the values the settings form currently holds,
/// so a configuration can be tried before it is saved.
pub async fn test_embedding_endpoint_with(
    base_url: &str,
    model: &str,
    api_key: &str,
) -> EmbeddingProbe {
    probe_endpoint(&EmbeddingConfig {
        base_url: base_url.to_string(),
        api_key: api_key.to_string(),
        model: model.to_string(),
    })
    .await
}

/// Probe an explicit configuration once.
pub async fn probe_endpoint(cfg: &EmbeddingConfig) -> EmbeddingProbe {
    let mut probe = EmbeddingProbe {
        ok: false,
        model: cfg.model.clone(),
        base_url: cfg.base_url.clone(),
        dimensions: None,
        latency_ms: 0,
        error: None,
    };

    if !cfg.enabled() {
        probe.error = Some(
            "未填写服务地址（需包含 /v1，例如 http://127.0.0.1:8899/v1）".to_string(),
        );
        return probe;
    }

    let started = std::time::Instant::now();
    let result =
        api_embed_texts(&cfg.base_url, &cfg.api_key, &cfg.model, &[PROBE_TEXT.to_string()]).await;
    probe.latency_ms = started.elapsed().as_millis() as u64;

    match result {
        Ok(vectors) => match vectors.first() {
            Some(v) if !v.is_empty() => {
                probe.ok = true;
                probe.dimensions = Some(v.len());
            }
            _ => probe.error = Some("端点返回了空向量".to_string()),
        },
        Err(e) => probe.error = Some(e),
    }
    probe
}

/// How much of the library actually has vectors for the active model.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EmbeddingModelCount {
    pub model: String,
    pub chunks: i64,
    pub dimensions: i64,
}

/// What the vector leg is doing right now, and why it might be doing nothing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EmbeddingStatus {
    /// Semantic search is on: a service address is configured.
    pub leg_enabled: bool,
    /// Model the vectors are (or will be) labelled with.
    pub model: String,
    pub base_url: String,
    pub total_chunks: i64,
    /// Chunks holding a vector under the active model label.
    pub embedded_chunks: i64,
    /// Dimensions of those vectors, when there is at least one row.
    pub dimensions: Option<i64>,
    /// Leftovers from the hash-placeholder era. They never take part in
    /// retrieval; the UI says so explicitly instead of letting them look like
    /// usable vectors.
    pub placeholder_chunks: i64,
    /// Vectors under any other real model. Retrieval ignores them, so they are
    /// the visible reason a paper "has no vectors" after switching models.
    pub other_models: Vec<EmbeddingModelCount>,
}

/// Report the state of the vector leg for the active configuration.
pub async fn embedding_status(db: &SqlitePool) -> Result<EmbeddingStatus, String> {
    let cfg = EmbeddingConfig::active();
    let model = embedding_model_label();
    embedding_status_with(&cfg, db, &model).await
}

/// `embedding_status` against an explicit configuration and model label.
pub async fn embedding_status_with(
    cfg: &EmbeddingConfig,
    db: &SqlitePool,
    model: &str,
) -> Result<EmbeddingStatus, String> {
    let total_chunks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chunks")
        .fetch_one(db)
        .await
        .map_err(|e| format!("db: {e}"))?;

    let (embedded_chunks, dimensions): (i64, Option<i64>) =
        sqlx::query_as("SELECT COUNT(*), MAX(dimensions) FROM embeddings WHERE model = ?")
            .bind(model)
            .fetch_one(db)
            .await
            .map_err(|e| format!("db: {e}"))?;

    let other_models: Vec<EmbeddingModelCount> =
        sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT model, COUNT(*), COALESCE(MAX(dimensions), 0) FROM embeddings \
             WHERE model <> ? AND model <> ? GROUP BY model ORDER BY COUNT(*) DESC",
        )
        .bind(model)
        .bind(PLACEHOLDER_MODEL)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))?
        .into_iter()
        .map(|(model, chunks, dimensions)| EmbeddingModelCount { model, chunks, dimensions })
        .collect();

    let placeholder_chunks: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM embeddings WHERE model = ?")
            .bind(PLACEHOLDER_MODEL)
            .fetch_one(db)
            .await
            .map_err(|e| format!("db: {e}"))?;

    Ok(EmbeddingStatus {
        leg_enabled: cfg.enabled(),
        model: model.to_string(),
        base_url: cfg.base_url.clone(),
        total_chunks,
        embedded_chunks,
        dimensions,
        placeholder_chunks,
        other_models,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    /// Only the address matters: there is no backend switch any more.
    fn cfg(base_url: &str) -> EmbeddingConfig {
        EmbeddingConfig {
            base_url: base_url.to_string(),
            api_key: String::new(),
            model: "bge-m3".to_string(),
        }
    }

    async fn test_db() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        sqlx::raw_sql(include_str!("../../schema_init.sql"))
            .execute(&pool)
            .await
            .expect("schema");
        pool
    }

    /// A stub endpoint that answers `body` only for `POST path`, and 404 for
    /// anything else — so a wrong URL shape fails loudly instead of silently
    /// passing.
    async fn stub_endpoint(path: &'static str, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub");
        let addr = listener.local_addr().expect("stub addr");
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 8192];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let (status, payload) = if head.starts_with(&format!("POST {path} ")) {
                        ("200 OK", body)
                    } else {
                        ("404 Not Found", "{\"error\":\"no such route\"}")
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        format!("http://{addr}/v1")
    }

    /// A URL that is guaranteed not to be listening.
    async fn dead_endpoint() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        format!("http://127.0.0.1:{port}/v1")
    }

    #[tokio::test]
    async fn probe_reports_dimensions_for_a_working_endpoint() {
        let base = stub_endpoint(
            "/v1/embeddings",
            "{\"data\":[{\"embedding\":[0.1,0.2,0.3]}]}",
        )
        .await;
        let probe = probe_endpoint(&cfg(&base)).await;
        assert!(probe.ok, "探针应成功，实际错误 {:?}", probe.error);
        assert_eq!(probe.dimensions, Some(3));
        assert!(probe.error.is_none());
        assert_eq!(probe.model, "bge-m3");
    }

    #[tokio::test]
    async fn probe_explains_a_missing_v1_segment() {
        let base = stub_endpoint("/v1/embeddings", "{}").await;
        let without_v1 = base.trim_end_matches("/v1").to_string();
        let probe = probe_endpoint(&cfg(&without_v1)).await;
        assert!(!probe.ok);
        let err = probe.error.expect("错误信息");
        assert!(err.contains("404"), "应报告 404，实际 {err}");
        assert!(err.contains("/v1"), "应提示补 /v1，实际 {err}");
    }

    #[tokio::test]
    async fn an_empty_address_means_semantic_search_is_off() {
        let db = test_db().await;
        assert!(!cfg("").enabled(), "地址为空即关闭语义搜索");
        assert!(embed_query_with(&cfg(""), &db, "任意查询").await.is_none());
        assert!(
            embed_texts_with(&cfg(""), &db, &["任意".to_string()])
                .await
                .is_err(),
            "没有端点时必须报错，不能悄悄回退成占位向量"
        );
    }

    #[tokio::test]
    async fn probe_requires_a_service_address() {
        let probe = probe_endpoint(&cfg("   ")).await;
        assert!(!probe.ok);
        assert_eq!(probe.latency_ms, 0, "没有地址时不应发起请求");
        let err = probe.error.expect("错误信息");
        assert!(err.contains("服务地址"), "应提示填服务地址，实际 {err}");
    }

    #[tokio::test]
    async fn query_embedding_uses_the_endpoint_vector() {
        let base = stub_endpoint(
            "/v1/embeddings",
            "{\"data\":[{\"embedding\":[1.0,0.0,0.0,0.0]}]}",
        )
        .await;
        let db = test_db().await;
        let vector = embed_query_with(&cfg(&base), &db, "这篇论文用了什么方法")
            .await
            .expect("应拿到查询向量");
        assert_eq!(vector.len(), 4);
        assert_eq!(vector[0], 1.0);
    }

    #[tokio::test]
    async fn query_embedding_is_none_when_the_endpoint_is_down() {
        let db = test_db().await;
        let dead = cfg(&dead_endpoint().await);
        assert!(
            embed_query_with(&dead, &db, "query").await.is_none(),
            "端点不可用时必须返回 None，而不是退回哈希占位向量"
        );
    }



    #[tokio::test]
    async fn status_reports_coverage_and_rows_left_under_other_models() {
        let db = test_db().await;
        sqlx::query(
            "INSERT INTO papers (id, title, created_at, updated_at) VALUES ('p1', '测试', 't', 't')",
        )
        .execute(&db)
        .await
        .unwrap();
        for i in 0..3 {
            sqlx::query(
                "INSERT INTO chunks (id, paper_id, content, search_text, block_type, is_tail, chunk_index, created_at)
                 VALUES (?, 'p1', '正文', '正文', 'prose', 0, ?, 't')",
            )
            .bind(format!("c{i}"))
            .bind(i)
            .execute(&db)
            .await
            .unwrap();
        }
        let rows: [(&str, &str, i64); 3] = [
            ("c0", "bge-m3", 1024),
            ("c1", "bge-m3", 1024),
            // Left behind by a previous backend: retrieval ignores these.
            ("c2", "old-model", 512),
        ];
        for (chunk_id, model, dimensions) in rows {
            sqlx::query(
                "INSERT INTO embeddings (chunk_id, model, dimensions, vector, created_at)
                 VALUES (?, ?, ?, ?, 't')",
            )
            .bind(chunk_id)
            .bind(model)
            .bind(dimensions)
            .bind(vec![0u8; 8])
            .execute(&db)
            .await
            .unwrap();
        }

        let status = embedding_status_with(
            &cfg("http://127.0.0.1:8899/v1"),
            &db,
            "bge-m3",
        )
        .await
        .expect("status");

        assert!(status.leg_enabled);
        assert_eq!(status.total_chunks, 3);
        assert_eq!(status.embedded_chunks, 2);
        assert_eq!(status.placeholder_chunks, 0, "没有占位行");
        assert_eq!(status.dimensions, Some(1024));
        assert_eq!(status.other_models.len(), 1);
        assert_eq!(status.other_models[0].model, "old-model");
        assert_eq!(status.other_models[0].chunks, 1);
        assert_eq!(status.other_models[0].dimensions, 512);
    }

    #[tokio::test]
    async fn status_flags_a_disabled_vector_leg() {
        let db = test_db().await;
        let status = embedding_status_with(&cfg(""), &db, "placeholder")
            .await
            .expect("status");
        assert!(!status.leg_enabled, "占位后端不能算作启用");
    }
}
