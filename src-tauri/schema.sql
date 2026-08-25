-- ============================================================
-- Siku Schema — 开发阶段，每次启动 DROP + CREATE
-- ============================================================

-- ///////////////////////////////////////////////////////////
-- DROP all (reverse dependency order)
-- ///////////////////////////////////////////////////////////
DROP TABLE IF EXISTS file_bookmarks;
DROP TABLE IF EXISTS system_events;
DROP TABLE IF EXISTS research_sources;
DROP TABLE IF EXISTS research_topics;
DROP TABLE IF EXISTS knowledge_items;
DROP TABLE IF EXISTS knowledge_domains;
DROP TABLE IF EXISTS tool_executions;
DROP TABLE IF EXISTS imports;
DROP TABLE IF EXISTS settings;
DROP TABLE IF EXISTS saved_items;
DROP TABLE IF EXISTS translation_cache;
DROP TABLE IF EXISTS chat_messages;
DROP TABLE IF EXISTS chat_sessions;
DROP TABLE IF EXISTS embeddings;
DROP TABLE IF EXISTS chunks;
DROP TABLE IF EXISTS annotations;
DROP TABLE IF EXISTS note_links;
DROP TABLE IF EXISTS notes;
DROP TABLE IF EXISTS vaults;
DROP TABLE IF EXISTS paper_collections;
DROP TABLE IF EXISTS collections;
DROP TABLE IF EXISTS paper_tags;
DROP TABLE IF EXISTS tags;
DROP TABLE IF EXISTS attachments;
DROP TABLE IF EXISTS papers;

DROP TABLE IF EXISTS knowledge_items_fts;
DROP TABLE IF EXISTS chunks_fts;
DROP TABLE IF EXISTS notes_fts;
DROP TABLE IF EXISTS papers_fts;

-- ///////////////////////////////////////////////////////////
-- CREATE all
-- ///////////////////////////////////////////////////////////

-- papers
CREATE TABLE papers (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    authors TEXT NOT NULL DEFAULT '[]',
    year INTEGER,
    journal TEXT,
    doi TEXT,
    url TEXT,
    abstract TEXT,
    keywords TEXT NOT NULL DEFAULT '[]',
    citation_key TEXT UNIQUE,
    bibtex TEXT,
    file_path TEXT,
    file_size INTEGER,
    page_count INTEGER,
    language TEXT DEFAULT 'en',
    item_type TEXT DEFAULT 'journal',
    volume TEXT,
    issue TEXT,
    pages TEXT,
    conference_name TEXT,
    publisher TEXT,
    place TEXT,
    editor TEXT NOT NULL DEFAULT '[]',
    series TEXT,
    edition TEXT,
    isbn TEXT,
    issn TEXT,
    num_pages INTEGER,
    archive_location TEXT,
    call_number TEXT,
    rights TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    imported_at TEXT NOT NULL
);
CREATE INDEX idx_papers_year ON papers(year);
CREATE INDEX idx_papers_journal ON papers(journal);
CREATE INDEX idx_papers_created ON papers(created_at);
CREATE INDEX idx_papers_citation_key ON papers(citation_key);

