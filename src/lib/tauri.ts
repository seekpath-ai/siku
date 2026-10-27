import { invoke } from '@tauri-apps/api/core';
import { convertFileSrc } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  Paper, PaperInput, PaperLinkMetadata, PaperImportResult, PaperEnrichMetadataResult, LinkImportError, ListPapersParams,
  AgentSession, AgentStep, ChatMessage, ChatAttachment, LlmConfigBlock, ApprovalConfig, Project, CronJob, TaskInfo,
  KnowledgeDomain, KnowledgeItem,
  ResearchTopic, ResearchSource, Note,
  FileEntry, FileItem, TextPreview, SystemInfo, TimelineItem,
  Collection, Tag, LlmProvider, Vault, NoteVersion, Bookmark,
} from './types';

/**
 * True when an invoke failed with Tauri's "state not managed" error — i.e.
 * the command ran before the backend's async startup manage()d AppState.
 * These always self-heal once init finishes, so queries should keep retrying
 * them instead of settling in an error state.
 */
export function isBackendNotReadyError(err: unknown): boolean {
  return String(err instanceof Error ? err.message : err).includes('state not managed');
}

// ============================================================
// Library
// ============================================================

export async function importPaper(filePath: string): Promise<Paper> {
  return invoke<Paper>('import_paper', { filePath });
}

// ---- Batch import (multi-select / folder / Zotero) ----

/** Progress payload emitted on `library:batch-import-progress`. */
export interface BatchImportProgress {
  batchId: string;
  total: number;
  current: number;
  file: string;
  lastStatus: 'imported' | 'skipped' | 'failed' | null;
  error: string | null;
  done: boolean;
  cancelled: boolean;
  imported: number;
  skipped: number;
  failed: number;
}

export interface BatchImportSummary {
  total: number;
  imported: number;
  skipped: number;
  failed: number;
  cancelled: boolean;
  errors: string[];
}

export const BATCH_IMPORT_PROGRESS_EVENT = 'library:batch-import-progress';

/** Recursively list PDF files under a folder. */
export async function libraryScanFolder(path: string, recursive = true): Promise<string[]> {
  return invoke<string[]>('library_scan_folder', { path, recursive });
}

/** Batch import local PDFs; progress arrives on BATCH_IMPORT_PROGRESS_EVENT. */
export async function importPapersBatch(paths: string[], batchId?: string): Promise<BatchImportSummary> {
  return invoke<BatchImportSummary>('import_papers_batch', { paths, batchId });
}

/** Stop the running batch import after the current item. */
export async function libraryCancelBatchImport(): Promise<void> {
  return invoke<void>('library_cancel_batch_import');
}

export interface ZoteroPreview {
  dataDir: string;
  items: number;
  withPdf: number;
  collections: number;
  tags: number;
}

/** Default Zotero data directory (~/Zotero), if present. */
export async function zoteroDetect(): Promise<string | null> {
  return invoke<string | null>('zotero_detect');
}

export async function zoteroPreview(path?: string): Promise<ZoteroPreview> {
  return invoke<ZoteroPreview>('zotero_preview', { path: path ?? null });
}

/** Import a whole Zotero library (metadata + PDFs + collection tree + tags). */
export async function zoteroImport(path?: string): Promise<BatchImportSummary> {
  return invoke<BatchImportSummary>('zotero_import', { path: path ?? null });
}

/** Preview metadata for a link before importing. */
export async function previewPaperFromLink(url: string): Promise<PaperLinkMetadata> {
  return invoke<PaperLinkMetadata>('preview_paper_from_link', { url });
}

/** Import a paper from a URL (DOI, arXiv, PubMed, Semantic Scholar, or direct PDF). */
export async function importPaperFromLink(
  url: string,
  metadata?: PaperLinkMetadata,
): Promise<PaperImportResult> {
  return invoke<PaperImportResult>('import_paper_from_link', { url, metadata });
}

/** Parse a link-import error thrown by the backend into a structured object. */
export function parseLinkImportError(err: unknown): LinkImportError {
  if (err && typeof err === 'object') {
    const e = err as Partial<LinkImportError>;
    if (e.code && e.message) {
      return { code: e.code as LinkImportError['code'], message: e.message };
    }
  }
  const msg = err instanceof Error ? err.message : String(err);
  // Tauri may serialize the error object as a JSON string in the message.
  if (msg.startsWith('{')) {
    try {
      const parsed = JSON.parse(msg) as Partial<LinkImportError>;
      if (parsed.code && parsed.message) {
        return { code: parsed.code as LinkImportError['code'], message: parsed.message };
      }
    } catch {
      // fall through
    }
  }
  if (msg.includes('[unsupported_url]')) return { code: 'unsupported_url', message: msg };
  if (msg.includes('[metadata_not_found]')) return { code: 'metadata_not_found', message: msg };
  if (msg.includes('[network_error]')) return { code: 'network_error', message: msg };
  if (msg.includes('[download_failed]')) return { code: 'download_failed', message: msg };
  if (msg.includes('[finalize_failed]')) return { code: 'finalize_failed', message: msg };
  return { code: 'unknown', message: msg };
}

export async function listPapers(params?: ListPapersParams): Promise<Paper[]> {
  return invoke<Paper[]>('list_papers', { params: params ?? {} });
}

export async function getPaper(id: string): Promise<Paper> {
  return invoke<Paper>('get_paper', { id });
}

export async function updatePaper(id: string, input: PaperInput): Promise<Paper> {
  return invoke<Paper>('update_paper', { id, input });
}

export async function deletePaper(id: string): Promise<void> {
  return invoke<void>('delete_paper', { id });
}

/** Import metadata from a BibTeX entry (parsed fields override, raw stored). */
export async function paperImportBibtex(paperId: string, bibtex: string): Promise<Paper> {
  return invoke<Paper>('paper_import_bibtex', { paperId, bibtex });
}

/** What a rebuild produced. `croppableFigures` are the ones whose caption has a
 *  bbox, i.e. the ones a `paper_snapshot(label=…)` can crop precisely. */
export interface PaperReprocessResult {
  chunks: number;
  figures: number;
  croppableFigures: number;
}

/** Rebuild a paper's text AND figure index (extract → chunk → figures → embed). */
export async function paperReprocessIndex(id: string): Promise<PaperReprocessResult> {
  return invoke<PaperReprocessResult>('paper_reprocess_index', { id });
}

/** Enrich a paper's bibliographic metadata from CrossRef (DOI/title). */
export async function paperEnrichMetadata(id: string): Promise<PaperEnrichMetadataResult> {
  return invoke<PaperEnrichMetadataResult>('paper_enrich_metadata', { id });
}

export async function collectionsList(): Promise<Collection[]> {
  return invoke<Collection[]>('collections_list');
}
export async function collectionsGet(id: string): Promise<Collection> {
  return invoke<Collection>('collections_get', { id });
}
export async function collectionsCreate(name: string, parentId?: string): Promise<Collection> {
  return invoke<Collection>('collections_create', { name, parentId });
}
export async function collectionsUpdate(id: string, name?: string, parentId?: string | null): Promise<Collection> {
  return invoke<Collection>('collections_update', { id, name, parentId });
}
export async function collectionsDelete(id: string): Promise<void> {
  return invoke<void>('collections_delete', { id });
}
export async function collectionsAddPapers(collectionId: string, paperIds: string[]): Promise<void> {
  return invoke<void>('collections_add_papers', { collectionId, paperIds });
}
export async function collectionsRemovePapers(collectionId: string, paperIds: string[]): Promise<void> {
  return invoke<void>('collections_remove_papers', { collectionId, paperIds });
}

/** Collections that a paper belongs to (Zotero-style display). */
export async function paperGetCollections(paperId: string): Promise<Collection[]> {
  return invoke<Collection[]>('paper_get_collections', { paperId });
}

export async function tagsList(): Promise<Tag[]> {
  return invoke<Tag[]>('tags_list');
}
export async function tagsGet(id: string): Promise<Tag> {
  return invoke<Tag>('tags_get', { id });
}
export async function tagsCreate(name: string, color?: string): Promise<Tag> {
  return invoke<Tag>('tags_create', { name, color });
}
export async function tagsDelete(id: string): Promise<void> {
  return invoke<void>('tags_delete', { id });
}
export async function tagsUpdate(id: string, name?: string, color?: string): Promise<Tag> {
  return invoke<Tag>('tags_update', { id, name, color });
}
export async function tagsPapers(paperId: string): Promise<Tag[]> {
  return invoke<Tag[]>('tags_papers', { paperId });
}
export async function tagsAddToPaper(paperId: string, tagIds: string[]): Promise<void> {
  return invoke<void>('tags_add_to_paper', { paperId, tagIds });
}
export async function tagsRemoveFromPaper(paperId: string, tagIds: string[]): Promise<void> {
  return invoke<void>('tags_remove_from_paper', { paperId, tagIds });
}
export async function tagsListPapers(tagId: string, sortBy?: ListPapersParams['sort_by'], sortOrder?: ListPapersParams['sort_order']): Promise<Paper[]> {
  return invoke<Paper[]>('tags_list_papers', { tagId, sortBy, sortOrder });
}

export async function bookmarksList(): Promise<Bookmark[]> {
  return invoke<Bookmark[]>('bookmarks_list');
}
export async function bookmarksCreate(input: { title: string; route: string; params_json?: string }): Promise<Bookmark> {
  return invoke<Bookmark>('bookmarks_create', { input });
}
export async function bookmarksDelete(id: string): Promise<void> {
  return invoke<void>('bookmarks_delete', { id });
}

export async function readPdfBytes(paperId: string): Promise<string> {
  const path: string = await invoke<string>('read_pdf_bytes', { paperId });
  return convertFileSrc(path);
}

/** Export a paper's PDF to an absolute filesystem path. */
export async function exportPdf(paperId: string, targetPath: string): Promise<void> {
  return invoke<void>('export_pdf', { paperId, targetPath });
}

// ============================================================
// Agent
// ============================================================

export interface AgentSessionInput {
  title: string;
  agentMode?: string;
  toolsEnabled?: string[];
  systemPrompt?: string;
  /** The project this session belongs to; null on update = unbind. */
  projectId?: string | null;
  /** Sandbox root for file tools; null = full disk access. */
  workingDir?: string | null;
  /** Optional vision (multimodal) provider for the agent. */
  visionProviderId?: string | null;
  /** Per-agent web proxy (web_search/web_fetch); empty = use global. */
  webProxy?: string | null;
  llmModels?: LlmConfigBlock[];
  llmProviderIds?: string[];
  approvalConfig?: ApprovalConfig;
  maxLoops?: number;
  /** Per-round output cap (ReAct limit); unset = follow the model config. */
  maxTokens?: number;
  /** Conversation context truncation budget; unset = global default. */
  contextBudget?: number;
  maxMemoryRounds?: number;
  memoryDir?: string;
  skillsDir?: string;
  /** Skills mounted on this session; undefined on update = leave untouched. */
  selectedSkills?: string[];
}

export async function agentCreateSession(input: AgentSessionInput): Promise<AgentSession> {
  return invoke<AgentSession>('agent_create_session', { input });
}

export async function agentUpdateSession(
  sessionId: string,
  input: AgentSessionInput,
): Promise<AgentSession> {
  return invoke<AgentSession>('agent_update_session', { sessionId, input });
}

