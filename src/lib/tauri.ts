import { invoke } from '@tauri-apps/api/core';
import { convertFileSrc } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  Paper, PaperInput, PaperLinkMetadata, PaperImportResult, LinkImportError, ListPapersParams,
  AgentSession, AgentStep, ChatMessage, ChatAttachment, LlmConfigBlock, ApprovalConfig, Project, CronJob,
  KnowledgeDomain, KnowledgeItem,
  ResearchTopic, ResearchSource, Note,
  FileEntry, SystemInfo, TimelineItem,
  Collection, Tag, LlmProvider, Vault, NoteVersion, Bookmark,
} from './types';

// ============================================================
// Library
// ============================================================

export async function importPaper(filePath: string): Promise<Paper> {
  return invoke<Paper>('import_paper', { filePath });
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

/** Rebuild a paper's text index (extract → chunk → embed). Returns chunk count. */
export async function paperReprocessIndex(id: string): Promise<number> {
  return invoke<number>('paper_reprocess_index', { id });
}

/** Enrich a paper's bibliographic metadata from CrossRef (DOI/title). */
export async function paperEnrichMetadata(id: string): Promise<boolean> {
  return invoke<boolean>('paper_enrich_metadata', { id });
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
  projectId?: string;
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
  maxTokens?: number;
  maxMemoryRounds?: number;
  memoryDir?: string;
  skillsDir?: string;
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

/** Create a pet (built-in domain agent) session bound to the current context. */
export async function petCreateSession(domain: string, context: Record<string, unknown>): Promise<AgentSession> {
  return invoke<AgentSession>('pet_create_session', { domain, context });
}

export interface PetDomainInfo {
  id: string;
  name: string;
  default_prompt: string;
}

/** List built-in pet domain agents with their default system prompts. */
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

export async function agentApproveTool(
  sessionId: string,
  toolCallId: string,
  approved: boolean,
  modifiedArgs?: Record<string, unknown>,
): Promise<void> {
  return invoke<void>('agent_approve_tool', {
    sessionId,
    toolCallId,
    approved,
    modifiedArgs,
  });
}

/** Cancel a running agent turn for a session. */
export async function agentCancel(sessionId: string): Promise<void> {
  return invoke<void>('agent_cancel', { sessionId });
}

/** Rename a session (title only). */
export async function agentRenameSession(sessionId: string, title: string): Promise<void> {
  return invoke<void>('agent_rename_session', { sessionId, title });
}

/** Answer a pending AskUserQuestion dialog for a session. */
export async function agentAnswerUser(
  sessionId: string,
  answers: { question: string; answer: string }[],
): Promise<void> {
  return invoke<void>('agent_answer_user', { sessionId, answers });
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

export async function agentListSessions(projectId?: string): Promise<AgentSession[]> {
  return invoke<AgentSession[]>('agent_list_sessions', { projectId });
}

export async function agentDeleteSession(sessionId: string): Promise<void> {
  return invoke<void>('agent_delete_session', { sessionId });
}

export async function agentPinSession(sessionId: string, pinned: boolean): Promise<void> {
  return invoke<void>('agent_pin_session', { sessionId, pinned });
}

export async function getAgentSteps(sessionId: string): Promise<AgentStep[]> {
  return invoke<AgentStep[]>('get_agent_steps', { sessionId });
}

export interface AppSettings {
  default_llm?: LlmConfigBlock;
  default_llm_provider_id?: string | null;
  default_approval: ApprovalConfig;
  default_max_loops: number;
  default_max_tokens: number;
  default_max_memory_rounds: number;
  data_dir?: string | null;
  memory_dir?: string | null;
  skills_dir?: string | null;

  // UI features
  show_pet?: boolean;
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
  embedding_backend?: string;
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

export async function getChatMessages(sessionId: string): Promise<ChatMessage[]> {
  return invoke<ChatMessage[]>('get_chat_messages', { sessionId });
}

// ============================================================
// Projects (Codex-style project folders)
// ============================================================

export async function projectsList(): Promise<Project[]> {
  return invoke<Project[]>('projects_list');
}

export async function projectCreate(input: { name?: string; path: string }): Promise<Project> {
  return invoke<Project>('project_create', { input });
}

export async function projectUpdate(id: string, input: { name?: string }): Promise<Project> {
  return invoke<Project>('project_update', { id, input });
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
  type: 'delta' | 'done' | 'error';
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
      unlisten?.();
      action();
    };
    // Safety net: the backend has its own HTTP timeouts, but guard against a
    // silently hung stream.
    const timer = setTimeout(() => finish(() => reject(new Error('翻译超时'))), 120_000);

    listen<TranslationStreamEvent>('translation:event', (event) => {
      const e = event.payload;
      if (e.request_id !== requestId) return;
      // Deltas drive the live display; done/error are informational because
      // resolution comes from the invoke result below.
      if (e.type === 'delta' && e.content) onDelta?.(e.content);
    })
      .then((fn) => {
        unlisten = fn;
        return invoke<string>('translate_text_stream', { text, sourceLang, targetLang, requestId });
      })
      .then((full) => {
        clearTimeout(timer);
        finish(() => resolve(full));
      })
      .catch((err) => {
        clearTimeout(timer);
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
): Promise<void> {
  return invoke<void>('research_update_topic', { topicId, status, description });
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

// ============================================================
// Search
// ============================================================

export async function searchHybrid(query: string, limit?: number): Promise<unknown[]> {
  return invoke('search_hybrid', { query, limit });
}
export async function searchGenerateEmbeddings(paperId: string): Promise<number> {
  return invoke<number>('search_generate_embeddings', { paperId });
}
export async function searchRagQuery(query: string, topK?: number): Promise<string> {
  return invoke<string>('search_rag_query', { query, topK });
}

// ============================================================
// Notes
// ============================================================

export async function notesCreate(title: string, content: string, paperId?: string, parentId?: string, isFolder?: boolean): Promise<Note> {
  return invoke<Note>('notes_create', { title, content, paperId, parentId, isFolder });
}
export async function notesGet(id: string): Promise<Note> {
  return invoke<Note>('notes_get', { id });
}
export async function notesUpdate(id: string, title?: string, content?: string, paperId?: string, aliases?: string, isFavorite?: number, touch?: boolean): Promise<Note> {
  return invoke<Note>('notes_update', { id, title, content, paperId, aliases, isFavorite, touch });
}
export async function notesDelete(id: string): Promise<void> {
  return invoke<void>('notes_delete', { id });
}
export async function notesList(paperId?: string, search?: string, parentId?: string): Promise<Note[]> {
  return invoke<Note[]>('notes_list', { paperId, search, parentId });
}
export async function notesListAll(): Promise<Note[]> {
  return invoke<Note[]>('notes_list_all');
}
export async function notesMove(id: string, parentId?: string | null, sortOrder?: number): Promise<Note> {
  return invoke<Note>('notes_move', { id, parentId, sortOrder });
}
export async function notesGetBacklinks(noteId: string): Promise<{ id: string; title: string; context: string; created_at: string }[]> {
  return invoke('notes_get_backlinks', { noteId });
}

export interface NoteSearchResult {
  id: string;
  title: string;
  snippet: string;
  updated_at: string;
}

/** Full-text search across all notes (ranked, with snippets). */
export async function notesSearch(query: string, limit?: number): Promise<NoteSearchResult[]> {
  return invoke<NoteSearchResult[]>('notes_search', { query, limit });
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
export async function vaultImport(id: string, sourceDir: string): Promise<{ imported: number; skipped: number }> {
  return invoke<{ imported: number; skipped: number }>('vault_import', { id, sourceDir });
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

/** Read a text file's content (bounded) — used by the chat attach feature. */
export async function readTextFile(path: string): Promise<string> {
  return invoke<string>('read_text_file', { path });
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
  revoked: boolean;
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

export async function deviceRevoke(relayUrl: string, deviceId: string): Promise<void> {
  return invoke<void>('device_revoke', { relayUrl, deviceId });
}

export async function deviceRename(relayUrl: string, deviceId: string, name: string): Promise<void> {
  return invoke<void>('device_rename', { relayUrl, deviceId, name });
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
