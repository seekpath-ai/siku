import type { AgentStep, ChatMessage } from './types';
import { parseToolCalls } from './agentPhases';

/** Structural view of the turn-context snapshot (matches tauri.ts
 *  TurnContext without pulling the invoke module into this pure lib). */
export interface TurnContextSnapshot {
  system_prompt: string;
  /** JSON array [{role, content, time}]; null for old snapshots. */
  history: string | null;
}

/** One turn's body: user message, reasoning chain, reply. `h` is the heading
 *  prefix (`##` for a standalone turn export, `###` inside a session export). */
function turnBody(
  userMessage: ChatMessage | undefined,
  assistantMessage: ChatMessage,
  steps: AgentStep[],
  h: string
): string[] {
  const lines: string[] = [];
  lines.push(`${h} 用户消息`, '');
  lines.push(userMessage?.content.trim() || '（未找到本轮用户消息）', '');

  const sorted = [...steps].sort((a, b) => a.step_index - b.step_index);
  if (sorted.length > 0) {
    lines.push(`${h} 推理过程`, '');
    for (const step of sorted) {
      const reasoning = step.reasoning_content?.trim();
      const calls = parseToolCalls(step.tool_calls);
      if (!reasoning && calls.length === 0) continue;
      lines.push(`**第 ${step.step_index} 轮**`, '');
      if (reasoning) {
        lines.push('*思考*', '', reasoning, '');
      }
      for (const call of calls) {
        lines.push(`*工具调用* \`${call.name}\``, '');
        lines.push('```json', JSON.stringify(call.arguments, null, 2), '```', '');
        if (call.result) {
          lines.push('*结果*', '', '```', call.result, '```', '');
        }
        if (call.status && call.status !== 'completed') {
          lines.push(`（状态：${call.status}）`, '');
        }
      }
    }
  }

  lines.push(`${h} 助手回复`, '');
  lines.push(assistantMessage.content.trim() || '（空）', '');
  return lines;
}

function turnContextSection(snapshot: TurnContextSnapshot): string[] {
  const lines: string[] = [];
  lines.push('---', '', '## 本轮上下文快照', '');
  lines.push('### 系统提示词', '', '```', snapshot.system_prompt, '```', '');
  if (snapshot.history) {
    lines.push('### 历史消息', '');
    try {
      const entries = JSON.parse(snapshot.history) as {
        role: string;
        content: string;
        time?: string;
      }[];
      for (const e of entries) {
        lines.push(`**[${e.role}]**${e.time ? ` ${e.time}` : ''}`, '', e.content, '');
      }
    } catch {
      lines.push('```json', snapshot.history, '```', '');
    }
  }
  return lines;
}

/** Build the troubleshooting markdown for one conversation turn: the user
 * message, the assistant's reasoning chain (per round, with tool calls), and
 * the final reply — optionally plus the turn-context snapshot. Pure string
 * assembly — no I/O, nothing synced. */
export function buildTurnMarkdown({
  agentName,
  userMessage,
  assistantMessage,
  steps,
  turnContext,
}: {
  agentName: string;
  userMessage?: ChatMessage;
  assistantMessage: ChatMessage;
  steps: AgentStep[];
  turnContext?: TurnContextSnapshot | null;
}): string {
  const lines: string[] = [];
  lines.push('# 思库对话导出', '');
  lines.push(`- 导出时间：${new Date().toLocaleString('zh-CN', { hour12: false })}`);
  lines.push(`- 智能体：${agentName}`);
  if (assistantMessage.model) lines.push(`- 模型：${assistantMessage.model}`);
  if (userMessage) lines.push(`- 用户消息时间：${userMessage.created_at}`);
  lines.push(`- 助手消息时间：${assistantMessage.created_at}`);
  if (assistantMessage.tokens_in != null || assistantMessage.tokens_out != null) {
    lines.push(
      `- Token：输入 ${assistantMessage.tokens_in ?? '-'} / 输出 ${assistantMessage.tokens_out ?? '-'}`
    );
  }

  lines.push('', '---', '');
  lines.push(...turnBody(userMessage, assistantMessage, steps, '##'));
  if (turnContext) lines.push(...turnContextSection(turnContext));
  return lines.join('\n');
}

/** Whole-session export: every turn (user → reasoning → reply) in order.
 *  A trailing user message with no assistant reply is kept at the end. */
export function buildSessionMarkdown({
  sessionTitle,
  messages,
  steps,
}: {
  sessionTitle: string;
  messages: ChatMessage[];
  steps: AgentStep[];
}): string {
  const stepsByMessageId = new Map<string, AgentStep[]>();
  for (const s of steps) {
    if (!s.message_id) continue;
    const list = stepsByMessageId.get(s.message_id) ?? [];
    list.push(s);
    stepsByMessageId.set(s.message_id, list);
  }

  const lines: string[] = [];
  lines.push('# 思库会话导出', '');
  lines.push(`- 导出时间：${new Date().toLocaleString('zh-CN', { hour12: false })}`);
  lines.push(`- 会话：${sessionTitle}`);
  lines.push(`- 消息数：${messages.length}`, '');

  let lastUser: ChatMessage | undefined;
  let turnNo = 0;
  for (const m of messages) {
    if (m.role === 'user') {
      lastUser = m;
      continue;
    }
    if (m.role !== 'assistant') continue;
    turnNo++;
    lines.push('---', '', `## 第 ${turnNo} 轮`, '');
    lines.push(`- 用户消息时间：${lastUser?.created_at ?? '-'}`);
    lines.push(`- 助手消息时间：${m.created_at}`);
    if (m.model) lines.push(`- 模型：${m.model}`);
    if (m.tokens_in != null || m.tokens_out != null) {
      lines.push(`- Token：输入 ${m.tokens_in ?? '-'} / 输出 ${m.tokens_out ?? '-'}`);
    }
    lines.push('');
    lines.push(...turnBody(lastUser, m, stepsByMessageId.get(m.id) ?? [], '###'));
    lastUser = undefined;
  }
  if (lastUser) {
    lines.push('---', '', '## 未回复的用户消息', '', lastUser.content.trim(), '');
  }
  return lines.join('\n');
}

function fileStamp(): string {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;
}

function safeFilePart(s: string): string {
  return s.replace(/[\\/:*?"<>|]/g, '_').trim() || 'agent';
}

/** Default turn export file name: siku-对话-<agent>-<yyyymmdd-hhmmss>.md */
export function turnExportFileName(agentName: string): string {
  return `siku-对话-${safeFilePart(agentName)}-${fileStamp()}.md`;
}

/** Default session export file name: siku-会话-<title>-<yyyymmdd-hhmmss>.md */
export function sessionExportFileName(sessionTitle: string): string {
  return `siku-会话-${safeFilePart(sessionTitle)}-${fileStamp()}.md`;
}
