import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';
import rehypeKatex from 'rehype-katex';
import { User, Copy, Check, Paperclip, ChevronDown, ChevronUp } from 'lucide-react';
import { useMemo, useState } from 'react';
import type { AgentPhase, AgentStep, ChatMessage, ToolCallInfo } from '@/lib/types';
import { parseAttachments } from '@/lib/attachments';
import { useActiveAgentName } from '@/hooks/useActiveAgentName';
import { MarkdownCode, MarkdownPre } from './CodeBlock';
import { ToolCallCard } from './ToolCallCard';
import { ReasoningProcessCard } from './ReasoningProcessCard';
import { ReasoningBlock } from './ReasoningBlock';
import { ExternalLink } from '@/components/ui/ExternalLink';

interface Props {
  message: ChatMessage;
  agentSteps?: AgentStep[];
}

function formatTime(iso: string): string {
  try {
    return new Date(iso).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
  } catch {
    return '';
  }
}

function formatTokenUsage(message: ChatMessage): string {
  if (message.tokens_in != null && message.tokens_out != null) {
    const hit = message.tokens_in_hit != null && message.tokens_in_hit > 0
      ? `（命中 ${message.tokens_in_hit}）`
      : '';
    return `输入 ${message.tokens_in}${hit} · 输出 ${message.tokens_out}`;
  }
  if (message.tokens_used != null && message.tokens_used > 0) {
    return `${message.tokens_used} tokens`;
  }
  return '';
}