-- attachments
CREATE TABLE attachments (
    id TEXT PRIMARY KEY,
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    file_name TEXT NOT NULL,
    file_path TEXT NOT NULL,
    file_type TEXT NOT NULL,
    description TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_attachments_paper ON attachments(paper_id);

-- tags
CREATE TABLE tags (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    color TEXT DEFAULT '#3b82f6',
    parent_id TEXT REFERENCES tags(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL
);

-- paper_tags
CREATE TABLE paper_tags (
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (paper_id, tag_id)
);

-- collections
CREATE TABLE collections (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    parent_id TEXT REFERENCES collections(id) ON DELETE CASCADE,
    sort_order INTEGER DEFAULT 0,
    created_at TEXT NOT NULL
);

-- paper_collections
CREATE TABLE paper_collections (
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    PRIMARY KEY (paper_id, collection_id)
);

-- notes
CREATE TABLE notes (
    id TEXT PRIMARY KEY,
    vault_id INTEGER NOT NULL DEFAULT 1 REFERENCES vaults(id) ON DELETE CASCADE,
    paper_id TEXT REFERENCES papers(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL DEFAULT '',
    content_plain TEXT NOT NULL DEFAULT '',
    parent_id TEXT REFERENCES notes(id) ON DELETE CASCADE,
    sort_order INTEGER DEFAULT 0,
    is_literature_note INTEGER DEFAULT 0,
    is_folder INTEGER DEFAULT 0,
    is_system INTEGER DEFAULT 0,
    source_collection_id TEXT,
    is_excerpt INTEGER DEFAULT 0,
    agent_edited_at TEXT,
    agent_edit_count INTEGER DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_notes_paper ON notes(paper_id);
CREATE INDEX idx_notes_vault ON notes(vault_id);

-- note_versions: snapshots for AI-edited notes (version history / restore)
CREATE TABLE note_versions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    content TEXT NOT NULL DEFAULT '',
    edited_by TEXT NOT NULL DEFAULT 'agent',
    created_at TEXT NOT NULL
);
CREATE INDEX idx_note_versions_note ON note_versions(note_id);

-- vaults (Obsidian-style note vaults)
CREATE TABLE vaults (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- note_links
CREATE TABLE note_links (
    source_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    target_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    context TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (source_id, target_id)
);

-- annotations
CREATE TABLE annotations (
    id TEXT PRIMARY KEY,
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    page INTEGER NOT NULL,
    type TEXT NOT NULL,
    rect TEXT NOT NULL,
    color TEXT DEFAULT '#ffeb3b',
    text TEXT,
    note TEXT,
    tags TEXT DEFAULT '[]',
    translation TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_annotations_paper ON annotations(paper_id);

-- chunks
CREATE TABLE chunks (
    id TEXT PRIMARY KEY,
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    page_start INTEGER,
    page_end INTEGER,
    section TEXT,
    chunk_index INTEGER NOT NULL,
    token_count INTEGER,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_chunks_paper ON chunks(paper_id);

-- embeddings
CREATE TABLE embeddings (
    chunk_id TEXT PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    dimensions INTEGER NOT NULL,
    vector BLOB NOT NULL,
    created_at TEXT NOT NULL
);

-- chat_sessions (agent-centric: each row is an agent)
CREATE TABLE chat_sessions (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT 'New Chat',
    mode TEXT NOT NULL DEFAULT 'qa',
    paper_ids TEXT NOT NULL DEFAULT '[]',
    agent_mode TEXT NOT NULL DEFAULT 'chat',
    tools_enabled TEXT NOT NULL DEFAULT '[]',
    system_prompt TEXT,
    -- Agent configuration (JSON)
    llm_models TEXT,              -- JSON array of LlmConfig blocks; first is active (legacy)
    llm_provider_ids TEXT,        -- JSON array of llm_providers.id; first is active
    approval_config TEXT,         -- JSON {mode, expire_sec, whitelist}
    max_loops INTEGER DEFAULT 10,
    max_tokens INTEGER DEFAULT 28000,
    max_memory_rounds INTEGER DEFAULT 10,
    memory_file_path TEXT,        -- optional JSONL memory file override
    memory_dir TEXT,              -- optional per-agent memory directory
    skills_dir TEXT,              -- optional per-agent skills directory
    -- UI / ordering
    is_pinned INTEGER DEFAULT 0,
    sort_order INTEGER DEFAULT 0,
    icon TEXT,
    color TEXT,
    domain TEXT,
    context TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- llm_providers: unified LLM configuration pool
CREATE TABLE llm_providers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    api_key TEXT NOT NULL DEFAULT '',
    base_url TEXT NOT NULL,
    proxy TEXT,
    max_tokens INTEGER DEFAULT 4096,
    temperature REAL DEFAULT 0.7,
    extra_body TEXT,
    is_default INTEGER DEFAULT 0,
    sort_order INTEGER DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_llm_providers_default ON llm_providers(is_default);

-- chat_messages (tool_calls/tool_call_id/tool_name already in base schema)
CREATE TABLE chat_messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    tool_calls TEXT,
    tool_call_id TEXT,
    tool_name TEXT,
    citations TEXT,
    model TEXT,
    tokens_used INTEGER,
    attachments TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_chat_messages_session ON chat_messages(session_id);

-- translation_cache
CREATE TABLE translation_cache (
    id TEXT PRIMARY KEY,
    source_hash TEXT NOT NULL,
    source_embedding BLOB,
    translation TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_translation_cache_hash_unique ON translation_cache(source_hash, model);

-- saved_items
CREATE TABLE saved_items (
    id TEXT PRIMARY KEY,
    title TEXT,
    url TEXT,
    doi TEXT,
    pdf_url TEXT,
    metadata TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    error TEXT,
    created_at TEXT NOT NULL,
    processed_at TEXT
);
CREATE INDEX idx_saved_items_status ON saved_items(status);

-- settings
CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- imports
CREATE TABLE imports (
    id TEXT PRIMARY KEY,
    file_path TEXT NOT NULL,
    paper_id TEXT REFERENCES papers(id) ON DELETE SET NULL,
    status TEXT NOT NULL,
    error TEXT,
    created_at TEXT NOT NULL,
    completed_at TEXT
);

-- tool_executions (from 0003)
CREATE TABLE tool_executions (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    message_id TEXT REFERENCES chat_messages(id) ON DELETE SET NULL,
    tool_name TEXT NOT NULL,
    tool_input TEXT NOT NULL,
    tool_output TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    duration_ms INTEGER,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_te_session ON tool_executions(session_id);
CREATE INDEX idx_te_created ON tool_executions(created_at);

-- knowledge_domains (from 0003)
CREATE TABLE knowledge_domains (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    domain_type TEXT NOT NULL CHECK(domain_type IN ('research','learning','life','reading','notes')),
    icon TEXT,
    color TEXT DEFAULT '#3b82f6',
    sort_order INTEGER DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
INSERT INTO knowledge_domains (id, name, domain_type, icon, color, sort_order, created_at, updated_at) VALUES
    ('dom-research', '学术研究', 'research', 'graduation-cap', '#3b82f6', 0, '2026-06-17T00:00:00Z', '2026-06-17T00:00:00Z'),
    ('dom-learning',  '学习提升', 'learning', 'book-open', '#27ae60', 1, '2026-06-17T00:00:00Z', '2026-06-17T00:00:00Z'),
    ('dom-life',      '生活记录', 'life', 'heart', '#e67e22', 2, '2026-06-17T00:00:00Z', '2026-06-17T00:00:00Z'),
    ('dom-reading',   '阅读笔记', 'reading', 'bookmark', '#8e44ad', 3, '2026-06-17T00:00:00Z', '2026-06-17T00:00:00Z'),
    ('dom-notes',     '个人笔记', 'notes', 'sticky-note', '#95a5a6', 4, '2026-06-17T00:00:00Z', '2026-06-17T00:00:00Z');

-- knowledge_items (from 0003)
CREATE TABLE knowledge_items (
    id TEXT PRIMARY KEY,
    domain_id TEXT NOT NULL REFERENCES knowledge_domains(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    content_type TEXT NOT NULL DEFAULT 'note',
    content TEXT,
    source_type TEXT,
    source_id TEXT,
    metadata TEXT NOT NULL DEFAULT '{}',
    tags TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_ki_domain ON knowledge_items(domain_id);
CREATE INDEX idx_ki_source ON knowledge_items(source_type, source_id);
CREATE INDEX idx_ki_created ON knowledge_items(created_at);

-- research_topics (from 0003)
CREATE TABLE research_topics (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    keywords TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','paused','completed','archived')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- research_sources (from 0003)
CREATE TABLE research_sources (
    id TEXT PRIMARY KEY,
    topic_id TEXT NOT NULL REFERENCES research_topics(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL CHECK(source_type IN ('arxiv','scholar','crossref','manual')),
    source_id TEXT,
    title TEXT,
    authors TEXT,
    url TEXT,
    doi TEXT,
    status TEXT NOT NULL DEFAULT 'discovered' CHECK(status IN ('discovered','downloaded','imported','read')),
    metadata TEXT NOT NULL DEFAULT '{}',
    discovered_at TEXT NOT NULL,
    processed_at TEXT
);
CREATE INDEX idx_rs_topic ON research_sources(topic_id);
CREATE INDEX idx_rs_status ON research_sources(status);

-- system_events (from 0003)
CREATE TABLE system_events (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    level TEXT NOT NULL DEFAULT 'info',
    message TEXT NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);
CREATE INDEX idx_se_type ON system_events(event_type);
CREATE INDEX idx_se_created ON system_events(created_at);

-- file_bookmarks (from 0003)
CREATE TABLE file_bookmarks (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    created_at TEXT NOT NULL
);

-- bookmarks: user-saved routes / pages
CREATE TABLE bookmarks (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    route TEXT NOT NULL,
    params_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);
CREATE INDEX idx_bookmarks_route ON bookmarks(route);

-- ///////////////////////////////////////////////////////////
-- FTS5 virtual tables (from 0002, 0004)
-- ///////////////////////////////////////////////////////////
CREATE VIRTUAL TABLE papers_fts USING fts5(
    title, abstract, keywords,
    content='papers', content_rowid='rowid'
);
CREATE VIRTUAL TABLE notes_fts USING fts5(
    title, content_plain,
    content='notes', content_rowid='rowid'
);
CREATE VIRTUAL TABLE chunks_fts USING fts5(
    content,
    content='chunks', content_rowid='rowid'
);
CREATE VIRTUAL TABLE knowledge_items_fts USING fts5(
    title, content,
    content='knowledge_items', content_rowid='rowid'
);
