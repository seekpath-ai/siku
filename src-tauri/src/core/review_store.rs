use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS review_runs (
    id TEXT PRIMARY KEY NOT NULL,
    status TEXT NOT NULL DEFAULT 'created',
    stage TEXT NOT NULL DEFAULT 'read',
    question TEXT NOT NULL DEFAULT '',
    scope TEXT NOT NULL DEFAULT '',
    language TEXT NOT NULL DEFAULT '中文',
    target_words INTEGER NOT NULL DEFAULT 4000,
    provider_id TEXT,
    paper_ids_json TEXT NOT NULL DEFAULT '[]',
    synthesis_json TEXT,
    outline_json TEXT NOT NULL DEFAULT '[]',
    evidence_registry_json TEXT NOT NULL DEFAULT '[]',
    final_raw_markdown TEXT,
    audit_json TEXT,
    warnings_json TEXT NOT NULL DEFAULT '[]',
    note_id TEXT,
    error TEXT,
    cancelled INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS review_paper_cards (
    run_id TEXT NOT NULL,
    paper_id TEXT NOT NULL,
    paper_index INTEGER NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    authors TEXT NOT NULL DEFAULT '[]',
    year INTEGER,
    journal TEXT,
    doi TEXT,
    url TEXT,
    abstract_text TEXT NOT NULL DEFAULT '',
    source_chunks_json TEXT NOT NULL DEFAULT '[]',
    source_kind TEXT NOT NULL DEFAULT 'unusable',
    status TEXT NOT NULL DEFAULT 'pending',
    chunk_count INTEGER NOT NULL DEFAULT 0,
    abstract_chars INTEGER NOT NULL DEFAULT 0,
    card_json TEXT,
    cache_key TEXT NOT NULL DEFAULT '',
    index_fingerprint TEXT NOT NULL DEFAULT '',
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (run_id, paper_id)
);
CREATE INDEX IF NOT EXISTS idx_review_cards_cache ON review_paper_cards(paper_id, cache_key, status);
CREATE TABLE IF NOT EXISTS review_sections (
    run_id TEXT NOT NULL,
    section_index INTEGER NOT NULL,
    plan_json TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'pending',
    raw_markdown TEXT NOT NULL DEFAULT '',
    attempts INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (run_id, section_index)
);
"#;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ReviewEvidence {
    pub id: String,
    pub chunk_id: Option<String>,
    pub page: Option<i32>,
    pub quote: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ReviewFinding {
    pub claim: String,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ReviewPaperCard {
    pub paper_id: String,
    pub research_question: String,
    pub methods: String,
    pub data_and_sample: String,
    pub findings: Vec<ReviewFinding>,
    pub limitations: String,
    pub relevance: String,
    pub evidence: Vec<ReviewEvidence>,
    pub source_type: String,
    pub edited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSource {
    pub paper_id: String,
    pub paper_index: usize,
    pub title: String,
    pub authors: String,
    pub year: Option<i64>,
    pub source_kind: String,
    pub status: String,
    pub chunk_count: usize,
    pub abstract_chars: usize,
    pub card: Option<ReviewPaperCard>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ReviewSynthesisItem {
    pub title: String,
    pub detail: String,
    pub paper_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ReviewSynthesis {
    pub themes: Vec<ReviewSynthesisItem>,
    pub agreements: Vec<ReviewSynthesisItem>,
    pub conflicts: Vec<ReviewSynthesisItem>,
    pub gaps: Vec<ReviewSynthesisItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ReviewSection {
    pub title: String,
    pub points: String,
    pub subsections: Vec<String>,
    pub paper_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSectionResult {
    pub section_index: usize,
    pub status: String,
    pub raw_markdown: String,
    pub attempts: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ReviewAudit {
    pub selected_count: usize,
    pub fulltext_count: usize,
    pub abstract_count: usize,
    pub unusable_count: usize,
    pub cited_paper_ids: Vec<String>,
    pub uncited_paper_ids: Vec<String>,
    pub invalid_markers: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSnapshot {
    pub run_id: String,
    pub status: String,
    pub stage: String,
    pub question: String,
    pub scope: String,
    pub language: String,
    pub target_words: u32,
    pub provider_id: Option<String>,
    pub paper_ids: Vec<String>,
    pub sources: Vec<ReviewSource>,
    pub outline: Vec<ReviewSection>,
    pub synthesis: Option<ReviewSynthesis>,
    pub sections: Vec<ReviewSectionResult>,
    pub final_raw_markdown: Option<String>,
    pub audit: Option<ReviewAudit>,
    pub note_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SourceChunk {
    pub id: String,
    pub chunk_index: i32,
    pub content: String,
    pub page_start: Option<i32>,
    pub page_end: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct StoredSource {
    pub source: ReviewSource,
    pub journal: Option<String>,
    pub doi: Option<String>,
    pub url: Option<String>,
    pub abstract_text: String,
    pub chunks: Vec<SourceChunk>,
    pub cache_key: String,
    pub index_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredEvidence {
    pub id: String,
    pub paper_id: String,
    pub paper_index: usize,
    pub source_type: String,
    pub chunk_id: Option<String>,
    pub page: Option<i32>,
    pub quote: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ReviewRun {
    pub id: String,
    pub status: String,
    pub stage: String,
    pub question: String,
    pub scope: String,
    pub language: String,
    pub target_words: u32,
    pub provider_id: Option<String>,
    pub paper_ids: Vec<String>,
    pub synthesis: Option<ReviewSynthesis>,
    pub outline: Vec<ReviewSection>,
    pub evidence_registry: Vec<RegisteredEvidence>,
    pub final_raw_markdown: Option<String>,
    pub audit: Option<ReviewAudit>,
    pub warnings: Vec<String>,
    pub note_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReviewRunState {
    pub run: ReviewRun,
    pub sources: Vec<StoredSource>,
    pub sections: Vec<ReviewSectionResult>,
}

impl ReviewRunState {
    pub fn snapshot(&self) -> ReviewSnapshot {
        ReviewSnapshot {
            run_id: self.run.id.clone(),
            status: self.run.status.clone(),
            stage: self.run.stage.clone(),
            question: self.run.question.clone(),
            scope: self.run.scope.clone(),
            language: self.run.language.clone(),
            target_words: self.run.target_words,
            provider_id: self.run.provider_id.clone(),
            paper_ids: self.run.paper_ids.clone(),
            sources: self.sources.iter().map(|s| s.source.clone()).collect(),
            outline: self.run.outline.clone(),
            synthesis: self.run.synthesis.clone(),
            sections: self.sections.clone(),
            final_raw_markdown: self.run.final_raw_markdown.clone(),
            audit: self.run.audit.clone(),
            note_id: self.run.note_id.clone(),
            error: self.run.error.clone(),
        }
    }

    pub fn source(&self, paper_id: &str) -> Option<&StoredSource> {
        self.sources.iter().find(|s| s.source.paper_id == paper_id)
    }

    pub fn usable_sources(&self) -> Vec<&StoredSource> {
        self.sources
            .iter()
            .filter(|s| s.source.status == "done" && s.source.card.is_some())
            .collect()
    }
}

fn decode<T: DeserializeOwned>(text: &str, label: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| format!("{label} JSON 损坏：{e}"))
}

fn encode<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("序列化综述状态失败：{e}"))
}

fn now() -> String {
    crate::core::time::now_iso()
}

#[allow(clippy::too_many_arguments)]
pub async fn create_run(
    db: &SqlitePool,
    question: &str,
    scope: &str,
    language: &str,
    target_words: u32,
    provider_id: Option<&str>,
    sources: Vec<StoredSource>,
) -> Result<ReviewSnapshot, String> {
    if sources.is_empty() {
        return Err("未选择文献".to_string());
    }
    let run_id = uuid::Uuid::new_v4().to_string();
    let paper_ids: Vec<String> = sources.iter().map(|s| s.source.paper_id.clone()).collect();
    let timestamp = now();
    let mut tx = db.begin().await.map_err(|e| format!("db: {e}"))?;
    sqlx::query(
        "INSERT INTO review_runs (id, status, stage, question, scope, language, target_words, provider_id, paper_ids_json, created_at, updated_at)
         VALUES (?, 'created', 'read', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&run_id)
    .bind(question)
    .bind(scope)
    .bind(language)
    .bind(target_words as i64)
    .bind(provider_id)
    .bind(encode(&paper_ids)?)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("db: {e}"))?;

    for stored in &sources {
        let s = &stored.source;
        sqlx::query(
            "INSERT INTO review_paper_cards (
                run_id, paper_id, paper_index, title, authors, year, journal, doi, url,
                abstract_text, source_chunks_json, source_kind, status, chunk_count,
                abstract_chars, card_json, cache_key, index_fingerprint, error, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&run_id)
        .bind(&s.paper_id)
        .bind(s.paper_index as i64)
        .bind(&s.title)
        .bind(&s.authors)
        .bind(s.year)
        .bind(&stored.journal)
        .bind(&stored.doi)
        .bind(&stored.url)
        .bind(&stored.abstract_text)
        .bind(encode(&stored.chunks)?)
        .bind(&s.source_kind)
        .bind(&s.status)
        .bind(s.chunk_count as i64)
        .bind(s.abstract_chars as i64)
        .bind(s.card.as_ref().map(encode).transpose()?)
        .bind(&stored.cache_key)
        .bind(&stored.index_fingerprint)
        .bind(&s.error)
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("db: {e}"))?;
    }
    tx.commit().await.map_err(|e| format!("db: {e}"))?;
    load_state(db, &run_id).await.map(|s| s.snapshot())
}

pub async fn load_state(db: &SqlitePool, run_id: &str) -> Result<ReviewRunState, String> {
    let row = sqlx::query(
        "SELECT id, status, stage, question, scope, language, target_words, provider_id,
                paper_ids_json, synthesis_json, outline_json, evidence_registry_json,
                final_raw_markdown, audit_json, warnings_json, note_id, error
         FROM review_runs WHERE id = ?",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?
    .ok_or_else(|| "综述任务不存在".to_string())?;
    let get = |key: &str| row.try_get::<String, _>(key).map_err(|e| format!("db: {e}"));
    let synthesis_json = row.try_get::<Option<String>, _>("synthesis_json").map_err(|e| format!("db: {e}"))?;
    let audit_json = row.try_get::<Option<String>, _>("audit_json").map_err(|e| format!("db: {e}"))?;
    let run = ReviewRun {
        id: get("id")?,
        status: get("status")?,
        stage: get("stage")?,
        question: get("question")?,
        scope: get("scope")?,
        language: get("language")?,
        target_words: row.try_get::<i64, _>("target_words").map_err(|e| format!("db: {e}"))?.max(0) as u32,
        provider_id: row.try_get("provider_id").map_err(|e| format!("db: {e}"))?,
        paper_ids: decode(&get("paper_ids_json")?, "文献顺序")?,
        synthesis: synthesis_json.as_deref().map(|s| decode(s, "跨文献综合")).transpose()?,
        outline: decode(&get("outline_json")?, "大纲")?,
        evidence_registry: decode(&get("evidence_registry_json")?, "证据注册表")?,
        final_raw_markdown: row.try_get("final_raw_markdown").map_err(|e| format!("db: {e}"))?,
        audit: audit_json.as_deref().map(|s| decode(s, "审计")).transpose()?,
        warnings: decode(&get("warnings_json")?, "警告")?,
        note_id: row.try_get("note_id").map_err(|e| format!("db: {e}"))?,
        error: row.try_get("error").map_err(|e| format!("db: {e}"))?,
    };

    let rows = sqlx::query(
        "SELECT paper_id, paper_index, title, authors, year, journal, doi, url, abstract_text,
                source_chunks_json, source_kind, status, chunk_count, abstract_chars, card_json,
                cache_key, index_fingerprint, error
         FROM review_paper_cards WHERE run_id = ? ORDER BY paper_index",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    let mut sources = Vec::with_capacity(rows.len());
    for row in rows {
        let card_json = row.try_get::<Option<String>, _>("card_json").map_err(|e| format!("db: {e}"))?;
        let chunks_json = row.try_get::<String, _>("source_chunks_json").map_err(|e| format!("db: {e}"))?;
        sources.push(StoredSource {
            source: ReviewSource {
                paper_id: row.try_get("paper_id").map_err(|e| format!("db: {e}"))?,
                paper_index: row.try_get::<i64, _>("paper_index").map_err(|e| format!("db: {e}"))?.max(0) as usize,
                title: row.try_get("title").map_err(|e| format!("db: {e}"))?,
                authors: row.try_get("authors").map_err(|e| format!("db: {e}"))?,
                year: row.try_get("year").map_err(|e| format!("db: {e}"))?,
                source_kind: row.try_get("source_kind").map_err(|e| format!("db: {e}"))?,
                status: row.try_get("status").map_err(|e| format!("db: {e}"))?,
                chunk_count: row.try_get::<i64, _>("chunk_count").map_err(|e| format!("db: {e}"))?.max(0) as usize,
                abstract_chars: row.try_get::<i64, _>("abstract_chars").map_err(|e| format!("db: {e}"))?.max(0) as usize,
                card: card_json.as_deref().map(|s| decode(s, "证据卡")).transpose()?,
                error: row.try_get("error").map_err(|e| format!("db: {e}"))?,
            },
            journal: row.try_get("journal").map_err(|e| format!("db: {e}"))?,
            doi: row.try_get("doi").map_err(|e| format!("db: {e}"))?,
            url: row.try_get("url").map_err(|e| format!("db: {e}"))?,
            abstract_text: row.try_get("abstract_text").map_err(|e| format!("db: {e}"))?,
            chunks: decode(&chunks_json, "证据原文快照")?,
            cache_key: row.try_get("cache_key").map_err(|e| format!("db: {e}"))?,
            index_fingerprint: row.try_get("index_fingerprint").map_err(|e| format!("db: {e}"))?,
        });
    }

    let rows = sqlx::query(
        "SELECT section_index, status, raw_markdown, attempts, error
         FROM review_sections WHERE run_id = ? ORDER BY section_index",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    let mut sections = Vec::with_capacity(rows.len());
    for row in rows {
        sections.push(ReviewSectionResult {
            section_index: row.try_get::<i64, _>("section_index").map_err(|e| format!("db: {e}"))?.max(0) as usize,
            status: row.try_get("status").map_err(|e| format!("db: {e}"))?,
            raw_markdown: row.try_get("raw_markdown").map_err(|e| format!("db: {e}"))?,
            attempts: row.try_get::<i64, _>("attempts").map_err(|e| format!("db: {e}"))?.max(0) as u32,
            error: row.try_get("error").map_err(|e| format!("db: {e}"))?,
        });
    }
    Ok(ReviewRunState { run, sources, sections })
}

pub async fn set_run_status(
    db: &SqlitePool,
    run_id: &str,
    status: &str,
    stage: &str,
    error: Option<&str>,
) -> Result<(), String> {
    sqlx::query("UPDATE review_runs SET status = ?, stage = ?, error = ?, updated_at = ? WHERE id = ?")
        .bind(status)
        .bind(stage)
        .bind(error)
        .bind(now())
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn clear_cancel(db: &SqlitePool, run_id: &str) -> Result<(), String> {
    sqlx::query("UPDATE review_runs SET cancelled = 0, updated_at = ? WHERE id = ?")
        .bind(now())
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn request_cancel(db: &SqlitePool, run_id: &str) -> Result<(), String> {
    sqlx::query("UPDATE review_runs SET cancelled = 1, updated_at = ? WHERE id = ?")
        .bind(now())
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn is_cancelled(db: &SqlitePool, run_id: &str) -> Result<bool, String> {
    let value = sqlx::query_scalar::<_, i64>("SELECT cancelled FROM review_runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?
        .ok_or_else(|| "综述任务不存在".to_string())?;
    Ok(value != 0)
}

pub async fn set_source_status(
    db: &SqlitePool,
    run_id: &str,
    paper_id: &str,
    status: &str,
    error: Option<&str>,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE review_paper_cards SET status = ?, error = ?, updated_at = ? WHERE run_id = ? AND paper_id = ?",
    )
    .bind(status)
    .bind(error)
    .bind(now())
    .bind(run_id)
    .bind(paper_id)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn invalidate_downstream(db: &SqlitePool, run_id: &str) -> Result<(), String> {
    let mut tx = db.begin().await.map_err(|e| format!("db: {e}"))?;
    sqlx::query(
        "UPDATE review_runs SET synthesis_json = NULL, outline_json = '[]', final_raw_markdown = NULL,
                audit_json = NULL, warnings_json = '[]', status = 'read_ready', stage = 'read',
                error = NULL, cancelled = 0, updated_at = ? WHERE id = ?",
    )
    .bind(now())
    .bind(run_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("db: {e}"))?;
    sqlx::query("DELETE FROM review_sections WHERE run_id = ?")
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("db: {e}"))?;
    tx.commit().await.map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn set_card_cache_key(
    db: &SqlitePool,
    run_id: &str,
    paper_id: &str,
    cache_key: &str,
) -> Result<(), String> {
    sqlx::query("UPDATE review_paper_cards SET cache_key = ?, updated_at = ? WHERE run_id = ? AND paper_id = ?")
        .bind(cache_key)
        .bind(now())
        .bind(run_id)
        .bind(paper_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn find_cached_card(
    db: &SqlitePool,
    paper_id: &str,
    cache_key: &str,
) -> Result<Option<ReviewPaperCard>, String> {
    let row = sqlx::query(
        "SELECT card_json FROM review_paper_cards
         WHERE paper_id = ? AND cache_key = ? AND status = 'done' AND card_json IS NOT NULL
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(paper_id)
    .bind(cache_key)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    row.map(|r| {
        let text = r.try_get::<String, _>("card_json").map_err(|e| format!("db: {e}"))?;
        decode(&text, "缓存证据卡")
    })
    .transpose()
}

pub async fn save_card(
    db: &SqlitePool,
    run_id: &str,
    paper_id: &str,
    card: &ReviewPaperCard,
    registry: &[RegisteredEvidence],
    edited: bool,
) -> Result<(), String> {
    let mut tx = db.begin().await.map_err(|e| format!("db: {e}"))?;
    sqlx::query(
        "UPDATE review_paper_cards SET card_json = ?, status = 'done', error = NULL, updated_at = ?
         WHERE run_id = ? AND paper_id = ?",
    )
    .bind(encode(card)?)
    .bind(now())
    .bind(run_id)
    .bind(paper_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("db: {e}"))?;
    sqlx::query(
        "UPDATE review_runs SET evidence_registry_json = ?, synthesis_json = NULL,
                outline_json = '[]', final_raw_markdown = NULL, audit_json = NULL,
                warnings_json = '[]', status = 'reading', stage = 'read', error = NULL, updated_at = ?
         WHERE id = ?",
    )
    .bind(encode(registry)?)
    .bind(now())
    .bind(run_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("db: {e}"))?;
    sqlx::query("DELETE FROM review_sections WHERE run_id = ?")
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("db: {e}"))?;
    tx.commit().await.map_err(|e| format!("db: {e}"))?;
    if edited {
        tracing::info!(run_id, paper_id, "stored user-edited review card");
    }
    Ok(())
}

pub async fn save_synthesis(
    db: &SqlitePool,
    run_id: &str,
    synthesis: &ReviewSynthesis,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE review_runs SET synthesis_json = ?, status = 'read_ready', stage = 'outline', error = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(encode(synthesis)?)
    .bind(now())
    .bind(run_id)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn save_outline(
    db: &SqlitePool,
    run_id: &str,
    outline: &[ReviewSection],
) -> Result<(), String> {
    sqlx::query(
        "UPDATE review_runs SET outline_json = ?, status = 'outline_ready', stage = 'generate', error = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(encode(outline)?)
    .bind(now())
    .bind(run_id)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn prepare_generation(
    db: &SqlitePool,
    run_id: &str,
    outline: &[ReviewSection],
) -> Result<(), String> {
    let state = load_state(db, run_id).await?;
    let preserve = state.run.outline == outline && state.sections.len() == outline.len();
    let mut tx = db.begin().await.map_err(|e| format!("db: {e}"))?;
    if !preserve {
        sqlx::query("DELETE FROM review_sections WHERE run_id = ?")
            .bind(run_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("db: {e}"))?;
    }
    let timestamp = now();
    for (index, section) in outline.iter().enumerate() {
        if preserve {
            sqlx::query(
                "UPDATE review_sections SET plan_json = ?, status = CASE WHEN status = 'done' THEN 'done' ELSE 'pending' END,
                        error = NULL, updated_at = ? WHERE run_id = ? AND section_index = ?",
            )
            .bind(encode(section)?)
            .bind(&timestamp)
            .bind(run_id)
            .bind(index as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("db: {e}"))?;
        } else {
            sqlx::query(
                "INSERT INTO review_sections (run_id, section_index, plan_json, status, updated_at)
                 VALUES (?, ?, ?, 'pending', ?)",
            )
            .bind(run_id)
            .bind(index as i64)
            .bind(encode(section)?)
            .bind(&timestamp)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("db: {e}"))?;
        }
    }
    sqlx::query(
        "UPDATE review_runs SET outline_json = ?, final_raw_markdown = NULL, audit_json = NULL,
                warnings_json = '[]', status = 'outline_ready', stage = 'section', error = NULL,
                cancelled = 0, updated_at = ? WHERE id = ?",
    )
    .bind(encode(outline)?)
    .bind(timestamp)
    .bind(run_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("db: {e}"))?;
    tx.commit().await.map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn prepare_section_retry(
    db: &SqlitePool,
    run_id: &str,
    section_index: usize,
) -> Result<(), String> {
    let changed = sqlx::query(
        "UPDATE review_sections SET status = 'pending', error = NULL, updated_at = ?
         WHERE run_id = ? AND section_index = ?",
    )
    .bind(now())
    .bind(run_id)
    .bind(section_index as i64)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?
    .rows_affected();
    if changed == 0 {
        return Err("节号超出大纲范围".to_string());
    }
    sqlx::query(
        "UPDATE review_runs SET final_raw_markdown = NULL, audit_json = NULL, cancelled = 0,
                status = 'outline_ready', stage = 'section', error = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(now())
    .bind(run_id)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn set_section_running(
    db: &SqlitePool,
    run_id: &str,
    section_index: usize,
) -> Result<u32, String> {
    sqlx::query(
        "UPDATE review_sections SET status = 'running', attempts = attempts + 1, error = NULL, updated_at = ?
         WHERE run_id = ? AND section_index = ?",
    )
    .bind(now())
    .bind(run_id)
    .bind(section_index as i64)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    let attempts = sqlx::query_scalar::<_, i64>(
        "SELECT attempts FROM review_sections WHERE run_id = ? AND section_index = ?",
    )
    .bind(run_id)
    .bind(section_index as i64)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?
    .ok_or_else(|| "节号超出大纲范围".to_string())?;
    Ok(attempts.max(0) as u32)
}

pub async fn set_section_result(
    db: &SqlitePool,
    run_id: &str,
    section_index: usize,
    status: &str,
    raw_markdown: Option<&str>,
    error: Option<&str>,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE review_sections SET status = ?, raw_markdown = COALESCE(?, raw_markdown), error = ?, updated_at = ?
         WHERE run_id = ? AND section_index = ?",
    )
    .bind(status)
    .bind(raw_markdown)
    .bind(error)
    .bind(now())
    .bind(run_id)
    .bind(section_index as i64)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn append_warning(db: &SqlitePool, run_id: &str, warning: &str) -> Result<(), String> {
    let state = load_state(db, run_id).await?;
    if state.run.warnings.iter().any(|w| w == warning) {
        return Ok(());
    }
    let mut warnings = state.run.warnings;
    warnings.push(warning.to_string());
    sqlx::query("UPDATE review_runs SET warnings_json = ?, updated_at = ? WHERE id = ?")
        .bind(encode(&warnings)?)
        .bind(now())
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn save_final(
    db: &SqlitePool,
    run_id: &str,
    raw_markdown: &str,
    audit: &ReviewAudit,
    status: &str,
    stage: &str,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE review_runs SET final_raw_markdown = ?, audit_json = ?, status = ?, stage = ?, error = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(raw_markdown)
    .bind(encode(audit)?)
    .bind(status)
    .bind(stage)
    .bind(now())
    .bind(run_id)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn set_note_id(db: &SqlitePool, run_id: &str, note_id: &str) -> Result<(), String> {
    sqlx::query("UPDATE review_runs SET note_id = ?, updated_at = ? WHERE id = ?")
        .bind(note_id)
        .bind(now())
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn recover_interrupted(db: &SqlitePool, run_id: &str) -> Result<(), String> {
    let state = load_state(db, run_id).await?;
    let active = matches!(state.run.status.as_str(), "reading" | "outlining" | "generating" | "revising")
        || state.sources.iter().any(|s| s.source.status == "running")
        || state.sections.iter().any(|s| s.status == "running");
    if !active {
        return Ok(());
    }
    let mut tx = db.begin().await.map_err(|e| format!("db: {e}"))?;
    sqlx::query(
        "UPDATE review_paper_cards SET status = 'pending', error = NULL, updated_at = ? WHERE run_id = ? AND status = 'running'",
    )
    .bind(now())
    .bind(run_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("db: {e}"))?;
    sqlx::query(
        "UPDATE review_sections SET status = 'pending', error = NULL, updated_at = ? WHERE run_id = ? AND status = 'running'",
    )
    .bind(now())
    .bind(run_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("db: {e}"))?;
    let (status, stage) = if state.run.outline.is_empty() {
        ("read_ready", if state.run.synthesis.is_some() { "outline" } else { "read" })
    } else {
        ("partial", "section")
    };
    sqlx::query("UPDATE review_runs SET status = ?, stage = ?, error = NULL, updated_at = ? WHERE id = ?")
        .bind(status)
        .bind(stage)
        .bind(now())
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("db: {e}"))?;
    tx.commit().await.map_err(|e| format!("db: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_db() -> SqlitePool {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(crate::core::db::SCHEMA_INIT_SQL).execute(&db).await.unwrap();
        sqlx::raw_sql(SCHEMA_SQL).execute(&db).await.unwrap();
        db
    }

    fn source(paper_id: &str, index: usize) -> StoredSource {
        StoredSource {
            source: ReviewSource {
                paper_id: paper_id.into(),
                paper_index: index,
                title: format!("标题 {paper_id}"),
                authors: "[]".into(),
                year: Some(2024),
                source_kind: "fulltext".into(),
                status: "pending".into(),
                chunk_count: 1,
                abstract_chars: 0,
                card: None,
                error: None,
            },
            journal: None,
            doi: None,
            url: None,
            abstract_text: String::new(),
            chunks: vec![SourceChunk {
                id: format!("chunk-{paper_id}"),
                chunk_index: 0,
                content: "冻结的证据原文".into(),
                page_start: Some(1),
                page_end: Some(1),
            }],
            cache_key: String::new(),
            index_fingerprint: "fingerprint".into(),
        }
    }

    #[tokio::test]
    async fn snapshot_and_completed_sections_survive_recovery() {
        let db = test_db().await;
        let snapshot = create_run(
            &db,
            "研究问题",
            "范围",
            "中文",
            3000,
            Some("provider"),
            vec![source("p1", 1), source("p2", 2)],
        )
        .await
        .unwrap();
        assert_eq!(snapshot.paper_ids, vec!["p1", "p2"]);
        assert_eq!(snapshot.provider_id.as_deref(), Some("provider"));
        let outline = vec![
            ReviewSection { title: "第一节".into(), points: "A".into(), subsections: vec![], paper_ids: vec!["p1".into()] },
            ReviewSection { title: "第二节".into(), points: "B".into(), subsections: vec![], paper_ids: vec!["p2".into()] },
        ];
        prepare_generation(&db, &snapshot.run_id, &outline).await.unwrap();
        assert_eq!(set_section_running(&db, &snapshot.run_id, 0).await.unwrap(), 1);
        set_section_result(&db, &snapshot.run_id, 0, "done", Some("完成稿[E1]"), None).await.unwrap();
        assert_eq!(set_section_running(&db, &snapshot.run_id, 1).await.unwrap(), 1);
        set_run_status(&db, &snapshot.run_id, "generating", "section", None).await.unwrap();

        recover_interrupted(&db, &snapshot.run_id).await.unwrap();
        let recovered = load_state(&db, &snapshot.run_id).await.unwrap();
        assert_eq!(recovered.run.status, "partial");
        assert_eq!(recovered.sections[0].status, "done");
        assert_eq!(recovered.sections[0].raw_markdown, "完成稿[E1]");
        assert_eq!(recovered.sections[1].status, "pending");
        assert_eq!(recovered.sections[1].attempts, 1);

        prepare_generation(&db, &snapshot.run_id, &outline).await.unwrap();
        let resumed = load_state(&db, &snapshot.run_id).await.unwrap();
        assert_eq!(resumed.sections[0].status, "done");
        assert_eq!(resumed.sections[0].raw_markdown, "完成稿[E1]");
        assert_eq!(resumed.sections[1].status, "pending");
        assert_eq!(resumed.snapshot().sections.len(), 2);
    }
}