export async function agentGetSession(sessionId: string): Promise<AgentSession> {
  return invoke<AgentSession>('agent_get_session', { sessionId });
}

/** Quick-switch a session's model from the chat header badge. Writes ONLY
 *  the llm columns (agentUpdateSession is a full-config overwrite); takes
 *  effect on the next turn. */
export async function agentSetSessionModel(
  sessionId: string,
  llmProviderIds: string[],
  llmModels: LlmConfigBlock[],
): Promise<void> {
  return invoke<void>('agent_set_session_model', { sessionId, llmProviderIds, llmModels });
}

/** Anchored paragraph from the dual-pane extraction (bbox 为 PDF 点坐标，y 自下而上). */
export interface PaperParagraph {
  page: number;
  /** [x0, y0(bottom), x1, y1(top)] PDF points; null = 仅页级锚定。 */
  bbox: [number, number, number, number] | null;
  text: string;
}

/** Anchored paragraphs for the reader's dual-pane view (懒计算+本地缓存). */
export async function paperGetParagraphs(id: string): Promise<PaperParagraph[]> {
  return invoke<PaperParagraph[]>('paper_get_paragraphs', { id });
}

/** Create a pet (built-in domain agent) session bound to the current context. */
export async function petCreateSession(domain: string, context: Record<string, unknown>): Promise<AgentSession> {
  return invoke<AgentSession>('pet_create_session', { domain, context });
}

/** System-prompt snapshot stored for a user message's turn (本地表，不同步). */
export interface TurnContext {
  system_prompt: string;
  /** JSON array of the exact history records sent with the turn
   *  ([{role, content, time}]); null for snapshots predating the column. */
  history: string | null;
  created_at: string;
}

/** Fetch the turn context snapshot for a user message; null when the turn
 *  predates the snapshot feature. */
export async function agentGetTurnContext(messageId: string): Promise<TurnContext | null> {
  return invoke<TurnContext | null>('agent_get_turn_context', { messageId });
}

export interface PetDomainInfo {
  id: string;
  name: string;
  default_prompt: string;
  default_max_tokens: number | null;
}

/** List built-in pet domain agents with their default system prompts. */
export interface TaskPromptPlaceholder {
  name: string;
  description: string;
}

/** A system prompt behind a batch feature (AI 综述 / 表格列 / 分类 …). */
export interface TaskPromptInfo {
  id: string;
  /** Feature the prompt belongs to. */
  group: string;
  name: string;
  description: string;
  /** Built-in text; the settings page shows it as the textarea placeholder. */
  defaultPrompt: string;
  /** What is actually in use right now. */
  effectivePrompt: string;
  overridden: boolean;
  placeholders: TaskPromptPlaceholder[];
  /** Rewriting the JSON contract in this prompt breaks the feature. */
  affectsParsing: boolean;
}

/** Per-feature "单轮输出上限" (a floor: it only raises the automatic budget). */
export interface TaskPromptGroup {
  id: string;
  name: string;
  description: string;
  builtinDefault: number;
  overrideValue: number | null;
  effective: number;
}

export async function taskPromptGroups(): Promise<TaskPromptGroup[]> {
  return invoke<TaskPromptGroup[]>('task_prompt_groups');
}

export async function taskPromptsList(): Promise<TaskPromptInfo[]> {
  return invoke<TaskPromptInfo[]>('task_prompts_list');
}

export async function petDomains(): Promise<PetDomainInfo[]> {
  return invoke<PetDomainInfo[]>('pet_domains');
}

export async function agentSendMessage(
  sessionId: string,
  content: string,
  attachments?: ChatAttachment[],
): Promise<void> {
  return invoke<void>('agent_send_message', { sessionId, content, attachments });
}

/** Tool-approval decision. "approve" accepts optional modifiedArgs (the tool
 * runs with replaced arguments); "decline" continues the turn;
 * "decline_guide" continues with feedback handed to the agent;
 * "decline_stop" ends the whole turn. */
export type ApprovalDecision = 'approve' | 'decline' | 'decline_guide' | 'decline_stop';

export async function agentApproveTool(
  sessionId: string,
  toolCallId: string,
  decision: ApprovalDecision,
  opts?: { guidance?: string; modifiedArgs?: Record<string, unknown> },
): Promise<void> {
  return invoke<void>('agent_approve_tool', {
    sessionId,
    toolCallId,
    decision,
    guidance: opts?.guidance ?? null,
    modifiedArgs: opts?.modifiedArgs ?? null,
  });
}

/** Cancel a running agent turn for a session. */
export async function agentCancel(sessionId: string): Promise<void> {
  return invoke<void>('agent_cancel', { sessionId });
}

/** Whether a turn is running for the session — used to heal stale
 *  streaming/loading flags after terminal events were missed (the event
 *  listener unmounts with the chat route). */
export async function agentIsRunning(sessionId: string): Promise<boolean> {
  return invoke<boolean>('agent_is_running', { sessionId });
}

/** Update only the session's approval config (chat-input quick switch). */
export async function agentSetApprovalConfig(
  sessionId: string,
  approvalConfig: ApprovalConfig,
): Promise<void> {
  return invoke<void>('agent_set_approval_config', { sessionId, approvalConfig });
}

/** Absolute path of the session's automatic workspace
 *  (`app_data_dir/agent_workspaces/<session_id>`). This is where relative paths
 *  land when the session has no write directory. The backend creates the
 *  directory on demand, so a successful call always names an existing path. */
export async function agentSessionWorkspace(sessionId: string): Promise<string> {
  return invoke<string>('agent_session_workspace', { sessionId });
}

/** Clear the session's automatic workspace (its files are removed).
 *  Irreversible — confirm with the user before calling. Whether the directory
 *  itself survives is the backend's decision; the frontend only re-reads the
 *  path afterwards and never assumes a shape. */
export async function agentSessionWorkspaceClear(sessionId: string): Promise<void> {
  return invoke<void>('agent_session_workspace_clear', { sessionId });
}

/** Launch the OS-native region screenshot tool; the captured image lands in
 * the clipboard and is attached automatically on window refocus. */
export async function screenshotStart(): Promise<string> {
  return invoke<string>('screenshot_start');
}

/** (Re)apply the global screenshot hotkey per settings (app start + toggle). */
export async function screenshotHotkeySync(): Promise<void> {
  return invoke<void>('screenshot_hotkey_sync');
}

/** Open a local file with the OS default application. Executable extensions
 * are refused backend-side; directories are revealed instead. */
export async function openLocalPath(path: string): Promise<void> {
  return invoke<void>('open_local_path', { path });
}

/** Select the file in the OS file manager (Linux: opens its folder). */
export async function revealInFileManager(path: string): Promise<void> {
  return invoke<void>('reveal_in_file_manager', { path });
}

/** Resolve a prose path candidate (may include glued text) to a path that
 * exists on disk; null means dead candidate — render it as plain text. */
export async function resolveExistingPath(candidate: string): Promise<string | null> {
  return invoke<string | null>('resolve_existing_path', { candidate });
}

/** Rename a session (title only). */
export async function agentRenameSession(sessionId: string, title: string): Promise<void> {
  return invoke<void>('agent_rename_session', { sessionId, title });
}

/** Answer a pending AskUserQuestion dialog for a session.
 *
 *  `answers` is the per-question reply; `note` is the user's free-form message
 *  (optional). The backend hands both to the model together, with the note
 *  marked as authoritative — it is how the user says "stop asking, do this"
 *  without abandoning the questions. */
export async function agentAnswerUser(
  sessionId: string,
  answers: { question: string; answer: string; custom?: boolean }[],
  note?: string,
): Promise<void> {
  return invoke<void>('agent_answer_user', { sessionId, answers, note: note ?? '' });
}

// ============================================================
// Cron (scheduled agent prompts)
// ============================================================

export async function cronCreate(input: {
  sessionId: string;
  cron: string;
  prompt: string;
  recurring?: boolean;
}): Promise<CronJob> {
  return invoke<CronJob>('cron_create', { input });
}

export async function cronList(): Promise<CronJob[]> {
  return invoke<CronJob[]>('cron_list');
}

export async function cronDelete(id: string): Promise<void> {
  return invoke<void>('cron_delete', { id });
}

/** Enable/disable a cron job without deleting it. */
export async function cronSetEnabled(id: string, enabled: boolean): Promise<void> {
  return invoke<void>('cron_set_enabled', { id, enabled });
}

// ============================================================
// Background tasks (long-running shell commands)
// ============================================================

export interface TaskOutput {
  status: string;
  exit_code: number | null;
  content: string;
  truncated: boolean;
}

export async function taskSnapshot(): Promise<TaskInfo[]> {
  return invoke<TaskInfo[]>('task_snapshot');
}

export async function taskOutput(id: string): Promise<TaskOutput> {
  return invoke<TaskOutput>('task_output', { id });
}

export async function taskStop(id: string): Promise<void> {
  return invoke<void>('task_stop', { id });
}

export async function agentListSessions(projectId?: string): Promise<AgentSession[]> {
  return invoke<AgentSession[]>('agent_list_sessions', { projectId });
}

export async function agentDeleteSession(sessionId: string): Promise<void> {
  return invoke<void>('agent_delete_session', { sessionId });
}

export async function agentPinSession(sessionId: string, pinned: boolean): Promise<void> {
  return invoke<void>('agent_pin_session', { sessionId, pinned });
}

/** Archive/restore a session (archived sessions hide from the sidebar list). */
export async function agentArchiveSession(sessionId: string, archived: boolean): Promise<void> {
  return invoke<void>('agent_archive_session', { sessionId, archived });
}

export async function getAgentSteps(sessionId: string): Promise<AgentStep[]> {
  return invoke<AgentStep[]>('get_agent_steps', { sessionId });
}

/** One search engine entry in the ordered web_search fallback chain. */
export interface SearchEngineConfig {
  /** bing | duckduckgo | tavily | brave | deepseek | searxng */
  id: string;
  enabled: boolean;
  /** tavily / brave / deepseek（deepseek 留空时回退 LLM 提供商池的 DeepSeek Key） */
  apiKey?: string | null;
  /** searxng */
  baseUrl?: string | null;
}

export interface AppSettings {
  default_llm?: LlmConfigBlock;
  default_llm_provider_id?: string | null;
  default_approval: ApprovalConfig;
  default_max_loops: number;
  default_context_budget: number;
  default_max_memory_rounds: number;
  data_dir?: string | null;
  memory_dir?: string | null;
  skills_dir?: string | null;

  // UI features
  show_pet?: boolean;
  /** System-wide Ctrl+Shift+S screenshot hotkey (default on). */
  global_screenshot_hotkey?: boolean;
  /** Ordered web_search fallback chain. */
  search_engines?: SearchEngineConfig[];
  sidebar_order?: string[] | null;
  homepage?: string | null;

  // Advanced truncation / limits
  log_max_size_mb: number;
  log_max_files: number;
  log_llm_response_preview_max_chars: number;
  log_region_detection_preview_max_chars: number;
  graph_node_label_max_chars: number;
  region_detection_line_max_chars: number;
  rag_chunk_max_chars: number;
  tool_web_fetch_max_chars: number;
  tool_file_read_max_chars: number;
  tool_paper_read_max_chars: number;
  tool_paper_read_total_max_chars: number;
  tool_note_read_max_chars: number;
  tool_knowledge_read_max_chars: number;

  // Embedding backend
  embedding_base_url?: string;
  embedding_api_key?: string;
  embedding_model?: string;

