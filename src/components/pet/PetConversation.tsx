import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';
import rehypeKatex from 'rehype-katex';
import { listen, emit } from '@tauri-apps/api/event';
import { useNavigate } from '@tanstack/react-router';
import { Send, Loader2, ShieldAlert, Check, NotebookPen, Quote, ChevronDown, ChevronRight } from 'lucide-react';
import { usePetStore } from '@/stores/petStore';
import type { PetContext } from '@/stores/petContextStore';
import { useEvidenceStore } from '@/stores/evidenceStore';
import { useDialogStore } from '@/stores/dialogStore';
import { getChatMessages, getAgentSteps, agentApproveTool, notesCreate, noteCreateUnderPaper } from '@/lib/tauri';
import { parseEvidence, buildNoteMarkdown } from '@/lib/evidence';
import type { EvidenceEntry } from '@/lib/evidence';
import { MarkdownCode, MarkdownPre } from '@/components/chat/CodeBlock';
import { ReasoningProcessCard } from '@/components/chat/ReasoningProcessCard';
import { ExternalLink } from '@/components/ui/ExternalLink';
import type { AgentStreamEvent, AgentPhase, AgentStep, ChatMessage, StreamingStep, ToolCallInfo } from '@/lib/types';

// Flatten streaming steps into reasoning / tool-call phases (same as chat).
function streamingToPhases(steps: StreamingStep[], current: StreamingStep | null): AgentPhase[] {
  const phases: AgentPhase[] = [];
  for (const step of [...steps, ...(current ? [current] : [])]) {
    if (step.reasoning_content.trim()) {
      phases.push({ kind: 'reasoning', step_index: step.step_index, content: step.reasoning_content });
    }
    for (const tc of step.tool_calls) {
      phases.push({ kind: 'tool_call', step_index: step.step_index, toolCall: tc });
    }
  }
  return phases;
}

// Flatten PERSISTED agent steps (history) into the same phase shape, so
// earlier turns keep their tool-call cards after later turns start.
function stepsToPhases(steps: AgentStep[]): AgentPhase[] {
  const phases: AgentPhase[] = [];
  for (const step of steps) {
    if (step.reasoning_content?.trim()) {
      phases.push({ kind: 'reasoning', step_index: step.step_index, content: step.reasoning_content });
    }
    let toolCalls: ToolCallInfo[] = [];
    try {
      toolCalls = step.tool_calls ? JSON.parse(step.tool_calls) : [];
    } catch {
      toolCalls = [];
    }
    for (const tc of toolCalls) {
      phases.push({ kind: 'tool_call', step_index: step.step_index, toolCall: tc });
    }
  }
  return phases;
}

// ── Evidence citations ([^n] → clickable badge that highlights the PDF) ──
// Parsing/conversion helpers live in @/lib/evidence (shared with notes).

/** Structural subset of mdast nodes (mdast is not a direct dependency). */
interface MdastNode {
  type: string;
  value?: string;
  url?: string;
  children?: MdastNode[];
}

/** remark plugin: turn [^n] text markers into `cite:n` links so the renderer
 *  can swap them for clickable evidence badges. Code spans/blocks hold their
 *  text in `value` (not text children), so they are naturally excluded. */
function remarkCitations() {
  return (tree: MdastNode) => {
    const walk = (node: MdastNode) => {
      if (!Array.isArray(node.children)) return;
      const out: MdastNode[] = [];
      for (const child of node.children) {
        if (child.type === 'text' && child.value && /\[\^\d+\]/.test(child.value)) {
          for (const part of child.value.split(/(\[\^\d+\])/)) {
            const m = /^\[\^(\d+)\]$/.exec(part);
            if (m) {
              out.push({ type: 'link', url: `cite:${m[1]}`, children: [{ type: 'text', value: m[1] }] });
            } else if (part) {
              out.push({ type: 'text', value: part });
            }
          }
        } else {
          walk(child);
          out.push(child);
        }
      }
      node.children = out;
    };
    walk(tree);
  };
}

/** Shared markdown renderer for pet messages: markdown + math + evidence
 *  citation badges. (Exported for tests.) */
