// TypeScript interfaces mirroring Rust backend models

export interface Paper {
  id: string;
  title: string;
  authors: string;
  year: number | null;
  journal: string | null;
  doi: string | null;
  url: string | null;
  abstract_text: string | null;
  keywords: string;
  citation_key: string | null;
  bibtex: string | null;
  file_path: string | null;
  file_size: number | null;
  page_count: number | null;
  language: string | null;
  item_type: string | null;
  volume: string | null;
  issue: string | null;
  pages: string | null;
  conference_name: string | null;
  publisher: string | null;
  place: string | null;
  editor: string;
  series: string | null;
  edition: string | null;
  isbn: string | null;
  issn: string | null;
  num_pages: number | null;
  archive_location: string | null;
  call_number: string | null;
  rights: string | null;
  deleted_at: string | null;
  is_favorite: number;
  read_status: string;
  last_read_at: string | null;
  created_at: string;
  updated_at: string;
  imported_at: string;
}

export interface PaperInput {
  title: string;
  authors: string[];
  year?: number | null;
  journal?: string | null;
  doi?: string | null;
  url?: string | null;
  abstract_text?: string | null;
  keywords: string[];
  item_type?: string | null;
  volume?: string | null;
  issue?: string | null;
  pages?: string | null;
  conference_name?: string | null;
  publisher?: string | null;
  place?: string | null;
  editor?: string[];
  series?: string | null;
  edition?: string | null;
  isbn?: string | null;
  issn?: string | null;
  language?: string | null;
  num_pages?: number | null;
  archive_location?: string | null;
  call_number?: string | null;
  rights?: string | null;
}

export interface PaperLinkMetadata {
  title: string;
  authors: string[];
  year: number | null;
  journal: string | null;
  doi: string | null;
  url: string | null;
  abstract_text: string | null;
  keywords: string[];
  pdf_url: string | null;
}

export interface PaperImportResult {
  paper: Paper;
  warning: string | null;
}

export interface LinkImportError {
  code: 'unsupported_url' | 'metadata_not_found' | 'network_error' | 'download_failed' | 'finalize_failed' | 'unknown';
  message: string;
}

export interface ListPapersParams {
  search?: string;
  collection_id?: string;
  tag_ids?: string[];
  tag_logic?: 'and' | 'or';
  sort_by?: 'title' | 'year' | 'imported_at' | 'last_read_at';
  sort_order?: 'asc' | 'desc';
  limit?: number;
  offset?: number;
  /** List only trashed papers when true (default: active only). */
  include_deleted?: boolean;
  /** Year range filter (inclusive). */
  year_from?: number;
  year_to?: number;
  /** Journal filter (case-insensitive substring). */
  journal?: string;
  /** List only favorited papers. */
  is_favorite?: boolean;
  /** Read-status filter. */
  read_status?: 'unread' | 'read' | 'in_progress';
  /** List papers related to this paper id. */
  related_to?: string;
}

export interface PaperDisplay extends Paper {
  authorsList: string[];
  keywordsList: string[];
}

export interface Collection {
  id: string;
  name: string;
  parent_id: string | null;
  sort_order: number;
  created_at: string;
}

export interface Tag {
  id: string;
  name: string;
  color: string;
  parent_id: string | null;
  created_at: string;
  paper_count: number;
}

// ============================================================
// Chat / Agent
// ============================================================

export interface AgentSession {
  id: string;
  title: string;
  mode: string;
  agent_mode: string;
  project_id: string | null;
  working_dir: string | null;
  vision_provider_id: string | null;
  web_proxy: string | null;
  tools_enabled: string[];
  system_prompt: string | null;
  llm_models: LlmConfigBlock[] | null;
  llm_provider_ids: string[] | null;
  approval_config: ApprovalConfig | null;
  max_loops: number | null;
  max_tokens: number | null;
  max_memory_rounds: number | null;
  memory_file_path: string | null;
  memory_dir: string | null;
  skills_dir: string | null;
  is_pinned: boolean;
  sort_order: number;
  icon: string | null;
  color: string | null;
  paper_ids: string;
  domain: string | null;
  context: string | null;
  created_at: string;
  updated_at: string;
}

export interface LlmConfigBlock {
  provider: string;
  model: string;
  api_key: string;
  base_url: string;
  proxy?: string | null;
  max_tokens?: number | null;
  temperature?: number | null;
  extra_body?: Record<string, unknown>;
}

