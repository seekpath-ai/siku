import { useState, useRef, useEffect, useCallback } from 'react';
import { Send, Paperclip, ImagePlus, BookOpen, Brain, Square, X, FileText, ShieldCheck, Check, Scissors } from 'lucide-react';
import { useChatStore } from '@/stores/chatStore';
import { useProjectStore } from '@/stores/projectStore';
import { agentSendMessage, agentCancel, readTextFile, readImageFile, agentMemoryGet, agentGetSession, agentSetApprovalConfig } from '@/lib/tauri';
import { useActiveAgentName } from '@/hooks/useActiveAgentName';
import { useDialog } from '@/hooks/useDialog';
import { useImageAttachments } from '@/hooks/useImageAttachments';
import { AgentMemoryModal } from './AgentMemoryModal';
import { ContextPickerModal } from './ContextPickerModal';
import { contextKey, type ContextItem } from './contextItem';
import type { ApprovalConfig, ChatAttachment } from '@/lib/types';

/** Approval policy options for the input-area quick switch. */
const APPROVAL_OPTIONS: { value: ApprovalConfig['mode']; label: string; hint: string }[] = [
  { value: 'auto', label: '自动批准', hint: '所有工具调用直接执行' },
  { value: 'auto_expire_time', label: '时间窗口内自动', hint: '批准后一段时间内同类调用免确认' },
  { value: 'auto_by_rules', label: '白名单自动', hint: '仅白名单工具免确认' },
  { value: 'manual', label: '手动审批', hint: '写操作逐条确认，只读免确认' },
  { value: 'manual_all', label: '严格审批', hint: '读写所有调用逐条确认' },
];

interface Props {
  disabled?: boolean;
}

interface TextAttachment {
  kind: 'text';
  id: string;
  path: string;
  name: string;
  content: string;
}

const TEXT_FILTERS = [
  { name: '文本文件', extensions: ['txt', 'md', 'json', 'ts', 'tsx', 'js', 'rs', 'py', 'css', 'html', 'yml', 'yaml', 'toml', 'csv', 'xml'] },
];