export function PetMarkdown({ content, onCitation }: { content: string; onCitation?: (ev: EvidenceEntry) => void }) {
  const { clean, evidence } = useMemo(() => parseEvidence(content), [content]);
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm, remarkMath, remarkCitations]}
      rehypePlugins={[[rehypeKatex, { throwOnError: false }]]}
      // Keep our internal cite: scheme (the default transform blanks
      // non-whitelisted protocols). Non-http(s)/anchor links are defused in
      // the `a` renderer below, so this is safe.
      urlTransform={(url) => url}
      components={{
        a: ({ href, children }: { href?: string; children?: React.ReactNode }) => {
          const cite = /^cite:(\d+)$/.exec(href ?? '');
          if (cite) {
            const n = Number(cite[1]);
            const ev = evidence.get(n);
            const clickable = !!ev && !!onCitation;
            return (
              <button
                type="button"
                // Tooltip shows the quoted evidence itself, so a mismatched
                // citation is diagnosable at a glance (quote content vs
                // highlight position).
                title={ev
                  ? `${ev.page != null ? `第 ${ev.page} 页 · ` : ''}${ev.exact.length > 120 ? `${ev.exact.slice(0, 120)}…` : ev.exact}`
                  : '未找到该引用的证据'}
                disabled={!clickable}
                onClick={() => ev && onCitation?.(ev)}
                className={`inline-block align-super mx-0.5 px-1 rounded text-[10px] leading-4 ${
                  clickable
                    ? 'bg-primary/15 text-primary hover:bg-primary/30 cursor-pointer'
                    : 'bg-surface-hover text-text-secondary/60 cursor-default'
                }`}
              >
                {n}
              </button>
            );
          }
          // Defuse javascript:/data: etc. — only web links, mailto and
          // in-page anchors stay anchors.
          if (href && !/^(https?:|mailto:|#)/.test(href)) {
            return <span>{children}</span>;
          }
          return <ExternalLink href={href}>{children}</ExternalLink>;
        },
        code: MarkdownCode,
        pre: MarkdownPre,
      }}
    >
      {clean}
    </ReactMarkdown>
  );
}

// ── Message rendering (lightweight, matches chat styles) ──

/** Collapsible card listing all evidence citations of a reply: the parsed
 *  entries (page + quote) as clickable rows that highlight the PDF passage —
 *  the audit view the per-badge tooltips can't give (all evidence at a
 *  glance). Raw JSON stays hidden; this renders the parsed form. */
