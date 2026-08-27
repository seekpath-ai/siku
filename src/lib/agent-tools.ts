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
      { key: 'paper_import', label: 'paper_import — 导入论文' },
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
