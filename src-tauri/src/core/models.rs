use serde::{Deserialize, Serialize};

// ============================================================
// Paper & Library
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Paper {
    pub id: String,
    pub title: String,
    pub authors: String,
    pub year: Option<i32>,
    pub journal: Option<String>,
    pub doi: Option<String>,
    pub url: Option<String>,
    #[sqlx(rename = "abstract")]
    pub abstract_text: Option<String>,
    pub keywords: String,
    pub citation_key: Option<String>,
    pub bibtex: Option<String>,
    pub file_path: Option<String>,
    pub file_size: Option<i64>,
    pub page_count: Option<i32>,
    pub language: Option<String>,
    pub item_type: Option<String>,
    pub volume: Option<String>,
    pub issue: Option<String>,
    pub pages: Option<String>,
    pub conference_name: Option<String>,
    pub publisher: Option<String>,
    pub place: Option<String>,
    pub editor: String,
    pub series: Option<String>,
    pub edition: Option<String>,
    pub isbn: Option<String>,
    pub issn: Option<String>,
    pub num_pages: Option<i32>,
    pub archive_location: Option<String>,
    pub call_number: Option<String>,
    pub rights: Option<String>,
    pub deleted_at: Option<String>,
    pub is_favorite: i64,
    pub read_status: String,
    pub last_read_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub imported_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperInput {
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub journal: Option<String>,
    pub doi: Option<String>,
    pub url: Option<String>,
    pub abstract_text: Option<String>,
    pub keywords: Vec<String>,
    pub item_type: Option<String>,
    pub volume: Option<String>,
    pub issue: Option<String>,
    pub pages: Option<String>,
    pub conference_name: Option<String>,
    pub publisher: Option<String>,
    pub place: Option<String>,
    pub editor: Vec<String>,
    pub series: Option<String>,
    pub edition: Option<String>,
    pub isbn: Option<String>,
    pub issn: Option<String>,
    pub language: Option<String>,
    pub num_pages: Option<i32>,
    pub archive_location: Option<String>,
    pub call_number: Option<String>,
    pub rights: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Tag {
    pub id: String,
    pub name: String,
    pub color: String,
    pub parent_id: Option<String>,
    pub created_at: String,
    /// Number of papers associated with this tag.
    pub paper_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub sort_order: i32,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Note {
    pub id: String,
    pub vault_id: String,
    pub paper_id: Option<String>,
    pub title: String,
    pub content: String,
    pub content_plain: String,
    pub tags: String,
    pub aliases: String,
    pub is_favorite: i32,
    pub is_folder: i32,
    pub is_system: i32,
    pub source_collection_id: Option<String>,
    pub is_excerpt: i32,
    pub agent_edited_at: Option<String>,
    pub agent_edit_count: i32,
    pub parent_id: Option<String>,
    pub sort_order: i32,
    pub is_literature_note: i32,
    pub created_at: String,
    pub updated_at: String,
}

/// Obsidian-style note vault: a named namespace for notes.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Vault {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A vault-managed file (PDF/Word/Excel/image/...) shown in the note tree
/// alongside notes and folders. Content lives in the blob store; `name` is
/// the display name and can change without touching the blob.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileItem {
    pub id: String,
    pub vault_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub blob_path: String,
    pub size: i64,
    pub mime_type: Option<String>,
    pub sort_order: i32,
    pub created_at: String,
    pub updated_at: String,
}

/// Text preview of a managed file (`files_read_text`). `truncated` is true
/// when the file exceeded the preview size cap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextPreview {
    pub content: String,
    pub truncated: bool,
}

/// A snapshot of a note captured before an AI edit (version history).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct NoteVersion {    pub id: String,
    pub note_id: String,
    pub title: String,
    pub content: String,
    pub edited_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub mode: String,
    pub paper_ids: String,
    pub agent_mode: String,
    pub project_id: Option<String>,
    pub working_dir: Option<String>,
    pub vision_provider_id: Option<String>,
    pub web_proxy: Option<String>,
    pub tools_enabled: String,
    pub system_prompt: Option<String>,
    pub llm_models: Option<String>,
    pub llm_provider_ids: Option<String>,
    pub approval_config: Option<String>,
    pub max_loops: Option<i32>,
    pub max_tokens: Option<i32>,
    pub max_memory_rounds: Option<i32>,
    pub memory_file_path: Option<String>,
    pub memory_dir: Option<String>,
    pub skills_dir: Option<String>,
    pub is_pinned: Option<i32>,
    pub sort_order: Option<i32>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub domain: Option<String>,
    pub context: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChatMessage {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub citations: Option<String>,
    pub model: Option<String>,
    pub tokens_used: Option<i32>,
    pub tokens_in: Option<i32>,
    pub tokens_in_hit: Option<i32>,
    pub tokens_out: Option<i32>,
    pub attachments: Option<String>,
    pub created_at: String,
}

/// A Codex-style project: a local folder the agent works in.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for creating/updating a project.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProjectInput {
    pub name: Option<String>,
    pub path: Option<String>,
}

