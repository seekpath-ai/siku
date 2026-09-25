/**
 * Agent tool definitions grouped by category, shared by the create dialog,
 * the per-agent config panel, and the default-tool selection.
 */

export interface AgentToolDef {
  key: string;
  label: string;
}

export interface AgentToolCategory {
  name: string;
  tools: AgentToolDef[];
}

export const TOOL_CATEGORIES: AgentToolCategory[] = [
  {
    name: '文献管理',
    tools: [
      { key: 'paper_search', label: 'paper_search — 搜索论文' },
      { key: 'paper_read', label: 'paper_read — 读取论文' },
      { key: 'paper_snapshot', label: 'paper_snapshot — 截取论文图/表' },
      { key: 'paper_import', label: 'paper_import — 导入论文' },
      { key: 'search_library', label: 'search_library — 全文检索图书馆' },
    ],
  },
  {
    name: '笔记',
    tools: [
      { key: 'note_read', label: 'note_read — 读取笔记' },
      { key: 'note_write', label: 'note_write — 写入笔记' },
    ],
  },
  {
    name: '知识库',
    tools: [
      { key: 'knowledge_query', label: 'knowledge_query — 查询知识' },
      { key: 'knowledge_create', label: 'knowledge_create — 写入知识' },
    ],
  },
  {
    name: '长期记忆',
    tools: [
      { key: 'memory_read', label: 'memory_read — 读取长期记忆' },
      { key: 'memory_write', label: 'memory_write — 写入长期记忆（追加/重写）' },
    ],
  },
  {
    name: '网络',
    tools: [
      { key: 'web_fetch', label: 'web_fetch — 抓取网页' },
      { key: 'web_search', label: 'web_search — 搜索网页' },
    ],
  },
  {
    name: '翻译',
    tools: [{ key: 'translate', label: 'translate — 翻译' }],
  },
  {
    name: '文件',
    tools: [
      { key: 'file_read', label: 'file_read — 读取文件' },
      { key: 'file_list', label: 'file_list — 列出文件' },
      { key: 'file_grep', label: 'file_grep — 项目内搜索文本' },
      { key: 'file_glob', label: 'file_glob — 按模式列文件' },
      { key: 'file_write', label: 'file_write — 写文件' },
      { key: 'file_edit', label: 'file_edit — 编辑文件' },
    ],
  },
  {
    name: 'Shell 与任务',
    tools: [
      { key: 'bash', label: 'bash — 执行命令' },
      { key: 'task_list', label: 'task_list — 列出后台任务' },
      { key: 'task_output', label: 'task_output — 查看任务输出' },
      { key: 'task_stop', label: 'task_stop — 停止任务' },
    ],
  },
  {
    name: '交互与系统',
    tools: [
      { key: 'ask_user', label: 'ask_user — 向用户提问' },
      { key: 'read_media_file', label: 'read_media_file — 图片理解（需多模态模型）' },
    ],
  },
];

/** All built-in tool keys (新智能体默认全选). */
export const ALL_TOOL_KEYS: string[] = TOOL_CATEGORIES.flatMap((c) =>
  c.tools.map((t) => t.key)
);

export const DEFAULT_TOOLS: string[] = [...ALL_TOOL_KEYS];

// ── Approval previews for the file-writing tools ───────────────────────────
// `file_write` / `file_edit` carry raw file content in their arguments, so the
// generic `JSON.stringify` display the approval card falls back to hides
// exactly what the user must read before approving (escaped `\n` turns a
// multi-line file into one unreadable wall of text). These helpers build a
// bounded, human-readable preview instead.

/** Write-preview budget. Bounded because the card prints it in a scrollable
 *  monospace block: a 5000-line file must not push the buttons off-screen. */
const WRITE_PREVIEW_LINES = 20;
const WRITE_PREVIEW_CHARS = 1200;
/** An edit shows two previews (old + new), so each gets half that budget. */
const EDIT_PREVIEW_LINES = 12;
const EDIT_PREVIEW_CHARS = 600;

interface PreviewSlice {
  text: string;
  /** Lines the whole value has (see `lineCount`). */
  totalLines: number;
  /** Lines actually shown. */
  shownLines: number;
  /** A budget ran out — the caller must not claim it shows everything. */
  truncated: boolean;
}

/** Line count matching the backend's `str::lines()`: a trailing newline does
 *  not open a new line, and empty content has none. Kept in step so the preview
 *  and the `file_write` result never disagree about a file's size. */
function lineCount(content: string): number {
  if (content === '') return 0;
  return content.split('\n').length - (content.endsWith('\n') ? 1 : 0);
}

/** First `maxLines` lines of `content`, hard-capped at `maxChars` code points
 *  (code points, so a surrogate pair is never split). Whichever budget runs
 *  out first wins; the trailing ellipsis keeps a mid-line cut visible. */
function slicePreview(content: string, maxLines: number, maxChars: number): PreviewSlice {
  const lines = content.split('\n');
  let text = lines.slice(0, maxLines).join('\n');
  let truncated = lines.slice(0, maxLines).length < lines.length;
  const chars = [...text];
  if (chars.length > maxChars) {
    text = `${chars.slice(0, Math.max(0, maxChars - 1)).join('')}…`;
    truncated = true;
  }
  const totalLines = lineCount(content);
  const shownLines = Math.min(lineCount(text), totalLines);
  return { text, totalLines, shownLines, truncated: truncated || shownLines < totalLines };
}

