#!/usr/bin/env node
/**
 * 查看 Siku（思库）的文献分块索引。
 *
 * 用法（在项目根目录）：
 *   node --experimental-sqlite view-chunks.mjs                 # 列出所有文献的分块数概览
 *   node --experimental-sqlite view-chunks.mjs "Need to"        # 按标题关键词查看某篇文献的全部分块
 *   node --experimental-sqlite view-chunks.mjs 6913cf88         # 按 id 前缀查看
 *
 * 可用环境变量 SIKU_DB 覆盖数据库路径（默认 Windows 用户目录）。
 */
import { DatabaseSync } from 'node:sqlite';
import { existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { execSync } from 'node:child_process';

// Windows 终端默认代码页多为 GBK（936），而脚本输出 UTF-8。
// 将控制台切到 UTF-8（65001）避免中文乱码（仅影响显示，不影响数据）。
if (process.platform === 'win32' && process.stdout.isTTY) {
  try {
    execSync('chcp 65001 >nul', { stdio: 'ignore' });
  } catch {
    /* 忽略：非 cmd 环境（如 Windows Terminal / Git Bash）无需切换 */
  }
}

const DB_PATH =
  process.env.SIKU_DB ||
  join(homedir(), 'AppData', 'Roaming', 'com.siku.reader', 'siku.db');
const filter = (process.argv[2] || '').trim();

if (!existsSync(DB_PATH)) {
  console.error('未找到数据库:', DB_PATH);
  console.error('可用环境变量 SIKU_DB 指定路径。');
  process.exit(1);
}

const db = new DatabaseSync(DB_PATH, { readOnly: true });
const q = (sql, ...args) => db.prepare(sql).all(...args);

// ── 概览模式 ──
if (!filter) {
  console.log('=== 文献分块概览 ===\n');
  const rows = q(
    `SELECT p.id, p.title, p.page_count, COUNT(c.id) n
     FROM papers p LEFT JOIN chunks c ON c.paper_id = p.id
     GROUP BY p.id ORDER BY n DESC`
  );
  for (const r of rows) {
    console.log(
      `${String(r.n).padStart(4)} 块 | ${String(r.page_count ?? '?').padStart(3)} 页 | ${(r.title || '').slice(0, 55)}`
    );
    console.log(`      id: ${r.id}`);
  }
  console.log('\n用法: node --experimental-sqlite view-chunks.mjs "<标题关键词 或 id 前缀>"');
  process.exit(0);
}

// ── 明细模式 ──
const papers = q(
  `SELECT id, title, page_count FROM papers
   WHERE id LIKE ? OR title LIKE ? ORDER BY title`,
  `%${filter}%`,
  `%${filter}%`
);
if (papers.length === 0) {
  console.log('未找到匹配的文献');
  process.exit(0);
}

for (const p of papers) {
  console.log(`\n${'='.repeat(78)}`);
  console.log(`📄 ${p.title}  (${p.page_count ?? '?'} 页, id: ${p.id})`);
  console.log('='.repeat(78));
  const chunks = q(
    `SELECT chunk_index, page_start, page_end, section, token_count, content
     FROM chunks WHERE paper_id = ? ORDER BY chunk_index`,
    p.id
  );
  if (chunks.length === 0) {
    console.log('  （该文献暂无分块，可在图书馆右键「重建索引」）');
    continue;
  }
  for (const c of chunks) {
    const section = c.section ? ` | 章节: ${c.section}` : '';
    console.log(
      `\n── 块 ${c.chunk_index} | 页 ${c.page_start ?? '?'}-${c.page_end ?? '?'} | ~${c.token_count ?? '?'} tokens${section} ──`
    );
    console.log(c.content);
  }
}