/// A scheduled agent prompt (cron job).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CronJob {
    pub id: String,
    pub session_id: String,
    pub cron: String,
    pub prompt: String,
    pub recurring: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for creating a cron job.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CronJobInput {
    pub session_id: String,
    pub cron: String,
    pub prompt: String,
    pub recurring: Option<bool>,
}

/// One ReAct iteration for an agent session.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AgentStep {
    pub id: String,
    pub session_id: String,
    pub message_id: Option<String>,
    pub step_index: i32,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct LlmProvider {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub proxy: Option<String>,
    pub max_tokens: Option<i32>,
    pub temperature: Option<f64>,
    pub extra_body: Option<String>,
    pub is_default: Option<i32>,
    pub is_vision: Option<bool>,
    pub sort_order: Option<i32>,
    pub created_at: String,
    pub updated_at: String,
}

impl std::fmt::Debug for LlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmProvider")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field(
                "api_key",
                &crate::core::redact::redact_api_key(&self.api_key),
            )
            .field("base_url", &self.base_url)
            .field("proxy", &self.proxy)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("extra_body", &self.extra_body)
            .field("is_default", &self.is_default)
            .field("is_vision", &self.is_vision)
            .field("sort_order", &self.sort_order)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListPapersParams {
    pub search: Option<String>,
    pub collection_id: Option<String>,
    #[serde(default)]
    pub tag_ids: Option<Vec<String>>,
    #[serde(default)]
    pub tag_logic: Option<String>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// When true, list only trashed papers (deleted_at IS NOT NULL);
    /// default lists only active papers.
    #[serde(default)]
    pub include_deleted: Option<bool>,
    /// Year range filter (inclusive).
    #[serde(default)]
    pub year_from: Option<i32>,
    #[serde(default)]
    pub year_to: Option<i32>,
    /// Journal filter (case-insensitive substring).
    #[serde(default)]
    pub journal: Option<String>,
    /// When true, list only favorited papers.
    #[serde(default)]
    pub is_favorite: Option<bool>,
    /// Read-status filter: "unread" | "read" | "in_progress".
    #[serde(default)]
    pub read_status: Option<String>,
    /// List papers related to this paper id.
    #[serde(default)]
    pub related_to: Option<String>,
}