/** Display form of a value that is not a string (number, array, object…). */
function rawValueText(value: unknown): string {
  if (typeof value === 'string') return value;
  try {
    return JSON.stringify(value) ?? String(value);
  } catch {
    return String(value);
  }
}

/** Display the arguments verbatim when there is nothing previewable. Wrapped
 *  because `JSON.stringify` throws on cyclic values — the card must render. */
function argsFallbackText(args: Record<string, unknown>): string {
  try {
    return JSON.stringify(args) ?? String(args);
  } catch {
    return '（无法显示工具参数）';
  }
}

function readPathArg(args: Record<string, unknown>): string | null {
  const path = args.path;
  return typeof path === 'string' && path.trim() !== '' ? path : null;
}

/** What `mode` actually does to the target file, in words.
 *
 *  Mirrors the backend's `resolve_mode`: an absent, `null` or blank `mode` is
 *  NOT an overwrite — the backend refuses to clobber an existing non-empty file
 *  unless `overwrite` is explicit. Anything else is rejected at execution, so
 *  the card says so instead of implying a safe write. */
function describeWriteIntent(mode: unknown): string {
  if (mode === undefined || mode === null) return '新建（缺省；目标已存在且非空时会失败）';
  if (typeof mode === 'string') {
    const trimmed = mode.trim();
    if (trimmed === '') return '新建（缺省；目标已存在且非空时会失败）';
    if (trimmed === 'overwrite') return '整文件覆盖';
    if (trimmed === 'append') return '追加到文件末尾';
    return `${mode}（未知取值（后端会拒绝））`;
  }
  return `${rawValueText(mode)}（未知取值（后端会拒绝））`;
}

function formatFileWritePreview(args: Record<string, unknown>): string {
  const path = readPathArg(args);
  if (path === null) return argsFallbackText(args);
  const out = [`file_write → ${path}`, `意图：${describeWriteIntent(args.mode)}`];
  const content = typeof args.content === 'string' ? args.content : null;
  if (content === null) {
    out.push('（缺少 content 参数（应为字符串）；后端会拒绝本次调用）');
    return out.join('\n');
  }
  // UTF-8 byte count, matching what the backend reports and writes.
  const byteCount = new TextEncoder().encode(content).length;
  const { text, totalLines, shownLines, truncated } = slicePreview(
    content,
    WRITE_PREVIEW_LINES,
    WRITE_PREVIEW_CHARS
  );
  out.push(
    truncated
      ? `共 ${totalLines} 行 / ${byteCount} 字节，预览前 ${shownLines} 行：`
      : `共 ${totalLines} 行 / ${byteCount} 字节（全部内容）：`
  );
  out.push(text === '' ? '（空内容）' : text);
  const hiddenLines = Math.max(0, totalLines - shownLines);
  if (hiddenLines > 0) out.push(`…（还有 ${hiddenLines} 行未显示）`);
  return out.join('\n');
}

/** One side of a `file_edit` diff preview. */
function formatEditSide(value: string): string {
  if (value === '') return '（空字符串）';
  const { text, totalLines, shownLines } = slicePreview(value, EDIT_PREVIEW_LINES, EDIT_PREVIEW_CHARS);
  const hiddenLines = Math.max(0, totalLines - shownLines);
  return hiddenLines > 0 ? `${text}\n…（还有 ${hiddenLines} 行未显示）` : text;
}

function formatFileEditPreview(args: Record<string, unknown>): string {
  const path = readPathArg(args);
  if (path === null) return argsFallbackText(args);
  const oldString = typeof args.old_string === 'string' ? args.old_string : null;
  const newString = typeof args.new_string === 'string' ? args.new_string : null;
  return [
    `file_edit → ${path}`,
    // `replace_all` absent/false means the backend needs the match to be
    // unique — the user should know that before approving.
    `替换 ${args.replace_all === true ? '全部匹配' : '首个唯一匹配'}`,
    '- 原文本：',
    oldString === null ? '（缺少 old_string 参数（应为字符串）；后端会拒绝本次调用）' : formatEditSide(oldString),
    '+ 新文本：',
    newString === null ? '（缺少 new_string 参数（应为字符串）；后端会拒绝本次调用）' : formatEditSide(newString),
  ].join('\n');
}

/**
 * Readable one-shot preview of a `file_write` / `file_edit` call for the
 * approval card, or `null` for every other tool — the caller then keeps its own
 * generic display. Never throws: arguments it cannot preview degrade to a JSON
 * dump. `args` is expected to already have the backend-injected
 * pseudo-arguments (`_working_dir`) stripped.
 */
export function formatFileToolApproval(
  toolName: string,
  args: Record<string, unknown>
): string | null {
  if (toolName === 'file_write') return formatFileWritePreview(args);
  if (toolName === 'file_edit') return formatFileEditPreview(args);
  return null;
}
