-- 修复旧库：去掉 4 张同步关联表的受检外键约束。
-- crsqlite 注册 CRR 时不允许表带 REFERENCES（可被行级复制违反），
-- 新装库的 schema_init.sql 已去掉这些外键，旧库需重建表以匹配。
-- 用法: sqlite3 siku.db < scripts/fix-old-db-drop-fks.sql
PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;

BEGIN;

ALTER TABLE note_links RENAME TO note_links_old;
CREATE TABLE note_links (
    source_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    context TEXT,
    created_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (source_id, target_id)
);
INSERT INTO note_links SELECT * FROM note_links_old;
DROP TABLE note_links_old;

ALTER TABLE paper_tags RENAME TO paper_tags_old;
CREATE TABLE paper_tags (
    paper_id TEXT NOT NULL,
    tag_id TEXT NOT NULL,
    PRIMARY KEY (paper_id, tag_id)
);
INSERT INTO paper_tags SELECT * FROM paper_tags_old;
DROP TABLE paper_tags_old;

ALTER TABLE paper_collections RENAME TO paper_collections_old;
CREATE TABLE paper_collections (
    paper_id TEXT NOT NULL,
    collection_id TEXT NOT NULL,
    PRIMARY KEY (paper_id, collection_id)
);
INSERT INTO paper_collections SELECT * FROM paper_collections_old;
DROP TABLE paper_collections_old;

ALTER TABLE related_papers RENAME TO related_papers_old;
CREATE TABLE related_papers (
    paper_id TEXT NOT NULL,
    related_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (paper_id, related_id)
);
INSERT INTO related_papers SELECT * FROM related_papers_old;
DROP TABLE related_papers_old;
CREATE INDEX IF NOT EXISTS idx_related_paper ON related_papers(paper_id);
CREATE INDEX IF NOT EXISTS idx_related_other ON related_papers(related_id);

COMMIT;

PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;