/// A saved (named) advanced search, stored device-locally.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SavedSearch {
    pub id: String,
    pub name: String,
    pub params_json: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Setting {
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChunkData {
    pub id: String,
    pub paper_id: String,
    pub content: String,
    pub page_start: Option<i32>,
    pub page_end: Option<i32>,
    pub section: Option<String>,
    pub chunk_index: i32,
    pub token_count: Option<i32>,
    pub created_at: String,
}

// ============================================================
// Agent & Tool Execution
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ToolExecution {
    pub id: String,
    pub session_id: String,
    pub message_id: Option<String>,
    pub tool_name: String,
    pub tool_input: String,
    pub tool_output: Option<String>,
    pub status: String,
    pub duration_ms: Option<i32>,
    pub created_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionInput {
    pub title: String,
    pub agent_mode: String,
    pub tools_enabled: Vec<String>,
    pub system_prompt: Option<String>,
    /// The project this session belongs to (Codex-style project folder).
    pub project_id: Option<String>,
    /// Sandbox root for file tools; None = full disk access.
    pub working_dir: Option<String>,
    /// Optional vision (multimodal) provider for the agent.
    pub vision_provider_id: Option<String>,
    /// Per-agent web proxy for web_search/web_fetch; None = use global.
    pub web_proxy: Option<String>,
    /// Legacy embedded LLM config blocks.
    #[serde(default)]
    pub llm_models: Vec<crate::ai::agent::config::LlmConfigBlock>,
    /// IDs from the `llm_providers` pool; preferred over `llm_models`.
    #[serde(default)]
    pub llm_provider_ids: Vec<String>,
    pub approval_config: Option<crate::ai::agent::config::ApprovalConfig>,
    pub max_loops: Option<i32>,
    pub max_tokens: Option<i32>,
    pub max_memory_rounds: Option<i32>,
    pub memory_dir: Option<String>,
    pub skills_dir: Option<String>,
}

impl std::fmt::Debug for AgentSessionInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSessionInput")
            .field("title", &self.title)
            .field("agent_mode", &self.agent_mode)
            .field("tools_enabled", &self.tools_enabled)
            .field(
                "system_prompt",
                &self.system_prompt.as_ref().map(|s| s.len()),
            )
            .field("project_id", &self.project_id)
            .field("working_dir", &self.working_dir)
            .field("vision_provider_id", &self.vision_provider_id)
            .field("web_proxy", &self.web_proxy)
            .field("llm_models", &self.llm_models)
            .field("llm_provider_ids", &self.llm_provider_ids)
            .field("approval_config", &self.approval_config)
            .field("max_loops", &self.max_loops)
            .field("max_tokens", &self.max_tokens)
            .field("max_memory_rounds", &self.max_memory_rounds)
            .field("memory_dir", &self.memory_dir)
            .field("skills_dir", &self.skills_dir)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub session_id: String,
    pub content: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<serde_json::Value>,
    pub tool_result: Option<String>,
}

// ============================================================
// Agent Configuration
// ============================================================

#[derive(Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// Legacy inline LLM config. Kept for backward compatibility; prefer `default_llm_provider_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_llm: Option<crate::ai::agent::config::LlmConfigBlock>,
    /// ID of the default LLM provider in the `llm_providers` pool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_llm_provider_id: Option<String>,
    pub default_approval: crate::ai::agent::config::ApprovalConfig,
    pub default_max_loops: i32,
    pub default_max_tokens: i32,
    pub default_max_memory_rounds: i32,

    // ── UI features ──
    #[serde(default = "default_show_pet")]
    pub show_pet: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,

    // ── Advanced truncation / limits ──
    #[serde(default = "default_log_max_size_mb")]
    pub log_max_size_mb: i32,
    #[serde(default = "default_log_max_files")]
    pub log_max_files: i32,
    #[serde(default = "default_log_llm_response_preview_max_chars")]
    pub log_llm_response_preview_max_chars: i32,
    #[serde(default = "default_log_region_detection_preview_max_chars")]
    pub log_region_detection_preview_max_chars: i32,
    #[serde(default = "default_graph_node_label_max_chars")]
    pub graph_node_label_max_chars: i32,
    #[serde(default = "default_region_detection_line_max_chars")]
    pub region_detection_line_max_chars: i32,
    #[serde(default = "default_rag_chunk_max_chars")]
    pub rag_chunk_max_chars: i32,
    #[serde(default = "default_tool_web_fetch_max_chars")]
    pub tool_web_fetch_max_chars: i32,
    #[serde(default = "default_tool_file_read_max_chars")]
    pub tool_file_read_max_chars: i32,
    #[serde(default = "default_tool_paper_read_max_chars")]
    pub tool_paper_read_max_chars: i32,
    /// Per-CALL total output budget for paper_read. The per-chunk cap alone is
    /// not enough — limit=50 × max_chars can dump a whole paper into the
    /// agent's context in one call.
    #[serde(default = "default_tool_paper_read_total_max_chars")]
    pub tool_paper_read_total_max_chars: i32,
    #[serde(default = "default_tool_note_read_max_chars")]
    pub tool_note_read_max_chars: i32,
    #[serde(default = "default_tool_knowledge_read_max_chars")]
    pub tool_knowledge_read_max_chars: i32,

    // ── Embedding backend ──
    /// "hash" (local fallback) | "api" (OpenAI-compatible embeddings endpoint).
    #[serde(default = "default_embedding_backend")]
    pub embedding_backend: String,
    #[serde(default = "default_embedding_base_url")]
    pub embedding_base_url: String,
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,

    // ── RAG context ──
    #[serde(default = "default_rag_max_context_tokens")]
    pub rag_max_context_tokens: i32,

    // ── Research auto-discovery ──
    /// Hours between automatic scans of active topics (0 = disabled).
    #[serde(default = "default_research_auto_discover_interval_hours")]
    pub research_auto_discover_interval_hours: i32,
    /// Max sources discovered per topic per scan.
    #[serde(default = "default_research_discover_max_results")]
    pub research_discover_max_results: i32,
}