  // RAG context
  rag_max_context_tokens?: number;

  // Research auto-discovery
  research_auto_discover_interval_hours?: number;
  research_discover_max_results?: number;
}

export async function settingsAppGet(): Promise<AppSettings> {
  return invoke<AppSettings>('settings_app_get');
}

export async function settingsAppSave(settings: AppSettings): Promise<void> {
  return invoke<void>('settings_app_save', { settings });
}

// ============================================================
// LLM Providers
// ============================================================

export interface LlmProviderInput {
  name: string;
  provider: string;
  model: string;
  api_key: string;
  base_url: string;
  proxy?: string | null;
  max_tokens?: number | null;
  temperature?: number | null;
  extra_body?: string | null;
  is_default?: boolean;
  is_vision?: boolean;
}

export async function llmProviderList(): Promise<LlmProvider[]> {
  return invoke<LlmProvider[]>('llm_provider_list');
}
export async function llmProviderGet(id: string): Promise<LlmProvider> {
  return invoke<LlmProvider>('llm_provider_get', { id });
}
export async function llmProviderCreate(input: LlmProviderInput): Promise<LlmProvider> {
  return invoke<LlmProvider>('llm_provider_create', { input });
}
export async function llmProviderUpdate(id: string, input: LlmProviderInput): Promise<LlmProvider> {
  return invoke<LlmProvider>('llm_provider_update', { id, input });
}
export async function llmProviderDelete(id: string): Promise<void> {
  return invoke<void>('llm_provider_delete', { id });
}
export async function llmProviderSetDefault(id: string): Promise<LlmProvider> {
  return invoke<LlmProvider>('llm_provider_set_default', { id });
}
export async function llmProviderValidate(id: string): Promise<boolean> {
  return invoke<boolean>('llm_provider_validate', { id });
}

export async function settingsGetMemoryDir(): Promise<string> {
  return invoke<string>('settings_get_memory_dir');
}

export async function settingsGetSkillsDir(): Promise<string> {
  return invoke<string>('settings_get_skills_dir');
}

export async function settingsEnsureDirectories(): Promise<void> {
  return invoke<void>('settings_ensure_directories');
}

// ============================================================
// Chat
// ============================================================

export async function listChatSessions(projectId?: string): Promise<AgentSession[]> {
  return invoke<AgentSession[]>('list_chat_sessions', { projectId });
}

export async function createChatSession(
  title: string,
  mode?: string,
  agentMode?: string,
  toolsEnabled?: string[],
  systemPrompt?: string,
  projectId?: string,
): Promise<AgentSession> {
  return invoke<AgentSession>('create_chat_session', {
    title,
    mode,
    agentMode,
    toolsEnabled,
    systemPrompt,
    projectId,
  });
}

export async function deleteChatSession(sessionId: string): Promise<void> {
  return invoke<void>('delete_chat_session', { sessionId });
}

export interface ChatMessagePage {
  messages: ChatMessage[];
  /** Older messages exist beyond the page's first row. */
  hasMore: boolean;
}

/** Paginated message history. No opts = full history (pet panel etc.);
 *  `{ limit }` = latest page; `{ before, limit }` = older page (scroll-up);
 *  `{ after }` = incremental rows newer than the cursor (post-turn reload). */
export async function getChatMessages(
  sessionId: string,
  opts?: {
    before?: { createdAt: string; id: string };
    after?: { createdAt: string; id: string };
    limit?: number;
  }
): Promise<ChatMessagePage> {
  return invoke<ChatMessagePage>('get_chat_messages', {
    sessionId,
    beforeCreatedAt: opts?.before?.createdAt ?? null,
    beforeId: opts?.before?.id ?? null,
    afterCreatedAt: opts?.after?.createdAt ?? null,
    afterId: opts?.after?.id ?? null,
    limit: opts?.limit ?? null,
  });
}

/** Tag a message from the bubble action bar. 'experience' copies the content
 *  into the session's long-term memory, 'knowledge' into the knowledge base,
 *  'chitchat' only marks; null clears the tag. Returns the updated message. */
export async function chatMessageTag(
  messageId: string,
  tag: 'experience' | 'knowledge' | 'chitchat' | null,
): Promise<ChatMessage> {
  return invoke<ChatMessage>('chat_message_tag', { messageId, tag });
}

export interface ChatMessageHit {
  message_id: string;
  session_id: string;
  session_title: string;
  role: string;
  snippet: string;
  created_at: string;
}

/** Search chat message content across conversations (or one session when
 *  sessionId is passed). Whitespace-split keywords are ANDed. */
export async function chatSearchMessages(
  query: string,
  sessionId?: string,
  limit?: number,
): Promise<ChatMessageHit[]> {
  return invoke<ChatMessageHit[]>('search_chat_messages', {
    query,
    sessionId: sessionId ?? null,
    limit: limit ?? null,
  });
}

// ---- Plugins (skills) ----

export interface SkillReviewBadge {
  verdict: 'pass' | 'warn' | 'risk';
  /** Skill content changed since the review → treat as unreviewed. */
  stale: boolean;
}

export interface SkillReviewFinding {
  severity: string;
  category: string;
  file: string;
  line: number;
  excerpt: string;
  rule: string;
}

export interface SkillReviewDependency {
  name: string;
  kind: string;
  available: boolean;
}

export interface SkillReviewRecord {
  verdict: 'pass' | 'warn' | 'risk';
  staticVerdict: string;
  llmVerdict: string | null;
  summary: string;
  findings: SkillReviewFinding[];
  dependencies: SkillReviewDependency[];
  contentHash: string;
  reviewedAt: string;
}

export interface SkillInfo {
  name: string;
  description: string;
  /** Directory holding SKILL.md. */
  path: string;
  /** Latest review badge; absent = never reviewed. */
  review?: SkillReviewBadge | null;
}

export interface SkillDetail {
  name: string;
  description: string;
  path: string;
  /** SKILL.md body (the instructions injected when the skill tool runs). */
  content: string;
  /** Latest review record; absent = never reviewed. */
  review?: SkillReviewRecord | null;
}

export async function skillsList(): Promise<SkillInfo[]> {
  return invoke<SkillInfo[]>('skills_list');
}

/** Full skill detail for the plugins dialog's second-level view. */
export async function skillsGet(name: string): Promise<SkillDetail> {
  return invoke<SkillDetail>('skills_get', { name });
}

/** Static scan report (deterministic, pre-LLM). */
export interface SkillStaticReport {
  findings: SkillReviewFinding[];
  dependencies: SkillReviewDependency[];
  verdict: string;
  contentHash: string;
  scannedFiles: string[];
}

/** Start an AI review (static scan + no-tool reviewer session). */
export async function skillsReviewStart(
  name: string
): Promise<{ sessionId: string; report: SkillStaticReport }> {
  return invoke('skills_review_start', { name });
}

/** Collect + persist the verdict after the review turn completes. */
export async function skillsReviewCollect(sessionId: string): Promise<SkillReviewRecord> {
  return invoke<SkillReviewRecord>('skills_review_collect', { sessionId });
}

/** Import a skill from a folder containing SKILL.md. */
export async function skillsImportFolder(path: string): Promise<SkillInfo> {
  return invoke<SkillInfo>('skills_import_folder', { path });
}

/** Import a skill from a zip archive. */
export async function skillsImportZip(path: string): Promise<SkillInfo> {
  return invoke<SkillInfo>('skills_import_zip', { path });
}

/** Delete an installed skill (removes its directory). */
export async function skillsDelete(name: string): Promise<void> {
  return invoke<void>('skills_delete', { name });
}

/** Replace the session's mounted skill list (per-session plugins). */
export async function agentSetSessionSkills(sessionId: string, skills: string[]): Promise<void> {
  return invoke<void>('agent_set_session_skills', { sessionId, skills });
}

/** Open the user skills (plugins) directory in the OS file manager. */
export async function skillsOpenDirectory(): Promise<void> {
  return invoke<void>('skills_open_directory');
}

// Agent long-term memory (per chat session, brain button in the input area).
// scope: 'global' (default — synced across devices) | 'device' (this machine
// only, never synced; for local environment facts).
export type MemoryScope = 'global' | 'device';

export interface AgentMemory {
  id: string;
  content: string;
  active: boolean;
  updated_at: string;
}

export async function agentMemoryGet(sessionId: string, scope?: MemoryScope): Promise<AgentMemory | null> {
  return invoke<AgentMemory | null>('agent_memory_get', { sessionId, scope });
}

export async function agentMemorySet(sessionId: string, content: string, scope?: MemoryScope): Promise<void> {
  return invoke<void>('agent_memory_set', { sessionId, content, scope });
}

export async function agentMemorySetActive(sessionId: string, active: boolean, scope?: MemoryScope): Promise<void> {
  return invoke<void>('agent_memory_set_active', { sessionId, active, scope });
}

export async function agentMemoryRestore(sessionId: string, versionId: string, scope?: MemoryScope): Promise<void> {
  return invoke<void>('agent_memory_restore', { sessionId, versionId, scope });
}

export interface AgentMemoryReview {
  /** Human-readable list of the changes the reviewer proposes. */
  notes: string;
  /** The full revised memory document (never auto-applied). */
  revised: string;
}

/** Ask the session's own model to review the long-term memory and propose a revision. */
export async function agentMemoryReview(sessionId: string, scope?: MemoryScope): Promise<AgentMemoryReview> {
  return invoke<AgentMemoryReview>('agent_memory_review', { sessionId, scope });
}

// ============================================================
// Projects (Codex-style project folders)
// ============================================================

export async function projectsList(): Promise<Project[]> {
  return invoke<Project[]>('projects_list');
}

export async function projectCreate(input: { name?: string; path: string; gitInit?: boolean }): Promise<Project> {
  return invoke<Project>('project_create', { input });
}

/** Whether the system git binary is available (new-project dialog checkbox). */
export async function gitAvailable(): Promise<boolean> {
  return invoke<boolean>('git_available');
}

// ---- Git hosting (PR) ----

export interface GitRemoteInfo {
  platform: 'github' | 'gitee';
  owner: string;
  repo: string;
  branch: string;
  remoteUrl: string;
}

/** Inspect a project's origin remote + current branch (errors when the
 *  project is not a git repo / has no origin / unsupported host). */
export async function gitRemoteInfo(projectId: string): Promise<GitRemoteInfo> {
  return invoke<GitRemoteInfo>('git_remote_info', { projectId });
}

/** Push the current branch to origin using the user's own git credentials. */
export async function gitPushBranch(projectId: string): Promise<string> {
  return invoke<string>('git_push_branch', { projectId });
}

export async function gitHostTokenStatus(): Promise<{ github: boolean; gitee: boolean }> {
  return invoke('git_host_token_status');
}

/** Store a platform PAT (device-local, used only for the platform API). */
export async function gitHostSetToken(platform: 'github' | 'gitee', token: string): Promise<void> {
  return invoke<void>('git_host_set_token', { platform, token });
}

export async function prCreate(
  projectId: string,
  title: string,
  body?: string,
  base?: string,
): Promise<{ htmlUrl: string; number: number }> {
  return invoke('pr_create', { projectId, title, body: body ?? null, base: base ?? null });
}

export async function projectUpdate(id: string, input: { name?: string }): Promise<Project> {
  return invoke<Project>('project_update', { id, input });
}