const IMAGE_FILTERS = [
  { name: '图片', extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp'] },
];

export function MessageInput({ disabled }: Props) {
  const agentName = useActiveAgentName();
  const { alert } = useDialog();
  const [input, setInput] = useState('');
  const [textAttachments, setTextAttachments] = useState<TextAttachment[]>([]);
  const [isDragging, setIsDragging] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const { activeSessionId, addMessage, setLoading, isStreaming } = useChatStore();
  const activeProject = useProjectStore((s) =>
    s.projects.find((p) => p.id === s.activeProjectId)
  );
  // Long-term memory button state (per agent, persisted server-side).
  const [memoryActive, setMemoryActive] = useState(false);
  const [memoryOpen, setMemoryOpen] = useState(false);
  // Picked note/file contexts attached to the next message (one-shot).
  const [contexts, setContexts] = useState<ContextItem[]>([]);
  const [contextOpen, setContextOpen] = useState(false);
  // Approval policy quick switch (per agent, persisted; next-turn effect).
  const [approvalConfig, setApprovalConfig] = useState<ApprovalConfig>({ mode: 'auto' });
  const [approvalOpen, setApprovalOpen] = useState(false);
  const approvalRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setMemoryActive(false);
    if (!activeSessionId) return;
    agentMemoryGet(activeSessionId)
      .then((m) => setMemoryActive(m?.active ?? false))
      .catch(() => {});
    agentGetSession(activeSessionId)
      .then((s) => setApprovalConfig(s.approval_config ?? { mode: 'auto' }))
      .catch(() => {});
  }, [activeSessionId]);

  // Close the approval dropdown on outside click.
  useEffect(() => {
    if (!approvalOpen) return;
    const onDown = (e: MouseEvent) => {
      if (approvalRef.current && !approvalRef.current.contains(e.target as Node)) {
        setApprovalOpen(false);
      }
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [approvalOpen]);

  const pickApprovalMode = (mode: ApprovalConfig['mode']) => {
    setApprovalOpen(false);
    if (!activeSessionId || mode === approvalConfig.mode) return;
    const next = { ...approvalConfig, mode };
    setApprovalConfig(next);
    agentSetApprovalConfig(activeSessionId, next).catch((err) =>
      console.error('set approval config:', err)
    );
  };

  // Image attachments (staging, paste, OS-screenshot flow) come from the
  // shared hook; this input owns the global Ctrl+Shift+S shortcut.
  const {
    images,
    addImageData,
    addImageFile,
    removeImage,
    clearImages,
    shotArmed,
    startScreenshot,
    handlePaste,
  } = useImageAttachments({
    disabled,
    enableShortcut: true,
    onAttached: () => textareaRef.current?.focus(),
    onError: (msg, title) => alert(msg, title ?? '图片附件'),
  });

  const handleAttachText = async () => {
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
        setTextAttachments((prev) =>
          prev.some((a) => a.path === selected)
            ? prev
            : [...prev, { kind: 'text', id: `txt_${Date.now()}`, path: selected, name, content }]
        );
      }
    } catch (err) {
      console.error('Attach failed:', err);
    }
  };

  const handleAttachImage = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({
        multiple: true,
        directory: false,
        defaultPath: activeProject?.path,
        filters: IMAGE_FILTERS,
      });
      const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
      for (const path of paths) {
        if (typeof path !== 'string') continue;
        const name = path.split(/[\\/]/).pop() || path;
        if (images.some((a) => a.name === name)) continue;
        const att = await readImageFile(path);
        addImageData({
          name: att.name || name,
          mime: att.mime,
          base64: att.base64,
          previewUrl: `data:${att.mime};base64,${att.base64}`,
        });
      }
    } catch (err) {
      console.error('Image attach failed:', err);
    }
  };

  const handleDrop = useCallback(
    async (e: React.DragEvent) => {
      e.preventDefault();
      e.stopPropagation();
      setIsDragging(false);
      const files = Array.from(e.dataTransfer.files).filter((f) => f.type.startsWith('image/'));
      for (const file of files) {
        await addImageFile(file);
      }
    },
    [addImageFile]
  );

  const handleDragOver = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    setIsDragging(true);
  }, []);

  const handleDragLeave = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    setIsDragging(false);
  }, []);

  const handleStop = () => {
    if (activeSessionId) agentCancel(activeSessionId).catch(() => {});
  };

  const handleSend = async () => {
    const text = input.trim();

    if ((!text && textAttachments.length === 0 && images.length === 0 && contexts.length === 0) || !activeSessionId || isStreaming) return;

    // Picked notes/files travel as a plain user-message context block
    // (unlike the long-term memory, which lives in the system prompt).
    const contextBlock = contexts.length
      ? `<context>\n${contexts
          .map(
            (c) =>
              `# ${c.kind === 'note' ? '笔记' : '文件'}「${c.name}」\n${c.content ?? '[二进制文件，未附加内容]'}`
          )
          .join('\n\n')}\n</context>`
      : '';
    const attachBlock = textAttachments
      .map((a) => `<file name="${a.name}" path="${a.path}">\n${a.content}\n</file>`)
      .join('\n');
    const content = [contextBlock, attachBlock, text].filter(Boolean).join('\n\n');

    const chatAttachments: ChatAttachment[] | undefined =
      images.length > 0
        ? images.map((a) => ({ mime: a.mime, base64: a.base64, name: a.name }))
        : undefined;

    setInput('');
    setTextAttachments([]);
    clearImages();
    setContexts([]);
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
      tokens_in: null,
      tokens_in_hit: null,
      tokens_out: null,
      attachments: chatAttachments ? JSON.stringify(chatAttachments) : null,
      created_at: new Date().toISOString(),
    });

    try {
      await agentSendMessage(activeSessionId, content, chatAttachments);
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
        tokens_in: null,
        tokens_in_hit: null,
        tokens_out: null,
        attachments: null,
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

  const canSend =
    !disabled &&
    (!!input.trim() || textAttachments.length > 0 || images.length > 0 || contexts.length > 0);

  return (
    <div className="px-5 pb-5 pt-2 bg-background">
      <div className="max-w-[800px] mx-auto">
        <div
          className={`rounded-xl border p-1 transition-colors focus-within:bg-surface ${
            isDragging ? 'border-primary bg-primary/5' : 'border-surface-hover'
          }`}
          onDrop={handleDrop}
          onDragOver={handleDragOver}
          onDragLeave={handleDragLeave}
        >
          {(textAttachments.length > 0 || images.length > 0 || contexts.length > 0) && (
            <div className="flex flex-wrap gap-1.5 px-2 pt-2">
              {contexts.map((c) => (
                <span
                  key={contextKey(c.kind, c.id)}
                  className="inline-flex items-center gap-1 px-2 py-1 rounded-md bg-emerald-500/10 text-emerald-400 text-[11px] max-w-[220px]"
                  title={c.kind === 'note' ? '笔记上下文' : '文件上下文'}
                >
                  <BookOpen size={11} className="shrink-0" />
                  <span className="truncate">{c.name}</span>
                  <button
                    type="button"
                    onClick={() =>
                      setContexts((prev) =>
                        prev.filter((x) => contextKey(x.kind, x.id) !== contextKey(c.kind, c.id))
                      )
                    }
                    className="hover:text-red-400 shrink-0"
                    aria-label="移除上下文"
                  >
                    <X size={11} />
                  </button>
                </span>
              ))}
              {textAttachments.map((a) => (
                <span
                  key={a.id}
                  className="inline-flex items-center gap-1 px-2 py-1 rounded-md bg-primary/10 text-primary text-[11px] max-w-[220px]"
                  title={a.path}
                >
                  <FileText size={11} className="shrink-0" />
                  <span className="truncate">{a.name}</span>
                  <button
                    type="button"
                    onClick={() => setTextAttachments((prev) => prev.filter((x) => x.id !== a.id))}
                    className="hover:text-red-400 shrink-0"
                    aria-label="移除附件"
                  >
                    <X size={11} />
                  </button>
                </span>
              ))}
              {images.map((a) => (
                <span
                  key={a.id}
                  className="inline-flex items-center gap-1 px-1.5 py-1 rounded-md bg-primary/10 text-primary text-[11px] max-w-[220px]"
                  title={a.name}
                >
                  <img
                    src={a.previewUrl}
                    alt={a.name}
                    className="w-6 h-6 object-cover rounded"
                  />
                  <span className="truncate">{a.name}</span>
                  <button
                    type="button"
                    onClick={() => removeImage(a.id)}
                    className="hover:text-red-400 shrink-0"
                    aria-label="移除图片"
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
            onPaste={handlePaste}
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
                onClick={handleAttachText}
                className="w-7 h-7 rounded-md border-0 bg-transparent text-text-secondary/70 flex items-center justify-center hover:bg-surface-hover hover:text-text-primary transition-colors disabled:opacity-50"
                title="附加文本文件"
              >
                <Paperclip size={15} />
              </button>
              <button
                type="button"
                disabled={disabled}
                onClick={handleAttachImage}
                className="w-7 h-7 rounded-md border-0 bg-transparent text-text-secondary/70 flex items-center justify-center hover:bg-surface-hover hover:text-text-primary transition-colors disabled:opacity-50"
                title="附加图片"
              >
                <ImagePlus size={15} />
              </button>
              <button
                type="button"
                disabled={disabled}
                onClick={startScreenshot}
                className={`w-7 h-7 rounded-md border-0 bg-transparent flex items-center justify-center hover:bg-surface-hover transition-colors disabled:opacity-50 ${
                  shotArmed
                    ? 'text-primary hover:text-primary'
                    : 'text-text-secondary/70 hover:text-text-primary'
                }`}
                title={shotArmed ? '等待截图…（截完自动附加）' : '截图（Ctrl+Shift+S，调用系统截图工具，截完自动附加）'}
              >
                <Scissors size={15} />
              </button>
              <button
                type="button"
                disabled={disabled}
                onClick={() => setContextOpen(true)}
                className="w-7 h-7 rounded-md border-0 bg-transparent text-text-secondary/70 flex items-center justify-center hover:bg-surface-hover hover:text-text-primary transition-colors disabled:opacity-50"
                title="选择上下文"
              >
                <BookOpen size={15} />
              </button>
              <button
                type="button"
                disabled={disabled || !activeSessionId}
                onClick={() => setMemoryOpen(true)}
                className={`w-7 h-7 rounded-md border-0 bg-transparent flex items-center justify-center hover:bg-surface-hover transition-colors disabled:opacity-50 ${
                  memoryActive
                    ? 'text-primary hover:text-primary'
                    : 'text-text-secondary/50 hover:text-text-primary'
                }`}
                title={memoryActive ? '长期记忆 · 已激活' : '长期记忆 · 已遗忘'}
              >
                <Brain size={15} />
              </button>
              <div className="relative" ref={approvalRef}>
                <button
                  type="button"
                  disabled={disabled || !activeSessionId}
                  onClick={() => setApprovalOpen((o) => !o)}
                  className={`w-7 h-7 rounded-md border-0 bg-transparent flex items-center justify-center hover:bg-surface-hover transition-colors disabled:opacity-50 ${
                    approvalConfig.mode === 'auto'
                      ? 'text-text-secondary/50 hover:text-text-primary'
                      : approvalConfig.mode === 'manual_all'
                        ? 'text-amber-400'
                        : 'text-primary'
                  }`}
                  title={`审批策略 · ${APPROVAL_OPTIONS.find((o) => o.value === approvalConfig.mode)?.label ?? ''}（下一条消息生效）`}
                >
                  <ShieldCheck size={15} />
                </button>
                {approvalOpen && (
                  <div className="absolute left-0 bottom-full z-50 mb-1 w-60 bg-surface border border-surface-hover rounded-lg shadow-xl py-1">
                    {APPROVAL_OPTIONS.map((opt) => (
                      <button
                        key={opt.value}
                        onClick={() => pickApprovalMode(opt.value)}
                        className="w-full flex items-start gap-2 px-3 py-1.5 text-left hover:bg-surface-hover"
                      >
                        <span className="flex-1 min-w-0">
                          <span className="block text-[12px] text-text-primary">{opt.label}</span>
                          <span className="block text-[10px] text-text-secondary/60">{opt.hint}</span>
                        </span>
                        {approvalConfig.mode === opt.value && (
                          <Check size={13} className="text-primary mt-0.5 shrink-0" />
                        )}
                      </button>
                    ))}
                    <div className="px-3 py-1.5 text-[10px] text-text-secondary/50 border-t border-surface-hover mt-1">
                      切换从下一条消息开始生效
                    </div>
                  </div>
                )}
              </div>
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
          Enter 发送 · Shift + Enter 换行 · Ctrl+Shift+S 截图 · 拖拽/粘贴图片也可发送
        </div>
      </div>
      {memoryOpen && activeSessionId && (
        <AgentMemoryModal
          sessionId={activeSessionId}
          onClose={() => setMemoryOpen(false)}
          onActiveChange={setMemoryActive}
        />
      )}
      {contextOpen && (
        <ContextPickerModal
          initialSelected={new Set(contexts.map((c) => contextKey(c.kind, c.id)))}
          onClose={() => setContextOpen(false)}
          onConfirm={(items) => {
            setContexts((prev) => {
              const existing = new Set(prev.map((c) => contextKey(c.kind, c.id)));
              return [...prev, ...items.filter((c) => !existing.has(contextKey(c.kind, c.id)))];
            });
            setContextOpen(false);
          }}
        />
      )}
    </div>
  );
}