/** A Codex-style project: a local folder the agent works in. */
export interface Project {
  id: string;
  name: string;
  path: string;
  created_at: string;
  updated_at: string;
}

/** A single cross-module activity entry on the timeline page. */
export interface TimelineItem {
  id: string;
  activity_type: string;
  module: string;
  title: string;
  subtitle?: string | null;
  timestamp: string;
  route: string;
  params?: Record<string, string> | null;
  search?: Record<string, string> | null;
}

/** A scheduled agent prompt (cron job). */
export interface CronJob {
  id: string;
  session_id: string;
  cron: string;
  prompt: string;
  recurring: boolean;
  enabled: boolean;
  last_fired: string | null;
  created_at: string;
  updated_at: string;
}

/** A background task (long-running shell command) tracked by the backend. */
export interface TaskInfo {
  id: string;
  description: string;
  status: 'running' | 'completed' | 'failed' | 'stopped' | 'timed_out';
  exit_code: number | null;
  output_path: string | null;
  created_at: string;
  session_id: string | null;
}

/** A structured question the agent asks the user (AskUserQuestion). */
export interface AskQuestion {
  question: string;
  header?: string;
  multi_select?: boolean;
  options: { label: string; description?: string }[];
}

/** User's answer to a question. */
export interface AskAnswer {
  question: string;
  answer: string;
}

export interface LlmProvider {
  id: string;
  name: string;
  provider: string;
  model: string;
  api_key: string;
  base_url: string;
  proxy: string | null;
  max_tokens: number | null;
  temperature: number | null;
  extra_body: string | null;
  is_default: boolean;
  is_vision: boolean;
  sort_order: number;
  created_at: string;
  updated_at: string;
}

export type ApprovalMode = 'auto' | 'auto_expire_time' | 'auto_by_rules' | 'manual' | 'manual_all';

export interface ApprovalConfig {
  mode: ApprovalMode;
  expire_sec?: number;
  whitelist?: string[];
}

export interface ChatAttachment {
  mime: string;
  base64: string;
  name?: string | null;
}

export interface ChatMessage {
  id: string;
  session_id: string;
  role: 'user' | 'assistant' | 'system' | 'tool';
  content: string;
  reasoning_content: string | null;
  tool_calls: string | null;
  citations: string | null;
  model: string | null;
  tokens_used: number | null;
  tokens_in: number | null;
  tokens_in_hit: number | null;
  tokens_out: number | null;
  attachments: string | null; // JSON ChatAttachment[]
  created_at: string;
}

export interface ToolCallInfo {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
  result?: string;
  status: 'pending' | 'running' | 'completed' | 'error' | 'timeout';
  duration_ms?: number;
}

/** A ReAct step while it is still streaming. */
export interface StreamingStep {
  step_index: number;
  reasoning_content: string;
  tool_calls: ToolCallInfo[];
  status: 'streaming' | 'completed';
}

/** A single flattened phase of a ReAct turn: either reasoning or one tool call. */
export type AgentPhase =
  | { kind: 'reasoning'; step_index: number; content: string }
  | { kind: 'tool_call'; step_index: number; toolCall: ToolCallInfo };

export interface AgentStep {
  id: string;
  session_id: string;
  message_id: string | null;
  step_index: number;
  reasoning_content: string | null;
  tool_calls: string | null;
  created_at: string;
}

export interface AgentStreamEvent {
  type: 'thinking' | 'reasoning' | 'tool_call' | 'tool_approval_required' | 'tool_result' | 'step_complete' | 'delta' | 'done' | 'cancelled' | 'ask_user' | 'error';
  session_id: string;
  step_index?: number;
  content?: string;
  tool_call_id?: string;
  tool_name?: string;
  tool_args?: Record<string, unknown>;
  tool_result?: string;
  status?: string;
  duration_ms?: number;
  tokens_used?: number | null;
  tokens_in?: number | null;
  tokens_in_hit?: number | null;
  tokens_out?: number | null;
}

// ============================================================
// LLM Configuration
// ============================================================

export type LlmProviderType =
  | 'openai'
  | 'anthropic'
  | 'deepseek'
  | 'siliconflow'
  | 'ollama'
  | 'qwen'
  | 'zhipu'
  | 'kimi'
  | 'gemini';

export interface LlmConfig {
  provider: LlmProviderType;
  api_key: string;
  base_url: string;
  model: string;
  proxy: string | null;
  max_tokens: number;
  temperature: number;
}