/** Archive/restore a project (archived projects hide from the sidebar list). */
export async function projectSetArchived(id: string, archived: boolean): Promise<void> {
  return invoke<void>('project_set_archived', { id, archived });
}

export async function projectDelete(id: string): Promise<void> {
  return invoke<void>('project_delete', { id });
}

// ============================================================
// Settings
// ============================================================

export async function settingsGet(key: string): Promise<string | null> {
  return invoke<string | null>('settings_get', { key });
}

export async function settingsSet(key: string, value: string): Promise<void> {
  return invoke<void>('settings_set', { key, value });
}

export async function settingsGetAll(): Promise<{ key: string; value: string; updated_at: string }[]> {
  return invoke('settings_get_all');
}

export async function settingsGetDataDir(): Promise<string> {
  return invoke<string>('settings_get_data_dir');
}
export async function settingsSetDataDir(path: string): Promise<void> {
  return invoke<void>('settings_set_data_dir', { path });
}

export async function settingsValidateLlm(
  provider: string,
  apiKey: string,
  baseUrl: string,
  model: string,
  proxy?: string,
): Promise<boolean> {
  return invoke<boolean>('settings_validate_llm', {
    provider,
    apiKey,
    baseUrl,
    model,
    proxy,
  });
}

/** List the models an API key can currently use (OpenAI-compatible GET
 *  {baseUrl}/models). Onboarding offers this live catalog for model choice. */
export async function settingsListModels(
  apiKey: string,
  baseUrl: string,
  proxy?: string,
): Promise<string[]> {
  return invoke<string[]>('settings_list_models', { apiKey, baseUrl, proxy });
}

// ============================================================
// Translation
// ============================================================

export async function translateText(
  text: string,
  sourceLang?: string | null,
  targetLang?: string | null,
): Promise<string> {
  return invoke<string>('translate_text', { text, sourceLang, targetLang });
}

/** Streaming translation event emitted by the `translate_text_stream` command. */
export interface TranslationStreamEvent {
  request_id: string;
  // 'reasoning' carries the model's thinking while it works on the
  // translation — not displayable output, but proof the stream is alive.
  type: 'delta' | 'reasoning' | 'done' | 'error';
  content?: string;
}

/**
 * Stream a translation. Deltas are delivered via `onDelta` as they arrive;
 * the promise resolves with the full translation (returned by the backend
 * command itself, so correctness does not depend on event delivery order)
 * and rejects if the backend reports an error or times out.
 */
export async function translateTextStream(
  text: string,
  sourceLang?: string | null,
  targetLang?: string | null,
  onDelta?: (delta: string) => void,
): Promise<string> {
  const requestId = `${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 10)}`;

  return new Promise<string>((resolve, reject) => {
    let settled = false;
    let unlisten: UnlistenFn | null = null;
    const finish = (action: () => void) => {
      if (settled) return;
      settled = true;
      clearTimeout(idleTimer);
      clearTimeout(absoluteTimer);
      unlisten?.();
      action();
    };
    // Idle guard, not a total cap: reasoning models (qwen3.8-27b) can think
    // for minutes before the first token and long translations stream for
    // minutes more — the old fixed 120s wall clock killed those healthy
    // streams mid-work. Only 120s of complete silence (no delta AND no
    // reasoning) means the stream is hung. A 10-minute absolute backstop
    // caps a stream that drips forever without finishing.
    const IDLE_TIMEOUT_MS = 120_000;
    const ABSOLUTE_TIMEOUT_MS = 600_000;
    let idleTimer: ReturnType<typeof setTimeout>;
    const resetIdle = () => {
      clearTimeout(idleTimer);
      idleTimer = setTimeout(
        () => finish(() => reject(new Error('翻译超时（120 秒无响应）'))),
        IDLE_TIMEOUT_MS,
      );
    };
    const absoluteTimer = setTimeout(
      () => finish(() => reject(new Error('翻译超时'))),
      ABSOLUTE_TIMEOUT_MS,
    );
    resetIdle();

    listen<TranslationStreamEvent>('translation:event', (event) => {
      const e = event.payload;
      if (e.request_id !== requestId) return;
      // Any event from this stream — including reasoning — is activity.
      resetIdle();
      // Deltas drive the live display; done/error are informational because
      // resolution comes from the invoke result below.
      if (e.type === 'delta' && e.content) onDelta?.(e.content);
    })
      .then((fn) => {
        unlisten = fn;
        return invoke<string>('translate_text_stream', { text, sourceLang, targetLang, requestId });
      })
      .then((full) => {
        finish(() => resolve(full));
      })
      .catch((err) => {
        finish(() => reject(err instanceof Error ? err : new Error(String(err))));
      });
  });
}

export async function translationClearCache(model?: string): Promise<number> {
  return invoke<number>('translation_clear_cache', { model });
}

// ── Region detection ──

import type { LlmRegionRequest, DetectedRegion } from '@/components/reader/regions';

/** Detect structural regions on a PDF page using LLM layout analysis. */
export async function detectRegionsLlm(request: LlmRegionRequest): Promise<DetectedRegion[]> {
  return invoke<DetectedRegion[]>('detect_regions_llm', { request });
}

// ============================================================
// Knowledge
// ============================================================

export async function knowledgeListDomains(): Promise<KnowledgeDomain[]> {
  return invoke<KnowledgeDomain[]>('knowledge_list_domains');
}

export async function knowledgeCreateDomain(
  name: string, domainType: string, icon?: string, color?: string,
): Promise<KnowledgeDomain> {
  return invoke<KnowledgeDomain>('knowledge_create_domain', { name, domainType, icon, color });
}
export async function knowledgeUpdateDomain(
  id: string, name?: string, icon?: string, color?: string, sortOrder?: number,
): Promise<void> {
  return invoke<void>('knowledge_update_domain', { id, name, icon, color, sortOrder });
}
export async function knowledgeDeleteDomain(id: string): Promise<void> {
  return invoke<void>('knowledge_delete_domain', { id });
}

export async function knowledgeCreateItem(
  domainId: string, title: string, content?: string,
  contentType?: string, sourceType?: string, sourceId?: string, tags?: string[],
): Promise<KnowledgeItem> {
  return invoke<KnowledgeItem>('knowledge_create_item', { domainId, title, content, contentType, sourceType, sourceId, tags });
}
export async function knowledgeUpdateItem(
  id: string, title?: string, content?: string, domainId?: string, tags?: string[],
): Promise<KnowledgeItem> {
  return invoke<KnowledgeItem>('knowledge_update_item', { id, title, content, domainId, tags });
}
export async function knowledgeListItems(
  domainId?: string, search?: string, tag?: string, limit?: number, offset?: number,
): Promise<KnowledgeItem[]> {
  return invoke<KnowledgeItem[]>('knowledge_list_items', { domainId, search, tag, limit, offset });
}
export async function knowledgeGetItem(id: string): Promise<KnowledgeItem> {
  return invoke<KnowledgeItem>('knowledge_get_item', { id });
}
export async function knowledgeDeleteItem(id: string): Promise<void> {
  return invoke<void>('knowledge_delete_item', { id });
}

// ============================================================
// Research
// ============================================================

export async function researchCreateTopic(
  name: string,
  description?: string,
  keywords?: string[],
): Promise<ResearchTopic> {
  return invoke<ResearchTopic>('research_create_topic', { name, description, keywords });
}

export async function researchListTopics(): Promise<ResearchTopic[]> {
  return invoke<ResearchTopic[]>('research_list_topics');
}

export async function researchUpdateTopic(
  topicId: string,
  status?: string,
  description?: string,
  name?: string,
  keywords?: string[],
): Promise<void> {
  return invoke<void>('research_update_topic', { topicId, status, description, name, keywords });
}

/** AI-assisted topic drafting: natural-language blurb → proposed
 * { name, keywords, description } (nothing is persisted — the user confirms). */
export async function researchGenerateTopic(
  blurb: string,
): Promise<{ name: string; keywords: string[]; description: string }> {
  return invoke('research_generate_topic', { blurb });
}

export async function researchDiscoverSources(
  topicId: string,
  maxResults?: number,
): Promise<ResearchSource[]> {
  return invoke<ResearchSource[]>('research_discover_sources', { topicId, maxResults });
}

export async function researchListSources(
  topicId: string,
  status?: string,
  limit?: number,
  offset?: number,
): Promise<ResearchSource[]> {
  return invoke<ResearchSource[]>('research_list_sources', { topicId, status, limit, offset });
}

export async function researchImportSource(sourceId: string): Promise<{ paper_id: string; title: string; source_id: string; status: string }> {
  return invoke('research_import_source', { sourceId });
}

export async function researchDeleteTopic(topicId: string): Promise<void> {
  return invoke<void>('research_delete_topic', { topicId });
}

export async function researchUpdateSourceStatus(sourceId: string, status: string): Promise<void> {
  return invoke<void>('research_update_source_status', { sourceId, status });
}

/** Per-topic count of sources still in 'discovered' status (unread). */
export async function researchUnreadCounts(): Promise<Record<string, number>> {
  return invoke<Record<string, number>>('research_unread_counts');
}

// ============================================================
// Search
// ============================================================

export async function searchHybrid(query: string, limit?: number): Promise<unknown[]> {
  return invoke('search_hybrid', { query, limit });
}
export async function searchGenerateEmbeddings(paperId: string): Promise<number> {
  return invoke<number>('search_generate_embeddings', { paperId });
}
export interface EmbeddingModelCount {
  model: string;
  chunks: number;
  dimensions: number;
}
export interface EmbeddingStatus {
  /** 语义搜索是否开启：填了服务地址就是开启。 */
  leg_enabled: boolean;
  model: string;
  base_url: string;
  total_chunks: number;
  embedded_chunks: number;
  dimensions: number | null;
  /** 早期占位向量，永远不参与检索。 */
  placeholder_chunks: number;
  other_models: EmbeddingModelCount[];
}
export interface EmbeddingProbe {
  ok: boolean;
  model: string;
  base_url: string;
  dimensions: number | null;
  latency_ms: number;
  error: string | null;
  /** 服务通过 GET /v1/models 声明的模型名，可用来自动填写。 */
  detected_model: string | null;
  /** 未能自动获取模型名的原因（例如服务没有 /v1/models）。 */
  detect_error: string | null;
}
export async function searchEmbeddingStatus(): Promise<EmbeddingStatus> {
  return invoke<EmbeddingStatus>('search_embedding_status');
}
export async function searchTestEmbeddingEndpoint(
  baseUrl: string,
  model: string,
  apiKey: string,
): Promise<EmbeddingProbe> {
  return invoke<EmbeddingProbe>('search_test_embedding_endpoint', { baseUrl, model, apiKey });
}
export async function searchRagQuery(query: string, topK?: number): Promise<string> {
  return invoke<string>('search_rag_query', { query, topK });
}

// ============================================================
// Notes
// ============================================================