function EvidenceListCard({ evidence, onCitation }: {
  evidence: Map<number, EvidenceEntry>;
  onCitation?: (ev: EvidenceEntry) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const entries = [...evidence.entries()].sort((a, b) => a[0] - b[0]);
  return (
    <div className="not-prose mt-1.5 rounded-lg border border-codex-border/70 bg-codex-surface/30 overflow-hidden">
      <button
        type="button"
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-1.5 w-full px-2.5 py-1.5 text-left hover:bg-codex-hover/40 transition-colors"
      >
        <Quote size={12} className="text-codex-accent" />
        <span className="text-[11px] font-medium text-codex-secondary">
          证据引用 · {entries.length} 条
        </span>
        <span className="ml-auto">
          {expanded ? (
            <ChevronDown size={12} className="text-codex-muted" />
          ) : (
            <ChevronRight size={12} className="text-codex-muted" />
          )}
        </span>
      </button>
      {expanded && (
        <div className="px-2.5 py-1.5 border-t border-codex-border/50 space-y-1">
          {entries.map(([n, ev]) => (
            <button
              key={n}
              type="button"
              disabled={!onCitation}
              onClick={() => onCitation?.(ev)}
              title="在 PDF 中定位并高亮该证据"
              className="flex items-start gap-1.5 w-full text-left rounded px-1 py-0.5 hover:bg-codex-hover/40 transition-colors disabled:cursor-default"
            >
              <span className="shrink-0 inline-block px-1 rounded text-[10px] leading-4 bg-primary/15 text-primary">
                {n}
              </span>
              {ev.page != null && (
                <span className="shrink-0 text-[10px] leading-4 text-text-secondary/60">
                  第 {ev.page} 页
                </span>
              )}
              <span className="text-[11px] leading-4 text-text-secondary/80 line-clamp-2">
                「{ev.exact}」
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function PetMessage({ msg, onCitation, onSaveNote, saving }: {
  msg: ChatMessage;
  onCitation?: (ev: EvidenceEntry) => void;
  onSaveNote?: (msg: ChatMessage) => void;
  saving?: boolean;
}) {
  // Parsed once here for the audit card; PetMarkdown parses internally for
  // the citation badges (cheap: one regex + one JSON.parse per render).
  const evidence = useMemo(() => parseEvidence(msg.content).evidence, [msg.content]);
  if (msg.role === 'user') {
    return (
      <div className="flex justify-end">
        <div className="max-w-[82%] bg-surface px-3 py-2 rounded-xl rounded-tr-sm text-[13px] text-text-primary whitespace-pre-wrap">
          {msg.content}
        </div>
      </div>
    );
  }
  return (
    <div className="flex justify-start">
      <div className="max-w-[92%] prose prose-sm prose-invert max-w-none [&>*:first-child]:mt-0">
        <PetMarkdown content={msg.content} onCitation={onCitation} />
        {evidence.size > 0 && (
          <EvidenceListCard evidence={evidence} onCitation={onCitation} />
        )}
        {/* Deterministic save: converts evidence citations to footnotes with
            deep links instead of relying on the model to call note_write. */}
        {onSaveNote && msg.content.trim() && (
          <button
            type="button"
            disabled={saving}
            onClick={() => onSaveNote(msg)}
            className="mt-1 inline-flex items-center gap-1 text-[11px] text-text-secondary/50 hover:text-primary transition-colors disabled:opacity-40"
          >
            {saving ? <Loader2 size={11} className="animate-spin" /> : <NotebookPen size={11} />}
            存为笔记
          </button>
        )}
      </div>
    </div>
  );
}

// ── Domain agent names + quick commands per page ──

// Keyed by session.domain (built-in agent id).
export const DOMAIN_ID_NAMES: Record<string, string> = {
  note_organizer: '笔记整理',
  literature_analyzer: '文献分析',
  research_tracker: '科研追踪',
  knowledge_curator: '知识库整理',
  chat_summarizer: '对话总结',
};

// Keyed by page type (context.page) — fallback when no session yet.
export const DOMAIN_NAMES: Record<string, string> = {
  notes: '笔记整理',
  library: '文献分析',
  reader: '文献分析',
  research: '科研追踪',
  knowledge: '知识库整理',
  chat: '对话总结',
};

const QUICK_COMMANDS: Record<string, string[]> = {
  notes: ['整理这篇笔记', '提炼要点', '补充标签', '检查错别字'],
  library: ['总结这篇文献', '提炼要点', '翻译摘要', '保存总结到笔记'],
  reader: ['解释当前段落', '总结这篇文献', '提炼核心观点'],
  research: ['总结课题进展', '发现相关文献', '梳理研究现状'],
  knowledge: ['整理当前条目', '提炼要点', '补充标签'],
  chat: ['总结对话', '提炼行动项'],
};

/** Quick command that depends on the LIVE text selection in the main
 *  window's reader — meaningless in a detached window. */
const SELECTION_COMMAND = '解释当前段落';

interface PetConversationProps {
  /** Context chip + quick commands. Null shows the generic empty state. */
  context: PetContext | null;
  /** False in a detached window: hide actions that need the live selection. */
  liveSelection?: boolean;
}

/** The pet conversation content: message list, streaming, approval, quick
 *  commands and the input box. Used both by the floating panel in the main
 *  window (Pet) and by the standalone PetChatWindow. Session state lives in
 *  the backend and `agent:event` is broadcast to every webview, so multiple
 *  windows can attach to the same session and stay in sync. */
export function PetConversation({ context, liveSelection = true }: PetConversationProps) {
  const store = usePetStore();
  const [input, setInput] = useState('');
  const navigate = useNavigate();

  // Citation badge click: highlight the quoted evidence in the reader. The
  // paper id comes from the session context (reader/library pages); in a
  // detached window the request is forwarded to the main window.
  const handleCitation = useCallback((ev: EvidenceEntry) => {
    const onPaperPage = context?.page === 'reader' || context?.page === 'library';
    const paperId = onPaperPage ? context?.objectId : null;
    if (!paperId) return;
    const payload = { paperId, page: ev.page, exact: ev.exact };
    if (liveSelection) {
      useEvidenceStore.getState().requestHighlight(payload);
      navigate({ to: '/reader/$paperId', params: { paperId } });
    } else {
      emit('pet:evidence-highlight', payload).catch(() => {});
    }
  }, [context, liveSelection, navigate]);

  // Save an assistant reply as a note. Deterministic: strips the evidence
  // block and appends GFM footnotes whose text is the quoted passage plus a
  // siku-reader:// deep link (see @/lib/evidence). Under a paper context the
  // note lands in the paper's collection folder tree (Zotero-style).
  const [savingNoteId, setSavingNoteId] = useState<string | null>(null);
  const handleSaveNote = useCallback(async (msg: ChatMessage) => {
    const onPaperPage = context?.page === 'reader' || context?.page === 'library';
    const paperId = onPaperPage ? context?.objectId : undefined;
    setSavingNoteId(msg.id);
    try {
      const markdown = buildNoteMarkdown(msg.content, paperId ?? undefined);
      const heading = /^#{1,6}\s+(.+?)\s*#*$/m.exec(parseEvidence(msg.content).clean)?.[1]?.trim();
      const title = (heading || (context?.title ? `${context.title} · 总结` : '智能体回复')).slice(0, 80);
      const note = paperId
        ? await noteCreateUnderPaper(paperId, title, markdown)
        : await notesCreate(title, markdown);
      useDialogStore.getState().alert(`已保存到笔记「${note.title}」`, '保存成功');
    } catch (err) {
      useDialogStore.getState().alert(
        `保存失败：${err instanceof Error ? err.message : String(err)}`,
        '保存到笔记'
      );
    } finally {
      setSavingNoteId(null);
    }
  }, [context]);

  // Message list auto-scroll (same behavior as the main chat panel).
  const messagesRef = useRef<HTMLDivElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const isNearBottomRef = useRef(true);
  const justOpenedRef = useRef(true);

  // Show a speech bubble next to the floating pet ball (separate window).
  const notify = useCallback(async (body: string) => {
    emit('pet:bubble', body).catch(() => {});
  }, []);

  // Stream agent events for the pet session.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    const setup = async () => {
      unlisten = await listen<AgentStreamEvent>('agent:event', (event) => {
        if (cancelled) return;
        const e = event.payload;
        const st = usePetStore.getState();
        if (!st.session || e.session_id !== st.session.id) return;
        switch (e.type) {
          case 'thinking':
            st.setStreaming(true);
            break;
          case 'delta':
            st.setStreaming(true);
            if (e.content) st.appendDelta(e.content);
            break;
          case 'reasoning': {
            st.setStreaming(true);
            if (!e.content || e.step_index === undefined) break;
            st.ensureStreamingStep(e.step_index);
            st.appendStreamingReasoning(e.step_index, e.content);
            break;
          }
          case 'tool_call':
            st.setStreaming(true);
            if (e.tool_call_id && e.tool_name && e.step_index !== undefined) {
              st.ensureStreamingStep(e.step_index);
              st.addStreamingToolCall(e.step_index, {
                id: e.tool_call_id,
                name: e.tool_name,
                arguments: e.tool_args || {},
                status: 'running',
              });
            }
            break;
          case 'tool_result':
            if (e.tool_call_id && e.step_index !== undefined) {
              st.ensureStreamingStep(e.step_index);
              st.updateStreamingToolCall(e.step_index, e.tool_call_id, {
                result: e.tool_result,
                status: (e.status as ToolCallInfo['status']) || 'completed',
                duration_ms: e.duration_ms,
              });
            }
            break;
          case 'step_complete':
            if (e.step_index !== undefined) {
              const cur = st.currentStreamingStep;
              if (cur && cur.step_index === e.step_index) st.finalizeStreamingStep(e.step_index);
            }
            break;
          case 'tool_approval_required': {
            const stepIndex = e.step_index;
            st.setPendingApproval({
              toolCallId: e.tool_call_id ?? '',
              toolName: e.tool_name ?? '',
              args: e.tool_args ? JSON.stringify(e.tool_args, null, 1) : '',
              stepIndex,
            });
            if (e.tool_call_id && e.tool_name && stepIndex !== undefined) {
              st.ensureStreamingStep(stepIndex);
              st.updateStreamingToolCall(stepIndex, e.tool_call_id, { status: 'pending' });
              st.addStreamingToolCall(stepIndex, {
                id: e.tool_call_id,
                name: e.tool_name,
                arguments: e.tool_args || {},
                status: 'pending',
              });
            }
            break;
          }
          case 'done':
          case 'cancelled': {
            st.setStreaming(false);
            st.setPendingApproval(null);
            const cur = st.currentStreamingStep;
            if (cur) st.finalizeStreamingStep(cur.step_index);
            if (st.session) {
              const sid = st.session.id;
              const reload = () => Promise.all([getChatMessages(sid), getAgentSteps(sid)]);
              const applyReloaded = ([msgs, steps]: [ChatMessage[], AgentStep[]]) => {
                // Attribute steps the backend left unlinked to the latest
                // assistant reply so their card still renders in history.
                const lastAssistant = [...msgs].reverse().find((m) => m.role === 'assistant');
                const linked = lastAssistant
                  ? steps.map((s) => (s.message_id === null ? { ...s, message_id: lastAssistant.id } : s))
                  : steps;
                usePetStore.setState({ messages: msgs, agentSteps: linked, streamContent: '' });
                // History cards now come from persisted steps; drop the
                // live copy so it is not rendered twice.
                usePetStore.getState().clearStreamingSteps();
                // Signal completion so the panel lands on the final reply
                // (handled by the completionNonce effect below). The
                // completion swap collapses the live reasoning card, and
                // the smooth auto-scroll may have disengaged mid-run (its
                // own scroll events flip isNearBottomRef), so without this
                // the view can stay stranded with the reply off-screen.
                usePetStore.setState((s) => ({ completionNonce: s.completionNonce + 1 }));
              };
              (async () => {
                try {
                  applyReloaded(await reload());
                } catch (err) {
                  // Retry once after a short delay — transient failures
                  // (e.g. the DB briefly locked by a concurrent sync write)
                  // usually clear immediately.
                  console.error('pet reload messages:', err);
                  try {
                    await new Promise((r) => setTimeout(r, 800));
                    applyReloaded(await reload());
                  } catch (err2) {
                    // Never lose the reply: fall back to the streamed content
                    // as a local-only message. A later successful reload
                    // replaces the whole list, so this cannot duplicate.
                    console.error('pet reload retry failed:', err2);
                    const streamed = usePetStore.getState().streamContent;
                    if (streamed.trim()) {
                      const localMsg: ChatMessage = {
                        id: `local-${Date.now()}`,
                        session_id: sid,
                        role: 'assistant',
                        content: streamed,
                        reasoning_content: null,
                        tool_calls: null,
                        citations: null,
                        model: null,
                        tokens_used: null,
                        tokens_in: null,
                        tokens_in_hit: null,
                        tokens_out: null,
                        attachments: null,
                        created_at: new Date().toISOString(),
                      };
                      usePetStore.setState((s) => ({ messages: [...s.messages, localMsg], streamContent: '' }));
                      // Keep the live streaming steps — without persisted
                      // steps they are the only copy of the reasoning chain.
                      usePetStore.setState((s) => ({ completionNonce: s.completionNonce + 1 }));
                    } else {
                      usePetStore.setState({ streamContent: '' });
                    }
                  }
                }
              })();
            }
            if (e.type === 'done') {
              notify('任务已完成，可在面板中查看结果');
            }
            break;
          }
          case 'error':
            st.setStreaming(false);
            st.setPendingApproval(null);
            st.clearStreamingSteps();
            st.setError(e.content || '调用失败');
            break;
          default:
            break;
        }
      });
    };
    setup();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify]);

  const scrollToBottom = useCallback((behavior: ScrollBehavior) => {
    bottomRef.current?.scrollIntoView({ behavior });
  }, []);

  const updateNearBottom = useCallback(() => {
    const container = messagesRef.current;
    if (!container) return;
    const distance = container.scrollHeight - container.scrollTop - container.clientHeight;
    isNearBottomRef.current = distance < 80;
  }, []);

  // Reset the "just opened" flag whenever the panel becomes visible so we
  // scroll to the bottom on first render after opening.
  useEffect(() => {
    if (store.open) {
      justOpenedRef.current = true;
    }
  }, [store.open]);

  // Keep the pet message list scrolled to the latest output.
  // - On first open, jump to the bottom immediately.
  // - While streaming, only auto-scroll if the user is already near the bottom.
  // - User-sent messages do NOT yank the view down, so scrolling back to read
  //   history is preserved.
  useEffect(() => {
    if (justOpenedRef.current && store.messages.length > 0) {
      justOpenedRef.current = false;
      requestAnimationFrame(() => scrollToBottom('auto'));
      return;
    }
    justOpenedRef.current = false;

    if (store.streaming && isNearBottomRef.current) {
      requestAnimationFrame(() => scrollToBottom('smooth'));
    }
  }, [store.messages, store.streamContent, store.streamingSteps, store.currentStreamingStep, store.streaming, scrollToBottom]);

  // Land on the final reply once a run's persisted messages have loaded.
  // Fires after the completion swap (live card → collapsed history card) has
  // been committed, so the destination is the final layout, not a moving
  // streaming target.
  const completionNonce = usePetStore((s) => s.completionNonce);
  useEffect(() => {
    if (completionNonce === 0) return;
    requestAnimationFrame(() => scrollToBottom('auto'));
  }, [completionNonce, scrollToBottom]);

  // Persisted tool-call/reasoning steps grouped by message (history cards).
  const stepsByMessage = useMemo(() => {
    const map = new Map<string, AgentStep[]>();
    for (const s of store.agentSteps) {
      if (!s.message_id) continue;
      const list = map.get(s.message_id) || [];
      list.push(s);
      map.set(s.message_id, list);
    }
    return map;
  }, [store.agentSteps]);

  const handleSend = async () => {
    const text = input.trim();
    if (!text || store.streaming) return;
    setInput('');
    await store.send(text);
  };

  const handleQuick = (cmd: string) => {
    // "解释当前段落" always uses the CURRENT selection (live from the
    // context store), not the snapshot baked into the session at creation.
    if (cmd === SELECTION_COMMAND) {
      const sel = context?.selectedText?.trim();
      if (sel) {
        store.send(`解释当前段落：\n"${sel}"`);
      } else {
        useDialogStore.getState().alert(
          '请先在 PDF 中划选要解释的段落，再点击「解释当前段落」。',
          '提示'
        );
      }
      return;
    }
    store.send(cmd);
  };

  const handleApprove = async (approved: boolean) => {
    const a = store.pendingApproval;
    if (!a || !store.session) return;
    try {
      await agentApproveTool(store.session.id, a.toolCallId, approved);
      // Only dismiss the approval on success — on failure keep it open so the
      // user can retry, and surface the error instead of hanging silently.
      store.setPendingApproval(null);
    } catch (err) {
      console.error('pet approve:', err);
      store.setError(err instanceof Error ? err.message : String(err));
    }
  };

  const quickCommands = (context ? QUICK_COMMANDS[context.page] ?? [] : [])
    .filter((cmd) => liveSelection || cmd !== SELECTION_COMMAND);

  return (
    <div className="flex-1 min-h-0 flex flex-col">
      {/* Messages */}
      <div ref={messagesRef} onScroll={updateNearBottom} className="flex-1 min-h-0 overflow-y-auto p-3 space-y-3">
        {context && (
          <div>
            <span className="inline-flex items-center gap-1 text-[11px] px-2 py-0.5 rounded-full bg-primary/10 text-primary max-w-full truncate">
              {context.title}
            </span>
          </div>
        )}
        {store.loading && (
          <div className="flex items-center gap-2 text-xs text-text-secondary/60">
            <Loader2 size={12} className="animate-spin" /> 正在准备…
          </div>
        )}
        {store.error && (
          <div className="text-xs text-red-400 bg-red-500/5 border border-red-500/20 rounded-lg px-3 py-2">
            {store.session ? `调用失败：${store.error}` : `无法启动该页面的智能体：${store.error}`}
            {!store.session && (
              <div className="text-text-secondary/60 mt-1">可在「设置 → 宠物」中启用对应智能体。</div>
            )}
          </div>
        )}
        {/* Messages ALWAYS render — the phases card used to gate this whole
            block, so an empty step list (e.g. right after sending a follow-up)
            unmounted every message, the scroll position clamped to the top,
            and the next auto-scroll visibly swept through the history. */}
        {store.messages.map((m) => {
          const mSteps = m.role === 'assistant' ? stepsByMessage.get(m.id) : undefined;
          return (
            <div key={m.id} className="space-y-1.5">
              {mSteps && mSteps.length > 0 && (
                <div className="flex justify-start">
                  <div className="w-full max-w-[95%]">
                    <ReasoningProcessCard phases={stepsToPhases(mSteps)} streaming={false} defaultCollapsed />
                  </div>
                </div>
              )}
              <PetMessage msg={m} onCitation={handleCitation} onSaveNote={handleSaveNote} saving={savingNoteId === m.id} />
            </div>
          );
        })}
        {/* Live (or not-yet-persisted) turn: card from streaming steps, below
            the existing messages and above the streaming reply. */}
        {(() => {
          const phases = streamingToPhases(store.streamingSteps, store.currentStreamingStep);
          if (phases.length === 0) return null;
          return (
            <div className="flex justify-start">
              <div className="w-full max-w-[95%]">
                <ReasoningProcessCard phases={phases} streaming={store.streaming} defaultCollapsed />
              </div>
            </div>
          );
        })()}
        {store.streamContent && (
          <div className="flex justify-start">
            <div className="max-w-[92%] prose prose-sm prose-invert max-w-none [&>*:first-child]:mt-0">
              <PetMarkdown content={store.streamContent} onCitation={handleCitation} />
              <span className="inline-block w-1.5 h-3.5 bg-primary animate-pulse ml-0.5 align-middle rounded-sm" />
            </div>
          </div>
        )}
        {store.messages.length === 0 && !store.loading && !store.streaming && (
          <div className="text-center text-xs text-text-secondary/50 py-4">
            {context
              ? `我可以针对「${context.title}」帮你处理，试试下面的快捷指令`
              : '打开笔记、文献、课题等内容后，我可以帮你处理'}
          </div>
        )}
        <div ref={bottomRef} />
      </div>

      {/* Approval */}
      {store.pendingApproval && (
        <div className="px-3 py-2 border-t border-surface-hover shrink-0">
          <div className="rounded-lg bg-background/60 border border-primary/30 p-2">
            <div className="flex items-center gap-1.5 text-[11px] text-primary mb-1">
              <ShieldAlert size={12} />
              <span className="font-medium">需要批准：{store.pendingApproval.toolName}</span>
            </div>
            <pre className="text-[10px] text-text-secondary/80 whitespace-pre-wrap max-h-20 overflow-y-auto mb-1.5">
              {store.pendingApproval.args}
            </pre>
            <div className="flex gap-1.5">
              <button
                onClick={() => handleApprove(true)}
                className="flex items-center gap-1 px-2.5 py-1 rounded bg-primary/15 text-primary text-[11px] hover:bg-primary/25 transition-colors"
              >
                <Check size={11} /> 批准
              </button>
              <button
                onClick={() => handleApprove(false)}
                className="px-2.5 py-1 rounded text-text-secondary text-[11px] hover:bg-surface-hover transition-colors"
              >
                拒绝
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Quick commands (fresh session only) */}
      {store.messages.length === 0 && context && quickCommands.length > 0 && !store.loading && !store.streaming && (
        <div className="px-3 pb-2 flex flex-wrap gap-1.5 shrink-0">
          {quickCommands.map((cmd) => (
            <button
              key={cmd}
              onClick={() => handleQuick(cmd)}
              disabled={store.streaming}
              className="px-2.5 py-1 rounded-full bg-surface-hover text-[11px] text-text-secondary hover:text-text-primary hover:bg-surface-hover/80 transition-colors disabled:opacity-40"
            >
              {cmd}
            </button>
          ))}
        </div>
      )}

      {/* Re-surface the most useful reader action when the user selects a
          new paragraph after the conversation has already started. Only in
          the main window, where the selection is live. */}
      {liveSelection &&
        store.messages.length > 0 &&
        context?.page === 'reader' &&
        context?.selectedText?.trim() &&
        !store.loading &&
        !store.streaming && (
          <div className="px-3 pb-2 flex flex-wrap gap-1.5 shrink-0">
            <button
              onClick={() => handleQuick(SELECTION_COMMAND)}
              disabled={store.streaming}
              className="px-2.5 py-1 rounded-full bg-primary/15 text-primary text-[11px] hover:bg-primary/25 transition-colors disabled:opacity-40"
            >
              解释当前段落
            </button>
          </div>
        )}

      {/* Input */}
      <div className="px-3 py-2.5 border-t border-surface-hover flex items-center gap-2 shrink-0">
        <input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') handleSend();
          }}
          placeholder="告诉我要做什么…"
          className="flex-1 min-w-0 h-9 bg-background text-text-primary text-[13px] px-3 rounded-lg border border-surface-hover focus:border-primary/40 focus:outline-none placeholder:text-text-secondary/40"
        />
        <button
          onClick={handleSend}
          disabled={!input.trim() || store.streaming}
          className="w-9 h-9 rounded-lg bg-primary/15 text-primary flex items-center justify-center hover:bg-primary/25 transition-colors disabled:opacity-40 disabled:cursor-not-allowed shrink-0"
          aria-label="发送"
        >
          {store.streaming ? <Loader2 size={14} className="animate-spin" /> : <Send size={14} />}
        </button>
      </div>
    </div>
  );
}