fn default_true() -> bool {
    true
}

fn default_empty_string() -> String {
    String::new()
}

/// Device-local settings. These are **not** synced across devices and may
/// contain sensitive values such as API keys.
#[derive(Clone, Serialize, Deserialize)]
pub struct DeviceAppSettings {
    pub data_dir: Option<String>,
    pub memory_dir: Option<String>,
    pub skills_dir: Option<String>,
    #[serde(default = "default_embedding_api_key")]
    pub embedding_api_key: String,
    /// Whether to sync optional data (chat_sessions, chat_messages, global settings).
    #[serde(default = "default_true")]
    pub sync_optional_data: bool,
    /// Stable device identifier used for pairing and sync presence.
    #[serde(default = "default_empty_string")]
    pub device_id: String,
}

impl std::fmt::Debug for DeviceAppSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceAppSettings")
            .field("data_dir", &self.data_dir)
            .field("memory_dir", &self.memory_dir)
            .field("skills_dir", &self.skills_dir)
            .field(
                "embedding_api_key",
                &crate::core::redact::redact_api_key(&self.embedding_api_key),
            )
            .field("sync_optional_data", &self.sync_optional_data)
            .field("device_id", &self.device_id)
            .finish()
    }
}

impl Default for DeviceAppSettings {
    fn default() -> Self {
        Self {
            data_dir: None,
            memory_dir: None,
            skills_dir: None,
            embedding_api_key: default_embedding_api_key(),
            sync_optional_data: true,
            device_id: default_empty_string(),
        }
    }
}