export async function notesCreate(title: string, content: string, paperId?: string, parentId?: string, isFolder?: boolean): Promise<Note> {
  const note = await invoke<Note>('notes_create', { title, content, paperId, parentId, isFolder });
  // New notes always open in edit mode regardless of the global default view
  // mode — the intent of creating a note is writing. Set here (the single
  // choke point) so every creation path (notes page, Ctrl+N, quick capture)
  // gets the behaviour.
  if (!note.is_folder) {
    const { useNoteEditorStore } = await import('@/stores/noteEditorStore');
    useNoteEditorStore.getState().setState(note.id, { mode: 'edit' });
  }
  return note;
}
export async function notesGet(id: string): Promise<Note> {
  return invoke<Note>('notes_get', { id });
}
export async function notesDuplicate(id: string): Promise<Note> {
  return invoke<Note>('note_duplicate', { id });
}
export async function notesUpdate(id: string, title?: string, content?: string, paperId?: string, aliases?: string, isFavorite?: number, touch?: boolean): Promise<Note> {
  return invoke<Note>('notes_update', { id, title, content, paperId, aliases, isFavorite, touch });
}
export async function notesDelete(id: string): Promise<void> {
  return invoke<void>('notes_delete', { id });
}
export async function notesList(paperId?: string, search?: string, parentId?: string, unlockedIds?: string[]): Promise<Note[]> {
  return invoke<Note[]>('notes_list', { paperId, search, parentId, unlockedIds });
}
export async function notesListAll(): Promise<Note[]> {
  return invoke<Note[]>('notes_list_all');
}
export async function notesMove(id: string, parentId?: string | null, sortOrder?: number): Promise<Note> {
  return invoke<Note>('notes_move', { id, parentId, sortOrder });
}
export async function notesGetBacklinks(noteId: string, unlockedIds?: string[]): Promise<{ id: string; title: string; context: string; created_at: string }[]> {
  return invoke('notes_get_backlinks', { noteId, unlockedIds });
}

export interface NoteSearchResult {
  id: string;
  title: string;
  snippet: string;
  updated_at: string;
}

/** Full-text search across all notes (ranked, with snippets). */
export async function notesSearch(query: string, limit?: number, unlockedIds?: string[]): Promise<NoteSearchResult[]> {
  return invoke<NoteSearchResult[]>('notes_search', { query, limit, unlockedIds });
}

// ============================================================
// Note view lock (privacy lock — view-only, session-scoped unlock)
// ============================================================

export async function vaultLockStatus(): Promise<{ has_password: boolean }> {
  return invoke('vault_lock_status');
}
export async function vaultSetLockPassword(password: string): Promise<void> {
  return invoke('vault_set_lock_password', { password });
}
export async function vaultVerifyLockPassword(password: string): Promise<boolean> {
  return invoke('vault_verify_lock_password', { password });
}
export async function notesSetLocked(id: string, locked: boolean): Promise<Note> {
  return invoke<Note>('notes_set_locked', { id, locked });
}

// ============================================================
// Note versions (AI-edit snapshots)
// ============================================================

export async function noteVersionsList(noteId: string): Promise<NoteVersion[]> {
  return invoke<NoteVersion[]>('note_versions_list', { noteId });
}
export async function noteVersionRestore(versionId: string): Promise<Note> {
  return invoke<Note>('note_version_restore', { versionId });
}
/** Create a note under the paper's collection folder tree (Zotero-style). */
/** Save an agent reply as a note. The reply's `[^n]` markers + ```evidence
 *  block are converted server-side into inline `[n]` links plus a
 *  `## 参考文献` list — the same citation form AI 综述 notes use. */
export async function noteCreateFromReply(
  paperId: string | null,
  title: string,
  content: string
): Promise<Note> {
  return invoke<Note>('note_create_from_reply', { paperId, title, content });
}

export async function noteCreateUnderPaper(paperId: string, title: string, content: string): Promise<Note> {
  return invoke<Note>('note_create_under_paper', { paperId, title, content });
}
/** Append an excerpt to the paper's excerpt note (titled with the paper's title). */
export async function noteAddExcerpt(paperId: string, content: string): Promise<Note> {
  return invoke<Note>('note_add_excerpt', { paperId, content });
}
/** Merge a standalone note into the paper's excerpt note, then delete it. */
export async function noteMergeIntoExcerpt(noteId: string, paperId: string): Promise<Note> {
  return invoke<Note>('note_merge_into_excerpt', { noteId, paperId });
}

// ============================================================
// Vaults (Obsidian-style note vaults)
// ============================================================

export async function vaultList(): Promise<Vault[]> {
  return invoke<Vault[]>('vault_list');
}
export async function vaultCurrent(): Promise<Vault> {
  return invoke<Vault>('vault_current');
}
export async function vaultCreate(name: string): Promise<Vault> {
  return invoke<Vault>('vault_create', { name });
}
export async function vaultRename(id: string, name: string): Promise<Vault> {
  return invoke<Vault>('vault_rename', { id, name });
}
export async function vaultDelete(id: string): Promise<void> {
  return invoke<void>('vault_delete', { id });
}
export async function vaultSetCurrent(id: string): Promise<Vault> {
  return invoke<Vault>('vault_set_current', { id });
}
export async function vaultExport(id: string, targetDir: string): Promise<number> {
  return invoke<number>('vault_export', { id, targetDir });
}
export async function vaultImport(id: string, sourceDir: string): Promise<{ imported: number; files_imported: number; unchanged: number; skipped: number }> {
  return invoke<{ imported: number; files_imported: number; unchanged: number; skipped: number }>('vault_import', { id, sourceDir });
}

// ============================================================
// Vault files (managed files in the note tree)
// ============================================================

export async function filesList(vaultId: string): Promise<FileItem[]> {
  return invoke<FileItem[]>('files_list', { vaultId });
}
/** Import a local file into the vault (copies content into the blob store). */
export async function filesImport(vaultId: string, sourcePath: string, parentId?: string | null): Promise<FileItem> {
  return invoke<FileItem>('files_import', { vaultId, sourcePath, parentId });
}
export async function filesMove(id: string, parentId?: string | null, sortOrder?: number): Promise<FileItem> {
  return invoke<FileItem>('files_move', { id, parentId, sortOrder });
}
export async function filesRename(id: string, name: string): Promise<FileItem> {
  return invoke<FileItem>('files_rename', { id, name });
}
export async function filesDelete(id: string): Promise<void> {
  return invoke<void>('files_delete', { id });
}
/** Import a vault-managed file (e.g. a PDF in the notes file list) into the
 *  paper library. Idempotent: returns the existing paper for the same blob. */
export async function filesImportToLibrary(id: string): Promise<Paper> {
  return invoke<Paper>('import_file_to_library', { fileId: id });
}
/** Open a managed file with the system default application. */
export async function filesOpen(id: string): Promise<void> {
  return invoke<void>('files_open', { id });
}
/** Get a managed file record. */
export async function filesGet(id: string): Promise<FileItem> {
  return invoke<FileItem>('files_get', { id });
}
/** Resolve a managed file to an asset-protocol URL for in-app preview
 *  (built-in PDF/image viewer). */
export async function filesResolveUrl(id: string): Promise<string> {
  const path = await invoke<string>('files_resolve_path', { id });
  return convertFileSrc(path);
}
/** Preview a managed file as text (content-sniffed; rejects binary files). */
export async function filesReadText(id: string): Promise<TextPreview> {
  return invoke<TextPreview>('files_read_text', { id });
}

// ============================================================
// File Browser
// ============================================================

export async function fileBrowserListDir(path: string, showHidden?: boolean): Promise<FileEntry[]> {
  return invoke<FileEntry[]>('file_browser_list_dir', { path, showHidden });
}
export async function fileBrowserGetInfo(path: string): Promise<FileEntry> {
  return invoke<FileEntry>('file_browser_get_info', { path });
}
export async function fileBrowserOpenInSystem(path: string): Promise<void> {
  return invoke<void>('file_browser_open_in_system', { path });
}
/** Open a paper's PDF with the system default app (resolves blob path server-side). */
export async function openPaperInSystem(paperId: string): Promise<void> {
  return invoke<void>('open_paper_in_system', { id: paperId });
}
/** Reveal a paper's PDF in the system file manager (resolves blob path server-side). */
export async function revealPaperInSystem(paperId: string): Promise<void> {
  return invoke<void>('reveal_paper_in_system', { id: paperId });
}

export interface DuplicateCandidate {
  id: string;
  title: string;
  year?: number | null;
  journal?: string | null;
  doi?: string | null;
  match_reason: 'doi' | 'title';
}
/** Find papers likely duplicating the given paper (DOI / normalized title). */
export async function paperFindDuplicates(paperId: string): Promise<DuplicateCandidate[]> {
  return invoke<DuplicateCandidate[]>('paper_find_duplicates', { id: paperId });
}
/** Merge removeId into keepId (metadata fill + children transfer + delete). */
export async function paperMerge(keepId: string, removeId: string): Promise<void> {
  return invoke<void>('paper_merge', { keepId, removeId });
}
/** Restore a trashed paper. */
export async function paperRestore(paperId: string): Promise<void> {
  return invoke<void>('paper_restore', { id: paperId });
}
/** Permanently delete a trashed paper and its unreferenced files. */
export async function paperPurge(paperId: string): Promise<void> {
  return invoke<void>('paper_purge', { id: paperId });
}
/** Export papers as citation text: format = 'bibtex' | 'ris' | 'csl-json'. */
export async function paperExport(ids: string[], format: 'bibtex' | 'ris' | 'csl-json'): Promise<string> {
  return invoke<string>('paper_export', { ids, format });
}
/** Set the favorite flag of a paper. */
export async function paperSetFavorite(id: string, favorite: boolean): Promise<void> {
  return invoke<void>('paper_set_favorite', { id, favorite });
}
/** Set the read status: 'unread' | 'read' | 'in_progress'. */
export async function paperSetReadStatus(id: string, status: string): Promise<void> {
  return invoke<void>('paper_set_read_status', { id, status });
}
/** Record that a paper was opened in the reader. */
export async function paperRecordRead(id: string): Promise<void> {
  return invoke<void>('paper_record_read', { id });
}
/** Link two papers bidirectionally. */
export async function paperAddRelated(paperId: string, relatedId: string): Promise<void> {
  return invoke<void>('paper_add_related', { paperId, relatedId });
}
/** Remove the link between two papers. */
export async function paperRemoveRelated(paperId: string, relatedId: string): Promise<void> {
  return invoke<void>('paper_remove_related', { paperId, relatedId });
}
/** List papers related to the given paper. */
export async function paperListRelated(paperId: string): Promise<Paper[]> {
  return invoke<Paper[]>('paper_list_related', { id: paperId });
}

export interface SavedSearch {
  id: string;
  name: string;
  params_json: string;
  created_at: string;
}
export async function savedSearchesList(): Promise<SavedSearch[]> {
  return invoke<SavedSearch[]>('saved_searches_list');
}
export async function savedSearchesCreate(name: string, paramsJson: string): Promise<SavedSearch> {
  return invoke<SavedSearch>('saved_searches_create', { name, paramsJson });
}
export async function savedSearchesDelete(id: string): Promise<void> {
  return invoke<void>('saved_searches_delete', { id });
}

export interface Creator {
  role: string;
  last_name: string;
  first_name: string;
  name: string;
}
/** Get structured creators for a paper. */
export async function paperGetCreators(paperId: string): Promise<Creator[]> {
  return invoke<Creator[]>('paper_get_creators', { id: paperId });
}
/** Replace structured creators (also regenerates the sync authors/editor columns). */
export async function paperSetCreators(paperId: string, creators: Creator[]): Promise<void> {
  return invoke<void>('paper_set_creators', { id: paperId, creators });
}

