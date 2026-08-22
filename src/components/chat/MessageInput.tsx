import { useState, useRef, useEffect } from 'react';
import { Send, Paperclip, BookOpen, Square, X, FileText } from 'lucide-react';
import { useChatStore } from '@/stores/chatStore';
import { useProjectStore } from '@/stores/projectStore';
import { agentSendMessage, agentCancel, readTextFile } from '@/lib/tauri';
import { useActiveAgentName } from '@/hooks/useActiveAgentName';

interface Props {
  disabled?: boolean;
}

interface Attachment {
  path: string;
  name: string;
  content: string;
}

const TEXT_FILTERS = [
  { name: '文本文件', extensions: ['txt', 'md', 'json', 'ts', 'tsx', 'js', 'rs', 'py', 'css', 'html', 'yml', 'yaml', 'toml', 'csv', 'xml'] },
];

export function MessageInput({ disabled }: Props) {
  const agentName = useActiveAgentName();
  const [input, setInput] = useState('');
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const { activeSessionId, addMessage, setLoading, isStreaming } = useChatStore();
  const activeProject = useProjectStore((s) =>
    s.projects.find((p) => p.id === s.activeProjectId)
  );

  const handleAttach = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({
        multiple: false,
        directory: false,
        defaultPath: activeProject?.path,
        filters: TEXT_FILTERS,
      });
      if (selected && typeof selected === 'string') {
        const content = await readTextFile(selected);
        const name = selected.split(/[\\/]/).pop() || selected;
        setAttachments((prev) => (prev.some((a) => a.path === selected) ? prev : [...prev, { path: selected, name, content }]));
      }
    } catch (err) {
      console.error('Attach failed:', err);
    }
  };

  const handleStop = () => {
    if (activeSessionId) agentCancel(activeSessionId).catch(() => {});
  };

  const handleSend = async () => {
    const text = input.trim();
    if ((!text && attachments.length === 0) || !activeSessionId || isStreaming) return;

    const attachBlock = attachments
      .map((a) => `<file name="${a.name}" path="${a.path}">\n${a.content}\n</file>`)
      .join('\n');
    const content = [attachBlock, text].filter(Boolean).join('\n\n');

    setInput('');
    setAttachments([]);
    setLoading(true);
    resetHeight();

    addMessage({
      id: `user_${Date.now()}`,
      session_id: activeSessionId,
      role: 'user',
      content,
      reasoning_content: null,
      tool_calls: null,
      citations: null,
      model: null,
      tokens_used: null,
      created_at: new Date().toISOString(),
    });

    try {
      await agentSendMessage(activeSessionId, content);
    } catch (err) {
      setLoading(false);
      addMessage({
        id: `error_${Date.now()}`,
        session_id: activeSessionId,
        role: 'assistant',
        content: `❌ 发送失败: ${err}`,
        reasoning_content: null,
        tool_calls: null,
        citations: null,
        model: null,
        tokens_used: null,
        created_at: new Date().toISOString(),
      });
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const resetHeight = () => {
    if (textareaRef.current) {
      textareaRef.current.style.height = 'auto';
    }
  };

  const handleInput = (e: React.FormEvent<HTMLTextAreaElement>) => {
    const el = e.currentTarget;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
  };

  useEffect(() => {
    if (textareaRef.current) {
      textareaRef.current.focus();
    }
  }, [activeSessionId]);

  const canSend = !disabled && (!!input.trim() || attachments.length > 0);

  return (
    <div className="px-5 pb-5 pt-2 bg-background">
      <div className="max-w-[800px] mx-auto">
        <div className="rounded-xl border border-surface-hover p-1 transition-colors focus-within:bg-surface">
          {attachments.length > 0 && (
            <div className="flex flex-wrap gap-1.5 px-2 pt-2">
              {attachments.map((a) => (
                <span
                  key={a.path}
                  className="inline-flex items-center gap-1 px-2 py-1 rounded-md bg-primary/10 text-primary text-[11px] max-w-[220px]"
                  title={a.path}
                >
                  <FileText size={11} className="shrink-0" />
                  <span className="truncate">{a.name}</span>
                  <button
                    type="button"
                    onClick={() =>
                      setAttachments((prev) => prev.filter((x) => x.path !== a.path))
                    }
                    className="hover:text-red-400 shrink-0"
                    aria-label="移除附件"
                  >
                    <X size={11} />
                  </button>
                </span>
              ))}
            </div>
          )}
          <textarea
            ref={textareaRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            onInput={handleInput}
            placeholder={isStreaming ? 'AI 正在回复…' : `给 ${agentName} 发送指令…`}
            disabled={disabled}
            rows={1}
            className="w-full min-h-[52px] max-h-[200px] bg-transparent border-0 outline-0 focus:outline-none text-text-primary text-[14px] leading-relaxed px-3.5 py-3 resize-none placeholder:text-text-secondary/60"
          />
          <div className="flex items-center justify-between px-2 pb-1.5">
            <div className="flex items-center gap-1">
              <button
                type="button"
                disabled={disabled}
                onClick={handleAttach}
                className="w-7 h-7 rounded-md border-0 bg-transparent text-text-secondary/70 flex items-center justify-center hover:bg-surface-hover hover:text-text-primary transition-colors disabled:opacity-50"
                title="附加文件（作为上下文发送）"
              >
                <Paperclip size={15} />
              </button>
              <button
                type="button"
                disabled={disabled}
                className="w-7 h-7 rounded-md border-0 bg-transparent text-text-secondary/70 flex items-center justify-center hover:bg-surface-hover hover:text-text-primary transition-colors disabled:opacity-50"
                title="选择上下文"
              >
                <BookOpen size={15} />
              </button>
            </div>
            {isStreaming ? (
              <button
                type="button"
                onClick={handleStop}
                className="w-8 h-8 rounded-lg bg-surface-hover text-text-primary flex items-center justify-center hover:bg-red-500/20 hover:text-red-400 transition-colors"
                title="停止生成"
              >
                <Square size={14} />
              </button>
            ) : (
              <button
                onClick={handleSend}
                disabled={!canSend}
                className="w-8 h-8 rounded-lg bg-surface-hover text-text-primary flex items-center justify-center hover:bg-surface transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
              >
                <Send size={15} />
              </button>
            )}
          </div>
        </div>
        <div className="text-center text-[12px] text-text-secondary/60 mt-2">
          Enter 发送 · Shift + Enter 换行 · 工具调用前会请求确认
        </div>
      </div>
    </div>
  );
}