impl std::fmt::Debug for AppSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppSettings")
            .field("default_llm", &self.default_llm)
            .field("default_llm_provider_id", &self.default_llm_provider_id)
            .field("default_approval", &self.default_approval)
            .field("default_max_loops", &self.default_max_loops)
            .field("default_max_tokens", &self.default_max_tokens)
            .field("default_max_memory_rounds", &self.default_max_memory_rounds)
            .field("show_pet", &self.show_pet)
            .field("sidebar_order", &self.sidebar_order)
            .field("homepage", &self.homepage)
            .field("log_max_size_mb", &self.log_max_size_mb)
            .field("log_max_files", &self.log_max_files)
            .field(
                "log_llm_response_preview_max_chars",
                &self.log_llm_response_preview_max_chars,
            )
            .field(
                "log_region_detection_preview_max_chars",
                &self.log_region_detection_preview_max_chars,
            )
            .field(
                "graph_node_label_max_chars",
                &self.graph_node_label_max_chars,
            )
            .field(
                "region_detection_line_max_chars",
                &self.region_detection_line_max_chars,
            )
            .field("rag_chunk_max_chars", &self.rag_chunk_max_chars)
            .field("tool_web_fetch_max_chars", &self.tool_web_fetch_max_chars)
            .field("tool_file_read_max_chars", &self.tool_file_read_max_chars)
            .field("tool_paper_read_max_chars", &self.tool_paper_read_max_chars)
            .field(
                "tool_paper_read_total_max_chars",
                &self.tool_paper_read_total_max_chars,
            )
            .field("tool_note_read_max_chars", &self.tool_note_read_max_chars)
            .field(
                "tool_knowledge_read_max_chars",
                &self.tool_knowledge_read_max_chars,
            )
            .field("embedding_backend", &self.embedding_backend)
            .field("embedding_base_url", &self.embedding_base_url)
            .field("embedding_model", &self.embedding_model)
            .field("rag_max_context_tokens", &self.rag_max_context_tokens)
            .field(
                "research_auto_discover_interval_hours",
                &self.research_auto_discover_interval_hours,
            )
            .field(
                "research_discover_max_results",
                &self.research_discover_max_results,
            )
            .finish()
    }
}

fn default_show_pet() -> bool {
    true
}
fn default_embedding_backend() -> String {
    "hash".into()
}
fn default_embedding_base_url() -> String {
    String::new()
}
fn default_embedding_api_key() -> String {
    String::new()
}
fn default_embedding_model() -> String {
    "text-embedding-3-small".into()
}
fn default_rag_max_context_tokens() -> i32 {
    4000
}
fn default_research_auto_discover_interval_hours() -> i32 {
    6
}
fn default_research_discover_max_results() -> i32 {
    10
}
fn default_log_max_size_mb() -> i32 {
    10
}
fn default_log_max_files() -> i32 {
    5
}
fn default_log_llm_response_preview_max_chars() -> i32 {
    500
}
fn default_log_region_detection_preview_max_chars() -> i32 {
    300
}
fn default_graph_node_label_max_chars() -> i32 {
    50
}
fn default_region_detection_line_max_chars() -> i32 {
    200
}
fn default_rag_chunk_max_chars() -> i32 {
    800
}
fn default_tool_web_fetch_max_chars() -> i32 {
    10000
}
fn default_tool_file_read_max_chars() -> i32 {
    8000
}
fn default_tool_paper_read_max_chars() -> i32 {
    // Matches the default shown in the settings UI. Callers of paper_read
    // can override per call via the max_chars parameter.
    500
}
fn default_tool_paper_read_total_max_chars() -> i32 {
    // Matches the default shown in the settings UI.
    24000
}
fn default_tool_note_read_max_chars() -> i32 {
    200
}
fn default_tool_knowledge_read_max_chars() -> i32 {
    200
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_llm: Some(crate::ai::agent::config::LlmConfigBlock::default()),
            default_llm_provider_id: None,
            default_approval: crate::ai::agent::config::ApprovalConfig::default(),
            default_max_loops: 10,
            default_max_tokens: 28000,
            default_max_memory_rounds: 10,
            show_pet: default_show_pet(),
            sidebar_order: None,
            homepage: None,
            log_max_size_mb: default_log_max_size_mb(),
            log_max_files: default_log_max_files(),
            log_llm_response_preview_max_chars: default_log_llm_response_preview_max_chars(),
            log_region_detection_preview_max_chars: default_log_region_detection_preview_max_chars(
            ),
            graph_node_label_max_chars: default_graph_node_label_max_chars(),
            region_detection_line_max_chars: default_region_detection_line_max_chars(),
            rag_chunk_max_chars: default_rag_chunk_max_chars(),
            tool_web_fetch_max_chars: default_tool_web_fetch_max_chars(),
            tool_file_read_max_chars: default_tool_file_read_max_chars(),
            tool_paper_read_max_chars: default_tool_paper_read_max_chars(),
            tool_paper_read_total_max_chars: default_tool_paper_read_total_max_chars(),
            tool_note_read_max_chars: default_tool_note_read_max_chars(),
            tool_knowledge_read_max_chars: default_tool_knowledge_read_max_chars(),
            embedding_backend: default_embedding_backend(),
            embedding_base_url: default_embedding_base_url(),
            embedding_model: default_embedding_model(),
            rag_max_context_tokens: default_rag_max_context_tokens(),
            research_auto_discover_interval_hours: default_research_auto_discover_interval_hours(),
            research_discover_max_results: default_research_discover_max_results(),
        }
    }
}