export interface AttachmentInfo {
  id: string;
  paper_id: string;
  file_name: string;
  file_path: string;
  file_type: string;
  created_at: string;
}
export async function paperListAttachments(paperId: string): Promise<AttachmentInfo[]> {
  return invoke<AttachmentInfo[]>('paper_list_attachments', { id: paperId });
}
export async function paperAddAttachment(paperId: string, sourcePath: string): Promise<AttachmentInfo> {
  return invoke<AttachmentInfo>('paper_add_attachment', { paperId, sourcePath });
}
export async function paperRemoveAttachment(attachmentId: string): Promise<void> {
  return invoke<void>('paper_remove_attachment', { id: attachmentId });
}
export async function paperOpenAttachment(attachmentId: string): Promise<void> {
  return invoke<void>('paper_open_attachment', { id: attachmentId });
}
export async function paperExportAnnotations(paperId: string): Promise<string> {
  return invoke<string>('paper_export_annotations', { id: paperId });
}
export async function fileBrowserRevealInSystem(path: string): Promise<void> {
  return invoke<void>('file_browser_reveal_in_system', { path });
}

/** Windows-only: show the system "Open with…" dialog for a file. */
export async function fileBrowserOpenWith(path: string): Promise<void> {
  return invoke<void>('file_browser_open_with', { path });
}

/** Read a text file's content (bounded) — used by the chat attach feature. */
export async function readTextFile(path: string): Promise<string> {
  return invoke<string>('read_text_file', { path });
}

/** Read any document (PDF/office/text, any extension/encoding) as text for
 *  chat context. Oversized extractions are cached under cacheDir (session
 *  working dir when set) and the returned text carries a continuation
 *  pointer the agent can follow with file_read. */
export async function readDocumentFile(path: string, cacheDir?: string): Promise<string> {
  return invoke<string>('read_document_file', { path, cacheDir });
}

/** Write text content to a file at an absolute path (export flows). */
export async function saveTextFile(path: string, content: string): Promise<void> {
  return invoke<void>('save_text_file', { path, content });
}

// ============================================================
// Attachments
// ============================================================

export interface ClipboardImageInput {
  rgba: number[];
  width: number;
  height: number;
  vaultId: string;
}

export async function saveClipboardImage(input: ClipboardImageInput): Promise<string> {
  return invoke<string>('save_clipboard_image', {
    input: {
      rgba: input.rgba,
      width: input.width,
      height: input.height,
      vault_id: input.vaultId,
    },
  });
}

export async function vaultAttachmentsDir(vaultId: string): Promise<string> {
  return invoke<string>('vault_attachments_dir', { vaultId });
}

export interface SaveAttachmentBytesInput {
  bytes: number[];
  filename: string;
  vaultId: string;
}

export async function saveAttachmentBytes(input: SaveAttachmentBytesInput): Promise<string> {
  return invoke<string>('save_attachment_bytes', {
    input: {
      bytes: input.bytes,
      filename: input.filename,
      vault_id: input.vaultId,
    },
  });
}

/** Read a local image file and return it as a base64-encoded attachment. */
export async function readImageFile(path: string): Promise<ChatAttachment> {
  return invoke<ChatAttachment>('read_image_file', { path });
}

// ============================================================
// Annotations (snippets)
// ============================================================

export interface AnnotationRow {
  id: string; paper_id: string; page: number;
  rect: string;        // JSON {x,y,w,h}
  text: string | null; note: string | null;
  tags: string;        // JSON string[]
  translation: string | null;
  created_at: string; updated_at: string;
}

export async function annotationList(paperId: string): Promise<AnnotationRow[]> {
  return invoke<AnnotationRow[]>('annotation_list', { paperId });
}
/** One segment of a multi-range (discontinuous) text selection. */
export interface AnnotationSegmentInput {
  page: number; // 1-based PDF page number
  rects: { x: number; y: number; w: number; h: number }[];
  text?: string | null;
  quote?: { prefix: string; exact: string; suffix: string } | null;
}

export async function annotationCreate(
  paperId: string, page: number,
  xRatio: number, yRatio: number, widthRatio: number, heightRatio: number,
  text: string | null, note: string | null, tags: string[],
  id?: string,
  rects?: { x: number; y: number; w: number; h: number }[] | null,
  segments?: AnnotationSegmentInput[] | null,
): Promise<AnnotationRow> {
  return invoke<AnnotationRow>('annotation_create', {
    paperId, page, xRatio, yRatio, widthRatio, heightRatio, text, note, tags,
    id: id ?? null,
    rects: rects && rects.length > 0 ? JSON.stringify(rects) : null,
    segments: segments && segments.length > 0 ? JSON.stringify(segments) : null,
  });
}
export async function annotationUpdateNote(id: string, note: string): Promise<AnnotationRow> {
  return invoke<AnnotationRow>('annotation_update_note', { id, note });
}
export async function annotationUpdateTags(id: string, tags: string[]): Promise<AnnotationRow> {
  return invoke<AnnotationRow>('annotation_update_tags', { id, tags });
}
export async function annotationUpdateTranslation(id: string, translation: string): Promise<AnnotationRow> {
  return invoke<AnnotationRow>('annotation_update_translation', { id, translation });
}
export async function annotationDelete(id: string): Promise<void> {
  return invoke<void>('annotation_delete', { id });
}
export async function annotationClearPaper(paperId: string): Promise<void> {
  return invoke<void>('annotation_clear_paper', { paperId });
}

// ============================================================
// System
// ============================================================

/** Export a diagnostics bundle (logs + environment report zip) to `destPath`. */
export async function exportDiagnostics(destPath: string): Promise<string> {
  return invoke<string>('export_diagnostics', { destPath });
}

export async function systemInfo(): Promise<SystemInfo> {
  return invoke<SystemInfo>('system_info');
}

// ============================================================
// Graph
// ============================================================

export interface GraphNode {
  id: string;
  label: string;
  node_type: string;
  color: string;
}

export interface GraphEdge {
  source: string;
  target: string;
  edge_type: string;
}