// ============================================================
// Knowledge Domains & Items
// ============================================================

export type KnowledgeDomainType = 'research' | 'learning' | 'life' | 'reading' | 'notes';

export interface KnowledgeDomain {
  id: string;
  name: string;
  domain_type: KnowledgeDomainType;
  icon: string | null;
  color: string;
  sort_order: number;
}

export interface KnowledgeItem {
  id: string;
  domain_id: string;
  title: string;
  content_type: string;
  content: string | null;
  source_type: string | null;
  source_id: string | null;
  tags: string; // JSON array string from Rust backend
  metadata: string; // JSON from Rust backend
  created_at: string;
  updated_at: string;
}

/** Parse tags string from backend into array */
export function parseKnowledgeTags(item: KnowledgeItem): string[] {
  if (!item.tags) return [];
  try { const p = JSON.parse(item.tags); return Array.isArray(p) ? p : []; }
  catch { return []; }
}

// ============================================================
// Research
// ============================================================

export type ResearchSourceStatus = 'discovered' | 'downloaded' | 'imported' | 'read';
export type ResearchSourceType = 'arxiv' | 'scholar' | 'crossref' | 'manual';

export interface ResearchTopic {
  id: string;
  name: string;
  description: string | null;
  keywords: string; // JSON array string from Rust
  status: 'active' | 'paused' | 'completed' | 'archived';
  created_at: string;
  updated_at: string;
}

export interface ResearchSource {
  id: string;
  topic_id: string;
  source_type: ResearchSourceType;
  source_id: string | null;
  title: string | null;
  authors: string | null;
  url: string | null;
  doi: string | null;
  status: ResearchSourceStatus;
  metadata: Record<string, unknown>;
  discovered_at: string;
  processed_at: string | null;
}

export interface Note {
  id: string; vault_id: string;
  title: string; content: string; content_plain: string;
  tags: string; aliases: string;
  is_favorite: number;
  is_folder: number;
  is_system: number;
  source_collection_id: string | null;
  is_excerpt: number;
  agent_edited_at: string | null;
  agent_edit_count: number;
  paper_id: string | null; parent_id: string | null;
  sort_order: number; is_literature_note: number;
  created_at: string; updated_at: string;
}

/** A snapshot of a note captured before an AI edit. */
export interface NoteVersion {
  id: string;
  note_id: string;
  title: string;
  content: string;
  edited_by: string;
  created_at: string;
}

/** Obsidian-style note vault: a named namespace for notes. */
export interface Vault {
  id: string;
  name: string;
  created_at: string;
  updated_at: string;
}

/** A vault-managed file shown in the note tree (content lives in the blob store). */
export interface FileItem {
  id: string;
  vault_id: string;
  parent_id: string | null;
  name: string;
  blob_path: string;
  size: number;
  mime_type: string | null;
  sort_order: number;
  created_at: string;
  updated_at: string;
}

/** Text preview of a managed file (files_read_text). */
export interface TextPreview {
  content: string;
  truncated: boolean;
}

/** Parse a note's JSON tags/aliases column into an array. */
export function parseNoteTags(note: Note): string[] {
  try {
    const v = JSON.parse(note.tags);
    return Array.isArray(v) ? v : [];
  } catch {
    return [];
  }
}
export function parseNoteAliases(note: Note): string[] {
  try {
    const v = JSON.parse(note.aliases);
    return Array.isArray(v) ? v : [];
  } catch {
    return [];
  }
}

export interface Bookmark {
  id: string;
  title: string;
  route: string;
  params_json: string;
  created_at: string;
}

export interface FileEntry {
  name: string; path: string; is_dir: boolean;
  size: number; modified_at: string | null; mime_type: string | null;
}

export interface SystemInfo {
  os: string; arch: string; hostname: string;
  cpu_count: number; memory_total_mb: number;
  memory_used_mb: number | null;
  disk_total_gb: number | null; disk_used_gb: number | null;
}

// ============================================================
// Helpers
// ============================================================

export function parseJsonArray(json: string): string[] {
  try {
    const parsed = JSON.parse(json);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

export function toPaperDisplay(paper: Paper): PaperDisplay {
  return {
    ...paper,
    authorsList: parseJsonArray(paper.authors),
    keywordsList: parseJsonArray(paper.keywords),
  };
}
