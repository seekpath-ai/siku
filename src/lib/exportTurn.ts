import type { AgentStep, ChatMessage } from './types';
import { parseToolCalls } from './agentPhases';

/** Build the troubleshooting markdown for one conversation turn: the user
 * message, the assistant's reasoning chain (per round, with tool calls), and
 * the final reply. Pure string assembly — no I/O, nothing synced. */
export function buildTurnMarkdown({
  agentName,
  userMessage,
  assistantMessage,
  steps,
}: {
  agentName: string;
  userMessage?: ChatMessage;
  assistantMessage: ChatMessage;
  steps: AgentStep[];
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

  lines.push('', '---', '', '## 用户消息', '');
  lines.push(userMessage?.content.trim() || '（未找到本轮用户消息）');
  lines.push('');

  const sorted = [...steps].sort((a, b) => a.step_index - b.step_index);
  if (sorted.length > 0) {
    lines.push('---', '', '## 推理过程', '');
    for (const step of sorted) {
      const reasoning = step.reasoning_content?.trim();
      const calls = parseToolCalls(step.tool_calls);
      if (!reasoning && calls.length === 0) continue;
      lines.push(`### 第 ${step.step_index} 轮`, '');
      if (reasoning) {
        lines.push('**思考**', '', reasoning, '');
      }
      for (const call of calls) {
        lines.push(`**工具调用** \`${call.name}\``, '');
        lines.push('```json', JSON.stringify(call.arguments, null, 2), '```', '');
        if (call.result) {
          lines.push('**结果**', '', '```', call.result, '```', '');
        }
        if (call.status && call.status !== 'completed') {
          lines.push(`（状态：${call.status}）`, '');
        }
      }
    }
  }

  lines.push('---', '', '## 助手回复', '');
  lines.push(assistantMessage.content.trim() || '（空）', '');
  return lines.join('\n');
}

/** Default export file name: siku-对话-<agent>-<yyyymmdd-hhmmss>.md */
export function turnExportFileName(agentName: string): string {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, '0');
  const stamp = `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;
  const safe = agentName.replace(/[\\/:*?"<>|]/g, '_').trim() || 'agent';
  return `siku-对话-${safe}-${stamp}.md`;
}