export interface GraphData {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export async function graphGet(): Promise<GraphData> {
  return invoke<GraphData>('graph_get');
}

export async function graphGetLocal(noteId: string, depth?: number): Promise<GraphData> {
  return invoke<GraphData>('graph_get_local', { noteId, depth });
}

// ============================================================
// Sync
// ============================================================

export async function getDeviceId(): Promise<string> {
  return invoke<string>('get_device_id');
}

/** Account auto-sync proxy: discover other account devices and sync automatically. */
export async function startAutoSync(input: {
  relay_url: string;
  token: string;
}): Promise<void> {
  // Tauri maps direct command args to camelCase: relay_url → relayUrl.
  return invoke<void>('start_auto_sync', { relayUrl: input.relay_url, token: input.token });
}

/** Best-effort local LAN IPv4 of this machine. */
export async function getLocalIp(): Promise<string> {
  return invoke<string>('get_local_ip');
}

/** LAN-local host: bind UDP signaling port and accept local guests.
 *  `pairingCode` is the code being broadcast; guest offers with a different
 *  code are rejected by the host (protocol-level pairing check). */
export async function startLocalHost(pairingCode: string): Promise<string> {
  return invoke<string>('start_local_host', { pairingCode });
}

/** Stop the LAN-local host loop (keeps account auto-sync running). */
export async function stopLocalHost(): Promise<void> {
  return invoke<void>('stop_local_host');
}

/** Whether the LAN-local host loop is currently accepting guests. */
export async function getLanHostActive(): Promise<boolean> {
  return invoke<boolean>('get_lan_host_active');
}

/** LAN-local guest: connect to the host's UDP signaling port directly.
 *  `hostDeviceId` (from LAN discovery) is used to persist per-host sync
 *  progress; `pairingCode` is the host's code (from the beacon or typed
 *  manually) — the host rejects a mismatching code. */
export async function connectLocalHost(
  hostIp: string,
  hostDeviceId?: string,
  pairingCode?: string,
): Promise<string> {
  return invoke<string>('connect_local_host', { hostIp, hostDeviceId, pairingCode });
}

/** Trigger a one-shot sync for `kind` ("lan" / "cloud" / undefined = most
 *  recent session). Falls back to the encrypted mailbox when the peer is
 *  offline. */
export async function syncOnce(kind?: 'lan' | 'cloud'): Promise<string> {
  return invoke<string>('sync_once', { kind });
}

/** Retry delivering queued outbox messages. */
export async function flushSyncOutbox(): Promise<string> {
  return invoke<string>('flush_sync_outbox');
}

/** Number of messages waiting in the local outbox. */
export async function getSyncOutboxCount(): Promise<number> {
  return invoke<number>('get_sync_outbox_count');
}

export async function exportEncryptedSeed(input: {
  archive_path: string;
  password: string;
}): Promise<string> {
  return invoke<string>('export_encrypted_seed', { input });
}

export async function importEncryptedSeed(input: {
  archive_path: string;
  password: string;
}): Promise<string> {
  return invoke<string>('import_encrypted_seed', { input });
}

export async function startLanBeacon(input: {
  device_id: string;
  pairing_payload: string;
}): Promise<void> {
  return invoke<void>('start_lan_beacon', { input });
}

export async function stopLanBeacon(): Promise<void> {
  return invoke<void>('stop_lan_beacon');
}

export async function startLanDiscovery(): Promise<void> {
  return invoke<void>('start_lan_discovery');
}

export async function stopLanDiscovery(): Promise<void> {
  return invoke<void>('stop_lan_discovery');
}

export interface LanPeerInfo {
  device_id: string;
  pairing_payload: string;
  addr: string;
}

export async function getLanPeers(): Promise<LanPeerInfo[]> {
  return invoke<LanPeerInfo[]>('get_lan_peers');
}

export interface SyncStatus {
  connected: boolean;
  peer_device_id?: string;
  last_sync_at?: string;
  last_error?: string;
  transport?: 'p2p' | 'mailbox' | 'none';
  outbox_pending?: number;
  pushed?: number;
  pulled?: number;
  kind?: 'lan' | 'cloud' | 'unknown';
  /** 本设备的 discovery relay 连接是否存活（进程级，由 auto-sync proxy 维护）。 */
  relay_connected?: boolean;
}

/** Status of the sync session for `kind` (undefined = most recent session).
 *  LAN and cloud sessions are independent, so each tab requests its own. */
export async function getSyncStatus(kind?: 'lan' | 'cloud'): Promise<SyncStatus> {
  return invoke<SyncStatus>('get_sync_status', { kind });
}

/** Disconnect only the LAN-local sync session (keeps cloud auto-sync). */
export async function stopLocalSession(): Promise<void> {
  return invoke<void>('stop_local_session');
}

/** Disconnect only the cloud (account) sync session (keeps LAN host loop). */
export async function stopCloudSession(): Promise<void> {
  return invoke<void>('stop_cloud_session');
}

export interface SyncConfig {
  sync_optional_data: boolean;
  allow_plaintext_relay: boolean;
}

export async function getSyncConfig(): Promise<SyncConfig> {
  return invoke<SyncConfig>('get_sync_config');
}

// ============================================================
// Account (production sync)
// ============================================================

export interface AuthInfo {
  access_token: string;
  user_id: string;
  email: string;
  device_id: string;
}

export interface AccountDeviceRow {
  device_id: string;
  name: string;
  online?: boolean;
}

export async function authRegister(relayUrl: string, email: string, password: string): Promise<void> {
  return invoke<void>('auth_register', { relayUrl, email, password });
}

export async function authLogin(relayUrl: string, email: string, password: string, deviceName: string): Promise<AuthInfo> {
  return invoke<AuthInfo>('auth_login', { relayUrl, email, password, deviceName });
}

export async function authLogout(): Promise<void> {
  return invoke<void>('auth_logout');
}

export async function authStatus(): Promise<AuthInfo> {
  return invoke<AuthInfo>('auth_status');
}

export async function suggestDeviceName(): Promise<string> {
  return invoke<string>('suggest_device_name');
}

export async function deviceList(relayUrl: string): Promise<AccountDeviceRow[]> {
  return invoke<AccountDeviceRow[]>('device_list', { relayUrl });
}

export async function deviceRemove(relayUrl: string, deviceId: string): Promise<void> {
  return invoke<void>('device_remove', { relayUrl, deviceId });
}

export async function deviceRename(relayUrl: string, deviceId: string, name: string): Promise<void> {
  return invoke<void>('device_rename', { relayUrl, deviceId, name });
}

// ============================================================
// Cloud storage quota & subscription plans
// ============================================================

export interface StorageStatus {
  used_bytes: number;
  quota_bytes: number;
  plan_id: string | null;
  /** RFC3339 到期时间；null = 永久（管理员直调配额） */
  expires_at: string | null;
}

export interface StoragePlan {
  id: string;
  name: string;
  quota_bytes: number;
  monthly_cny: number;
  yearly_cny: number;
}

export type StorageOrderStatus = 'pending' | 'paid' | 'rejected' | 'cancelled';

export interface StorageOrder {
  id: string;
  user_id?: string;
  plan_id: string;
  quota_bytes?: number;
  duration_days?: number;
  amount_cny: number;
  status: StorageOrderStatus;
  created_at: string;
  paid_at?: string | null;
  admin_note?: string | null;
  /** 收款说明：仅 pending 订单由 relay 下发，其他状态为空。 */
  payment_info?: string | null;
  /** 在线支付页 URL：pending 且选了支付宝渠道时由 relay 下发。 */
  pay_url?: string | null;
}

export interface StorageOrderCreateResult {
  order_id: string;
  amount_cny: number;
  payment_info: string;
  /** 在线支付页 URL（relay 已配置支付宝时）；无则走线下转账说明。 */
  pay_url?: string | null;
}

export async function storageStatus(relayUrl: string): Promise<StorageStatus> {
  return invoke<StorageStatus>('storage_status', { relayUrl });
}

export async function storagePlans(relayUrl: string): Promise<StoragePlan[]> {
  return invoke<StoragePlan[]>('storage_plans', { relayUrl });
}

export async function storageOrderCreate(
  relayUrl: string,
  planId: string,
  period: 'month' | 'year',
  channel?: 'alipay'
): Promise<StorageOrderCreateResult> {
  return invoke<StorageOrderCreateResult>('storage_order_create', { relayUrl, planId, period, channel });
}

export async function storageOrderList(relayUrl: string): Promise<StorageOrder[]> {
  return invoke<StorageOrder[]>('storage_order_list', { relayUrl });
}

/** 查询单个订单状态（等待支付结果时轮询）。 */
export async function storageOrderStatus(relayUrl: string, orderId: string): Promise<StorageOrder> {
  return invoke<StorageOrder>('storage_order_status', { relayUrl, orderId });
}

export async function setSyncConfig(config: SyncConfig): Promise<void> {
  return invoke<void>('set_sync_config', { config });
}

// ============================================================
// Timeline
// ============================================================

export async function timelineList(
  limit?: number,
  offset?: number,
  module?: string,
): Promise<TimelineItem[]> {
  return invoke<TimelineItem[]>('timeline_list', { limit, offset, module });
}

// ============================================================
// AI 表格列
// ============================================================

export interface AiTableColumn {
  key: string;
  prompt: string;
}

export interface AiTableCell {
  paperId: string;
  columnKey: string;
  value: string;
  /** done | failed | edited | empty（未检索到） | absent（文中无此内容） */
  status: string;
  edited: boolean;
  /** Model-reported source location, e.g. "p.21 §5". */
  src: string;
  /** Model-reported confidence (0 when unreported). */
  conf: number;
  /** True when nothing solid backs this value (no evidence / no location / low
   *  confidence / failed cross-check). Shown, never silently trusted. */
  suspect: boolean;
  suspectReason: string;
  /** Cross-check verdict: '' | yes | partial | no. */
  verifyVerdict: string;
  verifyNote: string;
}

/** One retrieved passage behind a cell (the audit trail). */
export interface AiTableEvidenceRef {
  label: string;
  page: number | null;
  section: string;
  /** Column whose query surfaced it ('' for a neighbouring chunk). */
  fromColumn: string;
  snippet: string;
}

/** Search terms stored for a column. */
export interface AiTableColumnTerms {
  columnKey: string;
  terms: string[];
  /** slot | ai | ascii | manual ('' when nothing is stored yet). */
  source: string;
  updatedAt: string;
}

export interface AiTableProgress {
  total: number;
  done: number;
  failed: number;
  currentTitle: string;
  /** One process-panel log line (paper finished/failed/skipped). */
  log?: string;
  /** Reasoning delta of the paper being extracted (thinking models). */
  reasoning?: string;
  /** "done" | "cancelled" | "busy" once the run ends; null while running. */
  finished: string | null;
}

export interface AiTablePreset {
  id: string;
  name: string;
  columns: AiTableColumn[];
  updatedAt: string;
}

export interface AiTablePaperBrief {
  id: string;
  title: string;
  year: number | null;
}

/** How verbose each cell is. An explicit length/verbosity in the column prompt
 *  overrides it. */
export type AiTableDetail = 'brief' | 'normal' | 'rich';

/** How hard the run looks for evidence (cost ↔ accuracy, the user's choice).
 *  fast: 1 call/paper, 3 chunks/column, ≤6k chars
 *  standard: 1 call + one follow-up pass for weakly-evidenced fields
 *  deep: 8 chunks/column, section-aware coverage, ≤24k chars
 *  per_column: one call PER COLUMN (most accurate, most expensive) */
export type AiTableDepth = 'fast' | 'standard' | 'deep' | 'per_column';

/** Cross-check ("交叉验证"): re-read the passages behind a value and ask whether
 *  they support it. off: no extra calls; doubtful: only flagged cells (default);
 *  all: every filled cell. */
export type AiTableVerify = 'off' | 'doubtful' | 'all';

export async function aiTableExtractStart(
  paperIds: string[],
  columns: AiTableColumn[],
  providerId?: string | null,
  detail?: AiTableDetail,
  depth?: AiTableDepth,
  verify?: AiTableVerify,
  force?: boolean
): Promise<void> {
  return invoke<void>('ai_table_extract_start', {
    paperIds,
    columns,
    providerId: providerId ?? null,
    detail: detail ?? null,
    depth: depth ?? null,
    verify: verify ?? null,
    force: force ?? null,
  });
}

export async function aiTableExtractCancel(): Promise<void> {
  return invoke<void>('ai_table_extract_cancel');
}

export interface AiTableExtractStatus {
  running: boolean;
  progress: AiTableProgress | null;
}

/** Whether a run is still in flight, plus its last beat — lets a reopened
 *  dialog re-attach instead of showing a dead spinner. */
export async function aiTableExtractStatus(): Promise<AiTableExtractStatus> {
  return invoke<AiTableExtractStatus>('ai_table_extract_status');
}

export async function aiTableCellsList(
  paperIds: string[],
  columnKeys: string[]
): Promise<AiTableCell[]> {
  return invoke<AiTableCell[]>('ai_table_cells_list', { paperIds, columnKeys });
}

export async function aiTableCellSet(
  paperId: string,
  columnKey: string,
  value: string
): Promise<void> {
  return invoke<void>('ai_table_cell_set', { paperId, columnKey, value });
}

export async function aiTablePresetsList(): Promise<AiTablePreset[]> {
  return invoke<AiTablePreset[]>('ai_table_presets_list');
}

export async function aiTablePresetSave(
  id: string | null,
  name: string,
  columns: AiTableColumn[]
): Promise<AiTablePreset> {
  return invoke<AiTablePreset>('ai_table_preset_save', { id, name, columns });
}

export async function aiTablePresetDelete(id: string): Promise<void> {
  return invoke<void>('ai_table_preset_delete', { id });
}

export interface AiTableBuiltinPreset {
  id: string;
  name: string;
  description: string;
  columns: AiTableColumn[];
}

/** Read-only built-in column presets (picking one only fills the editor). */
export async function aiTableBuiltinPresets(): Promise<AiTableBuiltinPreset[]> {
  return invoke<AiTableBuiltinPreset[]>('ai_table_builtin_presets');
}

export async function aiTablePapersInfo(paperIds: string[]): Promise<AiTablePaperBrief[]> {
  return invoke<AiTablePaperBrief[]>('ai_table_papers_info', { paperIds });
}

/** Stored search terms for these columns (cache only — no model call). */
export async function aiTableTermsGet(columnKeys: string[]): Promise<AiTableColumnTerms[]> {
  return invoke<AiTableColumnTerms[]>('ai_table_terms_get', { columnKeys });
}

/** Hand-write a column's search terms; [] clears the override. */
export async function aiTableTermsSet(
  columnKey: string,
  terms: string[]
): Promise<AiTableColumnTerms[]> {
  return invoke<AiTableColumnTerms[]>('ai_table_terms_set', { columnKey, terms });
}

/** Recompute a column's search terms now (one model call when needed). */
export async function aiTableTermsRegenerate(
  columnKey: string,
  prompt: string
): Promise<string[]> {
  return invoke<string[]>('ai_table_terms_regenerate', { columnKey, prompt });
}

/** The passages that backed one cell. */
export async function aiTableCellEvidence(
  paperId: string,
  columnKey: string
): Promise<AiTableEvidenceRef[]> {
  return invoke<AiTableEvidenceRef[]>('ai_table_cell_evidence', { paperId, columnKey });
}

// ============================================================
// AI 文献分类
// ============================================================

/** One node in the (user-editable) taxonomy draft. Two levels max: a group's
 *  children are leaves. */
export interface ClassifyGroup {
  name: string;
  description: string;
  /** Existing collection/tag id matched by full path; null = applying creates one. */
  existingId: string | null;
  children: ClassifyGroup[];
}

/** accepted = will be written on apply; pending = needs review. */
export type ClassifyItemStatus = 'accepted' | 'pending' | 'failed';

export interface ClassifyItem {
  paperId: string;
  title: string;
  year: number | null;
  /** Primary path (first label); empty = 待定. */
  groupName: string;
  /** All assigned paths (single element for the collection target). */
  labels: string[];
  confidence: number;
  reason: string;
  status: string;
  /** Manual override; a rerun keeps it and it counts as accepted. */
  edited: boolean;
}

export interface ClassifyRun {
  id: string;
  dimension: string;
  target: string;
  scopeKind: string;
  scopeId: string | null;
  sampleSize: number | null;
  paperCount: number;
  threshold: number;
  twoLevel: boolean;
  skipExisting: boolean;
  groups: ClassifyGroup[];
  items: ClassifyItem[];
  status: string;
  appliedAt: string | null;
  undoneAt: string | null;
}

export interface ClassifyRequest {
  paperIds: string[];
  scopeKind: string;
  scopeId: string | null;
  dimension: string;
  target: string;
  sampleSize: number | null;
  reuseExisting: boolean;
  maxGroups: number;
  /** Ask the model for a second level (one extra call). */
  twoLevel: boolean;
  /** Assignments at or above this confidence are accepted automatically. */
  threshold: number;
  /** Skip papers a previous applied run already filed under this dimension. */
  skipExisting: boolean;
  /** Draft engine: 'batch' | 'semantic'. Semantic falls back to batch. */
  engine: 'batch' | 'semantic';
  providerId: string | null;
}

export interface ClassifyCapabilities {
  /** A real embeddings endpoint is configured, so clustering is possible. */
  semantic: boolean;
}

export interface ClassifyRule {
  id: string;
  name: string;
  dimension: string;
  target: string;
  scopeKind: string;
  scopeId: string | null;
  maxGroups: number;
  threshold: number;
  twoLevel: boolean;
  reuseExisting: boolean;
  cron: string;
  enabled: boolean;
  providerId: string | null;
  lastRunAt: string | null;
  lastResult: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface ClassifyRuleInput {
  name: string;
  dimension: string;
  target: string;
  scopeKind: string;
  scopeId: string | null;
  maxGroups: number;
  threshold: number;
  twoLevel: boolean;
  reuseExisting: boolean;
  cron: string;
  enabled: boolean;
  providerId: string | null;
}

export interface VocabEntry {
  id: string;
  /** `父/子` for a child. */
  path: string;
  /** active | staged | rejected */
  status: string;
  /** ai | user | library */
  source: string;
  createdAt: string;
}

export interface ClassifyProgress {
  /** draft | merge | assign | done | cancelled | error */
  stage: string;
  total: number;
  done: number;
  failed: number;
  current: string;
  groupCount: number;
  /** Category paths proposed so far (grows batch by batch during the draft). */
  groupsPreview?: string[];
  log?: string;
  /** Set when the run ends: done | cancelled | error. */
  finished: string | null;
}

export interface ClassifyPaperBrief {
  id: string;
  title: string;
  year: number | null;
  journal: string | null;
  abstractText: string;
}

export interface ApplyPlan {
  targets: { groupName: string; create: boolean }[];
  parentCollectionId: string | null;
}

export interface ApplySummary {
  createdTargets: number;
  reusedTargets: number;
  linkedPapers: number;
  skippedPapers: number;
}

export interface UndoSummary {
  removedLinks: number;
  removedTargets: number;
}

export const AI_CLASSIFY_PROGRESS_EVENT = 'library:ai-classify-progress';

export async function aiClassifyStart(req: ClassifyRequest): Promise<string> {
  return invoke<string>('ai_classify_start', { req });
}

export async function aiClassifyCancel(): Promise<void> {
  return invoke<void>('ai_classify_cancel');
}

export async function aiClassifyRunGet(runId: string): Promise<ClassifyRun> {
  return invoke<ClassifyRun>('ai_classify_run_get', { runId });
}

export async function aiClassifyGroupsSet(
  runId: string,
  groups: ClassifyGroup[]
): Promise<void> {
  return invoke<void>('ai_classify_groups_set', { runId, groups });
}

export async function aiClassifyItemSet(
  runId: string,
  paperId: string,
  labels: string[]
): Promise<void> {
  return invoke<void>('ai_classify_item_set', { runId, paperId, labels });
}

/** Bulk override. `edited: true` marks a real user edit (protected from later
 *  reruns and applied as accepted); omitting it is a pure sync that keeps the
 *  existing flag, confidence and reason. */
export async function aiClassifyItemsSet(
  runId: string,
  items: { paperId: string; labels: string[]; edited?: boolean }[]
): Promise<void> {
  return invoke<void>('ai_classify_items_set', { runId, items });
}

export async function aiClassifyCapabilities(): Promise<ClassifyCapabilities> {
  return invoke<ClassifyCapabilities>('ai_classify_capabilities');
}

export async function aiClassifyRulesList(): Promise<ClassifyRule[]> {
  return invoke<ClassifyRule[]>('ai_classify_rules_list');
}

export async function aiClassifyRuleSave(
  id: string | null,
  input: ClassifyRuleInput
): Promise<ClassifyRule> {
  return invoke<ClassifyRule>('ai_classify_rule_save', { id, input });
}

export async function aiClassifyRuleSetEnabled(id: string, enabled: boolean): Promise<void> {
  return invoke<void>('ai_classify_rule_set_enabled', { id, enabled });
}

export async function aiClassifyRuleDelete(id: string): Promise<void> {
  return invoke<void>('ai_classify_rule_delete', { id });
}

/** Fire a rule now (same path the scheduler uses). */
export async function aiClassifyRuleRun(id: string): Promise<void> {
  return invoke<void>('ai_classify_rule_run', { id });
}

export async function aiClassifyVocabList(
  dimension: string,
  target: string
): Promise<VocabEntry[]> {
  return invoke<VocabEntry[]>('ai_classify_vocab_list', { dimension, target });
}

/** Promote (`active`), un-promote (`staged`) or reject (`rejected`). */
export async function aiClassifyVocabSetStatus(
  ids: string[],
  status: 'active' | 'staged' | 'rejected'
): Promise<void> {
  return invoke<void>('ai_classify_vocab_set_status', { ids, status });
}

export async function aiClassifyReassign(
  runId: string,
  providerId: string | null,
  onlyMissing: boolean
): Promise<void> {
  return invoke<void>('ai_classify_reassign', { runId, providerId, onlyMissing });
}

export async function aiClassifyApply(runId: string, plan: ApplyPlan): Promise<ApplySummary> {
  return invoke<ApplySummary>('ai_classify_apply', { runId, plan });
}

export async function aiClassifyUndo(runId: string): Promise<UndoSummary> {
  return invoke<UndoSummary>('ai_classify_undo', { runId });
}

export async function aiClassifyPapersInfo(paperIds: string[]): Promise<ClassifyPaperBrief[]> {
  return invoke<ClassifyPaperBrief[]>('ai_classify_papers_info', { paperIds });
}

// ============================================================
// AI 综述生成
// ============================================================

export interface ReviewSection {
  title: string;
  points: string;
  /** Subsection headings; empty = the section is written as one block. */
  subsections: string[];
  /** Papers assigned to this section; preserved by the outline editor. */
  paperIds: string[];
}

export interface ReviewFinding {
  claim: string;
  evidenceIds: string[];
}

export interface ReviewEvidence {
  id: string;
  chunkId: string | null;
  page: number | null;
  quote: string;
  content: string;
}

export interface ReviewPaperCard {
  paperId: string;
  researchQuestion: string;
  methods: string;
  dataAndSample: string;
  findings: ReviewFinding[];
  limitations: string;
  relevance: string;
  evidence: ReviewEvidence[];
  sourceType: 'fulltext' | 'abstract';
  edited: boolean;
}

export interface ReviewSource {
  paperId: string;
  paperIndex: number;
  title: string;
  /** JSON-encoded author array, matching the library's Paper.authors format. */
  authors: string;
  year: number | null;
  sourceKind: 'fulltext' | 'abstract' | 'unusable';
  status: 'pending' | 'running' | 'done' | 'failed';
  chunkCount: number;
  abstractChars: number;
  card: ReviewPaperCard | null;
  error: string | null;
}

export interface ReviewSynthesisItem {
  title: string;
  detail: string;
  paperIds: string[];
}

export interface ReviewSynthesis {
  themes: ReviewSynthesisItem[];
  agreements: ReviewSynthesisItem[];
  conflicts: ReviewSynthesisItem[];
  gaps: ReviewSynthesisItem[];
}

export interface ReviewSectionResult {
  /** Stored snapshots use zero-based coordinates; progress events remain 1-based. */
  sectionIndex: number;
  status: 'pending' | 'running' | 'done' | 'failed';
  rawMarkdown: string;
  attempts: number;
  error: string | null;
}

export interface ReviewAudit {
  selectedCount: number;
  fulltextCount: number;
  abstractCount: number;
  unusableCount: number;
  citedPaperIds: string[];
  uncitedPaperIds: string[];
  invalidMarkers: string[];
  warnings: string[];
}

export interface ReviewSnapshot {
  runId: string;
  status: string;
  stage: string;
  question: string;
  scope: string;
  language: string;
  targetWords: number;
  providerId: string | null;
  paperIds: string[];
  sources: ReviewSource[];
  outline: ReviewSection[];
  synthesis: ReviewSynthesis | null;
  sections: ReviewSectionResult[];
  finalRawMarkdown: string | null;
  audit: ReviewAudit | null;
  noteId: string | null;
  error: string | null;
}

export interface ReviewProgress {
  runId: string;
  /** read | synthesis | read_ready | outline | section | revision | assemble | done | partial | cancelled | error */
  stage: string;
  /** 1-based section coordinate; 0 for reading, outline and revision events. */
  sectionIndex: number;
  totalSections: number;
  sectionTitle: string;
  noteId: string | null;
  message: string | null;
  /** Streaming text fragment of the section being written. */
  delta?: string;
  /** Reasoning fragment (thinking models), shown dimmed above the content. */
  reasoning?: string;
  /** running | done | failed — a section status, or a paper status when sectionIndex is 0. */
  sectionStatus?: 'running' | 'done' | 'failed';
  /** Failure text for this specific section. */
  error?: string | null;
  /** Retries already performed for this section (0 = first attempt). */
  attempt?: number;
  /** max_tokens in effect for this attempt (doubles per retry). */
  maxTokens?: number;
  /** Sections with content so far. */
  doneSections?: number;
  paperId?: string | null;
  donePapers?: number | null;
  totalPapers?: number | null;
}

export async function reviewCreate(
  paperIds: string[],
  question: string,
  scope: string,
  language: string,
  targetWords: number,
  providerId?: string | null
): Promise<ReviewSnapshot> {
  return invoke<ReviewSnapshot>('review_create', {
    paperIds,
    question,
    scope,
    language,
    targetWords,
    providerId: providerId ?? null,
  });
}

export async function reviewGet(runId: string): Promise<ReviewSnapshot> {
  return invoke<ReviewSnapshot>('review_get', { runId });
}

export async function reviewReadStart(runId: string): Promise<void> {
  return invoke<void>('review_read_start', { runId });
}

export async function reviewReadRetry(runId: string, paperId: string): Promise<void> {
  return invoke<void>('review_read_retry', { runId, paperId });
}

export async function reviewCardUpdate(
  runId: string,
  paperId: string,
  card: ReviewPaperCard
): Promise<ReviewSnapshot> {
  return invoke<ReviewSnapshot>('review_card_update', { runId, paperId, card });
}

export async function reviewGenerateOutline(runId: string): Promise<ReviewSection[]> {
  return invoke<ReviewSection[]>('review_generate_outline', { runId });
}

export async function reviewGenerateStart(runId: string, outline: ReviewSection[]): Promise<void> {
  return invoke<void>('review_generate_start', { runId, outline });
}

/** Retry one failed section using its stored zero-based sectionIndex. */
export async function reviewGenerateRetrySection(runId: string, sectionIndex: number): Promise<void> {
  return invoke<void>('review_generate_retry_section', { runId, sectionIndex });
}

export async function reviewGenerateCancel(runId: string): Promise<void> {
  return invoke<void>('review_generate_cancel', { runId });
}

/** Save the note from whatever sections exist (placeholders for the gaps). */
export async function reviewGenerateSave(runId: string): Promise<string> {
  return invoke<string>('review_generate_save', { runId });
}