function parseToolCalls(json: string | null): ToolCallInfo[] {
  if (!json) return [];
  try {
    const parsed = JSON.parse(json);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

interface ContentBlock {
  label: string;
  text: string;
}

/** Split a user message into its machine-generated blocks (context picker
 * output, attached text files) and the text the user actually typed. */
function extractBlocks(content: string): { blocks: ContentBlock[]; rest: string } {
  const blocks: ContentBlock[] = [];
  let rest = content;

  const ctx = rest.match(/<context>[\s\S]*?<\/context>/);
  if (ctx) {
    const names = [...ctx[0].matchAll(/# (?:笔记|文件)「([^」]*)」/g)].map((m) => m[1]);
    blocks.push({
      label: names.length > 0 ? `上下文：${names.join('、')}` : '上下文',
      text: ctx[0],
    });
    rest = rest.replace(ctx[0], '');
  }

  const fileRe = /<file name="([^"]*)"[^>]*>[\s\S]*?<\/file>/g;
  let m: RegExpExecArray | null;
  while ((m = fileRe.exec(content)) !== null) {
    blocks.push({ label: `附件「${m[1]}」`, text: m[0] });
  }
  rest = rest.replace(fileRe, '');

  return { blocks, rest: rest.trim() };
}

/** User message text: attachment/context blocks are collapsed into a
 * summary chip (click to expand); the typed text stays visible. */
function UserText({ content }: { content: string }) {
  const { blocks, rest } = useMemo(() => extractBlocks(content), [content]);
  const [expanded, setExpanded] = useState(false);

  if (blocks.length === 0) {
    return <p className="whitespace-pre-wrap">{content}</p>;
  }
  return (
    <>
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        className="inline-flex items-center gap-1.5 max-w-full px-2 py-1 mb-1.5 rounded-md bg-codex-hover/60 text-codex-muted text-[11px] hover:text-codex-primary transition-colors"
      >
        <Paperclip size={11} className="shrink-0" />
        <span className="truncate">{blocks.map((b) => b.label).join(' · ')}</span>
        {expanded ? <ChevronUp size={11} className="shrink-0" /> : <ChevronDown size={11} className="shrink-0" />}
      </button>
      {expanded && (
        <pre className="whitespace-pre-wrap font-mono text-[11px] leading-relaxed text-codex-muted bg-codex-bg/60 border border-codex-border/50 rounded-md p-2.5 mb-1.5 max-h-[320px] overflow-y-auto">
          {blocks.map((b) => b.text).join('\n\n')}
        </pre>
      )}
      {rest && <p className="whitespace-pre-wrap">{rest}</p>}
    </>
  );
}

function stepsToPhases(steps: AgentStep[]): AgentPhase[] {
  const phases: AgentPhase[] = [];
  for (const step of steps) {
    if (step.reasoning_content?.trim()) {
      phases.push({ kind: 'reasoning', step_index: step.step_index, content: step.reasoning_content });
    }
    for (const tc of parseToolCalls(step.tool_calls)) {
      phases.push({ kind: 'tool_call', step_index: step.step_index, toolCall: tc });
    }
  }
  return phases;
}

export function MessageBubble({ message, agentSteps = [] }: Props) {
  const agentName = useActiveAgentName();
  const [copied, setCopied] = useState(false);
  const isUser = message.role === 'user';
  const isSystem = message.role === 'system';
  const isTool = message.role === 'tool';
  const toolCalls = parseToolCalls(message.tool_calls);
  const hasLegacyReasoning = !!message.reasoning_content;
  const hasLegacyToolCalls = toolCalls.length > 0;
  const hasSteps = agentSteps.length > 0;

  if (isSystem || isTool) return null;

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(message.content);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch { /* ignore */ }
  };

  const avatar = (
    <div
      className={`w-8 h-8 rounded-full shrink-0 flex items-center justify-center text-[14px] font-semibold ${
        isUser
          ? 'bg-codex-surface border border-codex-border text-codex-primary'
          : 'bg-gradient-to-br from-codex-accent to-emerald-700 text-black'
      }`}
    >
      {isUser ? <User size={16} /> : 'S'}
    </div>
  );

  const content = (
    <div className={`flex-1 min-w-0 pt-1 ${isUser ? 'text-right' : 'text-left'}`}>
      {isUser && (
        <div className="flex items-center justify-end gap-1 text-[11px] text-codex-muted mb-1">
          <span>{formatTime(message.created_at)}</span>
          <button
            onClick={handleCopy}
            className="opacity-0 group-hover/bubble:opacity-100 transition-opacity p-0.5 rounded hover:bg-codex-hover text-codex-muted hover:text-codex-primary"
            title="复制"
          >
            {copied ? <Check size={11} className="text-codex-accent" /> : <Copy size={11} />}
          </button>
        </div>
      )}

      {!isUser && (
        <div className="flex items-center gap-2 mb-1 flex-wrap">
          <span className="text-[13px] font-semibold text-codex-primary">{agentName}</span>
          {message.model && (
            <span className="text-[10px] px-1 py-px rounded bg-codex-surface border border-codex-border text-codex-muted truncate max-w-[200px]">
              {message.model}
            </span>
          )}
          <span className="text-[11px] text-codex-muted">{formatTime(message.created_at)}</span>
          <button
            onClick={handleCopy}
            className="opacity-0 group-hover/bubble:opacity-100 transition-opacity p-0.5 rounded hover:bg-codex-hover text-codex-muted hover:text-codex-primary"
            title="复制"
          >
            {copied ? <Check size={11} className="text-codex-accent" /> : <Copy size={11} />}
          </button>
        </div>
      )}

      {!isUser && hasSteps && (
        <div className="mb-2">
          <ReasoningProcessCard phases={stepsToPhases(agentSteps)} />
        </div>
      )}

      {!isUser && !hasSteps && hasLegacyReasoning && message.reasoning_content && (
        <ReasoningBlock content={message.reasoning_content} />
      )}

      <div
        className={`relative inline-block text-left max-w-[85%] rounded-2xl px-4 py-2.5 text-[14px] leading-relaxed ${
          isUser
            ? 'bg-codex-surface border border-codex-border text-codex-primary'
            : 'bg-codex-surface/60 border border-codex-border/60 text-codex-primary'
        }`}
      >
        {/* WeChat-style tail pointing toward the user's avatar */}
        {isUser && (
          <span className="absolute -right-[7px] top-3 h-0 w-0 border-y-[6px] border-l-[8px] border-y-transparent border-l-codex-surface" />
        )}
        {isUser ? (
          <>
            <UserText content={message.content} />
            {message.attachments && (
              <div className="flex flex-wrap gap-2 mt-2">
                {parseAttachments(message.attachments).map((att, idx) => (
                  <a
                    key={idx}
                    href={`data:${att.mime};base64,${att.base64}`}
                    target="_blank"
                    rel="noreferrer"
                    className="block"
                  >
                    <img
                      src={`data:${att.mime};base64,${att.base64}`}
                      alt={att.name || `图片 ${idx + 1}`}
                      className="max-w-[120px] max-h-[120px] object-cover rounded-lg border border-codex-border/50"
                    />
                  </a>
                ))}
              </div>
            )}
          </>
        ) : (
          <div className="prose prose-sm prose-invert max-w-none [&>*:first-child]:mt-0">
            <ReactMarkdown
              remarkPlugins={[remarkGfm, remarkMath]}
              rehypePlugins={[[rehypeKatex, { throwOnError: false }]]}
              components={{
                a: ExternalLink,
                code: MarkdownCode,
                pre: MarkdownPre,
              }}
            >
              {message.content}
            </ReactMarkdown>
          </div>
        )}

        {!isUser && (message.tokens_in != null || message.tokens_out != null || message.tokens_used != null) && (
          <div className="text-right mt-2 text-[10px] text-codex-muted select-none">
            {formatTokenUsage(message)}
          </div>
        )}
      </div>

      {/* Copy button under the assistant's bubble */}
      {!isUser && (
        <div className="flex items-center gap-1 mt-1 opacity-0 group-hover/bubble:opacity-100 transition-opacity">
          <button
            onClick={handleCopy}
            className="p-0.5 rounded hover:bg-codex-hover text-codex-muted hover:text-codex-primary"
            title="复制"
          >
            {copied ? <Check size={11} className="text-codex-accent" /> : <Copy size={11} />}
          </button>
        </div>
      )}

      {!isUser && !hasSteps && hasLegacyToolCalls && (
        <div className="mt-3 space-y-2">
          {toolCalls.map((tc) => (
            <ToolCallCard key={tc.id} toolCall={tc} />
          ))}
        </div>
      )}
    </div>
  );

  return (
    <div
      className={`flex gap-3 group/bubble animate-in fade-in slide-in-from-bottom-1.5 duration-200 ${
        isUser ? 'flex-row-reverse' : 'flex-row'
      }`}
    >
      {avatar}
      {content}
    </div>
  );
}
