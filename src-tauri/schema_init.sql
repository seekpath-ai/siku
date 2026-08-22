-- ============================================================
-- Siku Schema Init — 持久化版本，使用 IF NOT EXISTS 保留数据
-- ============================================================

-- papers
CREATE TABLE IF NOT EXISTS papers (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    authors TEXT NOT NULL DEFAULT '[]',
    year INTEGER,
    journal TEXT,
    doi TEXT,
    url TEXT,
    abstract TEXT,
    keywords TEXT NOT NULL DEFAULT '[]',
    citation_key TEXT,
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
    deleted_at TEXT,
    is_favorite INTEGER NOT NULL DEFAULT 0,
    read_status TEXT NOT NULL DEFAULT 'unread',
    last_read_at TEXT,
    created_at TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT '',
    imported_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_papers_year ON papers(year);
CREATE INDEX IF NOT EXISTS idx_papers_journal ON papers(journal);
CREATE INDEX IF NOT EXISTS idx_papers_created ON papers(created_at);
CREATE INDEX IF NOT EXISTS idx_papers_citation_key ON papers(citation_key);

-- attachments
CREATE TABLE IF NOT EXISTS attachments (
    id TEXT PRIMARY KEY,
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    file_name TEXT NOT NULL,
    file_path TEXT NOT NULL,
    file_type TEXT NOT NULL,
    description TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_attachments_paper ON attachments(paper_id);

-- tags
CREATE TABLE IF NOT EXISTS tags (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    color TEXT DEFAULT '#3b82f6',
    parent_id TEXT,
    created_at TEXT NOT NULL DEFAULT ''
);

-- paper_tags
CREATE TABLE IF NOT EXISTS paper_tags (
    paper_id TEXT NOT NULL,
    tag_id TEXT NOT NULL,
    PRIMARY KEY (paper_id, tag_id)
);

-- collections
CREATE TABLE IF NOT EXISTS collections (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    parent_id TEXT,
    sort_order INTEGER DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT ''
);

-- paper_collections
CREATE TABLE IF NOT EXISTS paper_collections (
    paper_id TEXT NOT NULL,
    collection_id TEXT NOT NULL,
    PRIMARY KEY (paper_id, collection_id)
);

-- structured creators (role + name per paper; device-local overlay on the
-- sync-transport authors/editor JSON columns — write-through keeps them in sync)
CREATE TABLE IF NOT EXISTS creators (
    id TEXT PRIMARY KEY,
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'author',
    last_name TEXT NOT NULL DEFAULT '',
    first_name TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL DEFAULT '',
    sort_order INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_creators_paper ON creators(paper_id);

-- related papers (bidirectional item links)
CREATE TABLE IF NOT EXISTS related_papers (
    paper_id TEXT NOT NULL,
    related_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (paper_id, related_id)
);
CREATE INDEX IF NOT EXISTS idx_related_paper ON related_papers(paper_id);
CREATE INDEX IF NOT EXISTS idx_related_other ON related_papers(related_id);

-- saved searches (device-local; not synced)
CREATE TABLE IF NOT EXISTS saved_searches (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    params_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT ''
);

-- notes
CREATE TABLE IF NOT EXISTS notes (
    id TEXT PRIMARY KEY NOT NULL,
    vault_id TEXT NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001',
    paper_id TEXT,
    title TEXT NOT NULL DEFAULT '',
    content TEXT NOT NULL DEFAULT '',
    content_plain TEXT NOT NULL DEFAULT '',
    tags TEXT NOT NULL DEFAULT '[]',
    aliases TEXT NOT NULL DEFAULT '[]',
    is_favorite INTEGER DEFAULT 0,
    is_folder INTEGER DEFAULT 0,
    is_system INTEGER DEFAULT 0,
    source_collection_id TEXT,
    is_excerpt INTEGER DEFAULT 0,
    agent_edited_at TEXT,
    agent_edit_count INTEGER DEFAULT 0,
    parent_id TEXT,
    sort_order INTEGER DEFAULT 0,
    is_literature_note INTEGER DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_notes_paper ON notes(paper_id);

-- note_versions: snapshots for AI-edited notes (version history / restore)
CREATE TABLE IF NOT EXISTS note_versions (
    id TEXT PRIMARY KEY NOT NULL,
    note_id TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    content TEXT NOT NULL DEFAULT '',
    edited_by TEXT NOT NULL DEFAULT 'agent',
    created_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_note_versions_note ON note_versions(note_id);

-- vaults (Obsidian-style note vaults). TEXT uuid PK for multi-device sync;
-- the default vault uses a fixed id so every device converges on one row.
-- name carries no UNIQUE constraint: CRDT merges cannot enforce uniqueness.
CREATE TABLE IF NOT EXISTS vaults (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT ''
);

-- note_links
CREATE TABLE IF NOT EXISTS note_links (
    source_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    context TEXT,
    created_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (source_id, target_id)
);

-- annotations
CREATE TABLE IF NOT EXISTS annotations (
    id TEXT PRIMARY KEY NOT NULL,
    paper_id TEXT NOT NULL DEFAULT '',
    page INTEGER NOT NULL DEFAULT 0,
    type TEXT NOT NULL DEFAULT '',
    rect TEXT NOT NULL DEFAULT '',
    color TEXT DEFAULT '#ffeb3b',
    text TEXT,
    note TEXT,
    tags TEXT DEFAULT '[]',
    translation TEXT,
    created_at TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_annotations_paper ON annotations(paper_id);

-- chunks
CREATE TABLE IF NOT EXISTS chunks (
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
CREATE INDEX IF NOT EXISTS idx_chunks_paper ON chunks(paper_id);

-- embeddings
CREATE TABLE IF NOT EXISTS embeddings (
    chunk_id TEXT PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    dimensions INTEGER NOT NULL,
    vector BLOB NOT NULL,
    created_at TEXT NOT NULL
);

-- projects: Codex-style project (a local folder the agent works in)
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_path ON projects(path);

-- chat_sessions
CREATE TABLE IF NOT EXISTS chat_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL DEFAULT 'New Chat',
    mode TEXT NOT NULL DEFAULT 'qa',
    paper_ids TEXT NOT NULL DEFAULT '[]',
    agent_mode TEXT NOT NULL DEFAULT 'chat',
    project_id TEXT,
    working_dir TEXT,
    tools_enabled TEXT NOT NULL DEFAULT '[]',
    system_prompt TEXT,
    llm_models TEXT,
    llm_provider_ids TEXT,
    approval_config TEXT,
    max_loops INTEGER DEFAULT 10,
    max_tokens INTEGER DEFAULT 28000,
    max_memory_rounds INTEGER DEFAULT 10,
    memory_file_path TEXT,
    memory_dir TEXT,
    skills_dir TEXT,
    is_pinned INTEGER DEFAULT 0,
    sort_order INTEGER DEFAULT 0,
    icon TEXT,
    color TEXT,
    domain TEXT,
    context TEXT,
    created_at TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT ''
);

-- llm_providers: unified LLM configuration pool
CREATE TABLE IF NOT EXISTS llm_providers (
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
    is_vision INTEGER DEFAULT 0,
    sort_order INTEGER DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_llm_providers_default ON llm_providers(is_default);

-- chat_messages
CREATE TABLE IF NOT EXISTS chat_messages (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL DEFAULT '',
    role TEXT NOT NULL DEFAULT '',
    content TEXT NOT NULL DEFAULT '',
    reasoning_content TEXT,
    tool_calls TEXT,
    tool_call_id TEXT,
    tool_name TEXT,
    citations TEXT,
    model TEXT,
    tokens_used INTEGER,
    created_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_chat_messages_session ON chat_messages(session_id);

-- agent_steps: one row per ReAct iteration (thought + tool_calls + observations)
CREATE TABLE IF NOT EXISTS agent_steps (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    message_id TEXT REFERENCES chat_messages(id) ON DELETE CASCADE,
    step_index INTEGER NOT NULL,
    reasoning_content TEXT,
    tool_calls TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_steps_session ON agent_steps(session_id);
CREATE INDEX IF NOT EXISTS idx_agent_steps_message ON agent_steps(message_id);

-- translation_cache
CREATE TABLE IF NOT EXISTS translation_cache (
    id TEXT PRIMARY KEY,
    source_hash TEXT NOT NULL,
    source_embedding BLOB,
    translation TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_translation_cache_hash_unique ON translation_cache(source_hash, model);

-- saved_items
CREATE TABLE IF NOT EXISTS saved_items (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT,
    url TEXT,
    doi TEXT,
    pdf_url TEXT,
    metadata TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    error TEXT,
    created_at TEXT NOT NULL DEFAULT '',
    processed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_saved_items_status ON saved_items(status);

-- settings (global, syncable)
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT ''
);

-- device_settings (device-local, not synced)
CREATE TABLE IF NOT EXISTS device_settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT ''
);

-- imports
CREATE TABLE IF NOT EXISTS imports (
    id TEXT PRIMARY KEY NOT NULL,
    file_path TEXT,
    source_url TEXT,
    paper_id TEXT,
    status TEXT NOT NULL DEFAULT '',
    error TEXT,
    created_at TEXT NOT NULL DEFAULT '',
    completed_at TEXT
);

-- tool_executions
CREATE TABLE IF NOT EXISTS tool_executions (
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
CREATE INDEX IF NOT EXISTS idx_te_session ON tool_executions(session_id);
CREATE INDEX IF NOT EXISTS idx_te_created ON tool_executions(created_at);

-- knowledge_domains
CREATE TABLE IF NOT EXISTS knowledge_domains (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    domain_type TEXT NOT NULL CHECK(domain_type IN ('research','learning','life','reading','notes')),
    icon TEXT,
    color TEXT DEFAULT '#3b82f6',
    sort_order INTEGER DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
INSERT OR IGNORE INTO knowledge_domains (id, name, domain_type, icon, color, sort_order, created_at, updated_at) VALUES
    ('dom-research', '学术研究', 'research', 'graduation-cap', '#3b82f6', 0, '2026-06-17T00:00:00Z', '2026-06-17T00:00:00Z'),
    ('dom-learning',  '学习提升', 'learning', 'book-open', '#27ae60', 1, '2026-06-17T00:00:00Z', '2026-06-17T00:00:00Z'),
    ('dom-life',      '生活记录', 'life', 'heart', '#e67e22', 2, '2026-06-17T00:00:00Z', '2026-06-17T00:00:00Z'),
    ('dom-reading',   '阅读笔记', 'reading', 'bookmark', '#8e44ad', 3, '2026-06-17T00:00:00Z', '2026-06-17T00:00:00Z'),
    ('dom-notes',     '个人笔记', 'notes', 'sticky-note', '#95a5a6', 4, '2026-06-17T00:00:00Z', '2026-06-17T00:00:00Z');

-- knowledge_items
CREATE TABLE IF NOT EXISTS knowledge_items (
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
CREATE INDEX IF NOT EXISTS idx_ki_domain ON knowledge_items(domain_id);
CREATE INDEX IF NOT EXISTS idx_ki_source ON knowledge_items(source_type, source_id);
CREATE INDEX IF NOT EXISTS idx_ki_created ON knowledge_items(created_at);

-- research_topics
CREATE TABLE IF NOT EXISTS research_topics (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    keywords TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','paused','completed','archived')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- research_sources
CREATE TABLE IF NOT EXISTS research_sources (
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
CREATE INDEX IF NOT EXISTS idx_rs_topic ON research_sources(topic_id);
CREATE INDEX IF NOT EXISTS idx_rs_status ON research_sources(status);

-- system_events
CREATE TABLE IF NOT EXISTS system_events (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    level TEXT NOT NULL DEFAULT 'info',
    message TEXT NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_se_type ON system_events(event_type);
CREATE INDEX IF NOT EXISTS idx_se_created ON system_events(created_at);

-- file_bookmarks
CREATE TABLE IF NOT EXISTS file_bookmarks (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    path TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT ''
);

-- bookmarks: user-saved routes / pages
CREATE TABLE IF NOT EXISTS bookmarks (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    route TEXT NOT NULL DEFAULT '',
    params_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_bookmarks_route ON bookmarks(route);

-- FTS5 virtual tables
CREATE VIRTUAL TABLE IF NOT EXISTS papers_fts USING fts5(
    title, abstract, keywords,
    content='papers', content_rowid='rowid'
);
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
    title, content_plain,
    content='notes', content_rowid='rowid'
);
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    content,
    tokenize='trigram',
    content='chunks', content_rowid='rowid'
);
CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_items_fts USING fts5(
    title, content,
    content='knowledge_items', content_rowid='rowid'
);

-- FTS sync triggers (keep the external-content indexes in sync)
CREATE TRIGGER IF NOT EXISTS papers_fts_ai AFTER INSERT ON papers BEGIN
  INSERT INTO papers_fts(rowid, title, abstract, keywords) VALUES (new.rowid, new.title, new.abstract, new.keywords);
END;
CREATE TRIGGER IF NOT EXISTS papers_fts_ad AFTER DELETE ON papers BEGIN
  INSERT INTO papers_fts(papers_fts, rowid, title, abstract, keywords) VALUES('delete', old.rowid, old.title, old.abstract, old.keywords);
END;
CREATE TRIGGER IF NOT EXISTS papers_fts_au AFTER UPDATE ON papers BEGIN
  INSERT INTO papers_fts(papers_fts, rowid, title, abstract, keywords) VALUES('delete', old.rowid, old.title, old.abstract, old.keywords);
  INSERT INTO papers_fts(rowid, title, abstract, keywords) VALUES (new.rowid, new.title, new.abstract, new.keywords);
END;

CREATE TRIGGER IF NOT EXISTS notes_fts_ai AFTER INSERT ON notes BEGIN
  INSERT INTO notes_fts(rowid, title, content_plain) VALUES (new.rowid, new.title, new.content_plain);
END;
CREATE TRIGGER IF NOT EXISTS notes_fts_ad AFTER DELETE ON notes BEGIN
  INSERT INTO notes_fts(notes_fts, rowid, title, content_plain) VALUES('delete', old.rowid, old.title, old.content_plain);
END;
CREATE TRIGGER IF NOT EXISTS notes_fts_au AFTER UPDATE ON notes BEGIN
  INSERT INTO notes_fts(notes_fts, rowid, title, content_plain) VALUES('delete', old.rowid, old.title, old.content_plain);
  INSERT INTO notes_fts(rowid, title, content_plain) VALUES (new.rowid, new.title, new.content_plain);
END;

CREATE TRIGGER IF NOT EXISTS chunks_fts_ai AFTER INSERT ON chunks BEGIN
  INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
END;
CREATE TRIGGER IF NOT EXISTS chunks_fts_ad AFTER DELETE ON chunks BEGIN
  INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES('delete', old.rowid, old.content);
END;
CREATE TRIGGER IF NOT EXISTS chunks_fts_au AFTER UPDATE ON chunks BEGIN
  INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES('delete', old.rowid, old.content);
  INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS knowledge_items_fts_ai AFTER INSERT ON knowledge_items BEGIN
  INSERT INTO knowledge_items_fts(rowid, title, content) VALUES (new.rowid, new.title, new.content);
END;
CREATE TRIGGER IF NOT EXISTS knowledge_items_fts_ad AFTER DELETE ON knowledge_items BEGIN
  INSERT INTO knowledge_items_fts(knowledge_items_fts, rowid, title, content) VALUES('delete', old.rowid, old.title, old.content);
END;
CREATE TRIGGER IF NOT EXISTS knowledge_items_fts_au AFTER UPDATE ON knowledge_items BEGIN
  INSERT INTO knowledge_items_fts(knowledge_items_fts, rowid, title, content) VALUES('delete', old.rowid, old.title, old.content);
  INSERT INTO knowledge_items_fts(rowid, title, content) VALUES (new.rowid, new.title, new.content);
END;

-- cron_jobs: scheduled agent prompts
CREATE TABLE IF NOT EXISTS cron_jobs (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    cron TEXT NOT NULL,
    prompt TEXT NOT NULL,
    recurring INTEGER DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_cron_jobs_session ON cron_jobs(session_id);

-- sync_outbox: encrypted changesets awaiting delivery to a peer (mailbox fallback)
CREATE TABLE IF NOT EXISTS sync_outbox (
    id TEXT PRIMARY KEY,
    to_device_id TEXT NOT NULL,
    ciphertext TEXT NOT NULL,
    nonce TEXT NOT NULL,
    ttl_seconds INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    retry_count INTEGER DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_sync_outbox_peer ON sync_outbox(to_device_id);