// ============================================================
// Knowledge Domains & Items
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct KnowledgeDomain {
    pub id: String,
    pub name: String,
    pub domain_type: String,
    pub icon: Option<String>,
    pub color: String,
    pub sort_order: i32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct KnowledgeItem {
    pub id: String,
    pub domain_id: String,
    pub title: String,
    pub content_type: String,
    pub content: Option<String>,
    pub source_type: Option<String>,
    pub source_id: Option<String>,
    pub metadata: String,
    pub tags: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeItemInput {
    pub domain_id: String,
    pub title: String,
    pub content: Option<String>,
    pub content_type: Option<String>,
    pub source_type: Option<String>,
    pub source_id: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub tags: Vec<String>,
}

// ============================================================
// Research
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ResearchTopic {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub keywords: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchTopicInput {
    pub name: String,
    pub description: Option<String>,
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ResearchSource {
    pub id: String,
    pub topic_id: String,
    pub source_type: String,
    pub source_id: Option<String>,
    pub title: Option<String>,
    pub authors: Option<String>,
    pub url: Option<String>,
    pub doi: Option<String>,
    pub status: String,
    pub metadata: String,
    pub discovered_at: String,
    pub processed_at: Option<String>,
}

// ============================================================
// System
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub os: String,
    pub arch: String,
    pub hostname: String,
    pub cpu_count: u32,
    pub memory_total_mb: u64,
    pub memory_used_mb: Option<u64>,
    pub disk_total_gb: Option<f64>,
    pub disk_used_gb: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_at: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileBookmark {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Bookmark {
    pub id: String,
    pub title: String,
    pub route: String,
    pub params_json: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookmarkInput {
    pub title: String,
    pub route: String,
    pub params_json: Option<String>,
}

// ============================================================
// Annotations (snippets)
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Optional per-line/per-column rectangles for multi-rect selections
    /// (e.g. a selection spanning two columns). Legacy rows omit this field;
    /// the top-level x/y/w/h then acts as the single bounding rect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rects: Option<Vec<AnnotationRect>>,
    /// Optional segments of a multi-range (discontinuous) selection. Each
    /// segment carries its own page so a single annotation can span pages.
    /// Legacy rows omit this field; single-range selections only use rects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<Vec<AnnotationSegment>>,
}

/// W3C Web Annotation style text quote selector: enough context to
/// re-locate the selected text in the page's text content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextQuote {
    pub prefix: String,
    pub exact: String,
    pub suffix: String,
}

/// One contiguous segment of a multi-range selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationSegment {
    /// 1-based PDF page number this segment lives on.
    pub page: i64,
    /// Per-line rectangles of the segment, ratios relative to that page.
    #[serde(default)]
    pub rects: Vec<AnnotationRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<TextQuote>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Annotation {
    pub id: String,
    pub paper_id: String,
    pub page: i64,
    #[sqlx(rename = "type")]
    pub atype: String,
    pub rect: String, // JSON of AnnotationRect
    pub color: String,
    pub text: Option<String>,
    pub note: Option<String>,
    pub tags: String, // JSON array of strings, default '[]'
    pub translation: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationInput {
    pub id: Option<String>,
    pub paper_id: String,
    pub page: i64,
    pub rect: AnnotationRect,
    pub text: Option<String>,
    pub note: Option<String>,
    pub tags: Vec<String>,
}
