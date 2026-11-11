import { useState, useRef, useEffect, useCallback } from 'react';
import { Send, Paperclip, BookOpen, Brain, Square, X, FileText, Scissors } from 'lucide-react';
import { useChatStore } from '@/stores/chatStore';
import { useProjectStore } from '@/stores/projectStore';
import { agentSendMessage, agentCancel, agentMemoryGet, agentGetSession, agentSetApprovalConfig } from '@/lib/tauri';
import { useActiveAgentName } from '@/hooks/useActiveAgentName';
import { useDialog } from '@/hooks/useDialog';
import { useImageAttachments } from '@/hooks/useImageAttachments';
import { useTextAttachments } from '@/hooks/useTextAttachments';
import { useFileDrop } from '@/hooks/useFileDrop';
import { ATTACH_FILTERS, dispatchAttachmentPaths } from '@/lib/attachments';
import { AgentMemoryModal } from './AgentMemoryModal';
import { ApprovalPolicySwitch } from './ApprovalPolicySwitch';
import { useSkillMention } from './useSkillMention';
import { ContextPickerModal } from './ContextPickerModal';
import { contextKey, type ContextItem } from './contextItem';
import type { ApprovalConfig, ChatAttachment } from '@/lib/types';

interface Props {
  disabled?: boolean;
}

export function MessageInput({ disabled }: Props) {
  const agentName = useActiveAgentName();
  const { alert } = useDialog();
  const [input, setInput] = useState('');
  const {
    attachments: textAttachments,
    addPath: addTextPath,
    removeAttachment,
    clearAttachments,
    toMessageBlock,
  } = useTextAttachments();
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const { activeSessionId, addMessage, setLoading, isStreaming, sessions, setSessions } = useChatStore();
  const activeSession = sessions.find((s) => s.id === activeSessionId) ?? null;
  const activeProject = useProjectStore((s) =>
    s.projects.find((p) => p.id === s.activeProjectId)
  );
  // Long-term memory button state (per agent, persisted server-side).
  const [memoryActive, setMemoryActive] = useState(false);
  // Days since the memory was last edited, once it crosses the staleness
  // threshold — the brain button turns amber to prompt a review.
  const [memoryStaleDays, setMemoryStaleDays] = useState<number | null>(null);
  const [memoryOpen, setMemoryOpen] = useState(false);
  // Picked note/file contexts attached to the next message (one-shot).
  const [contexts, setContexts] = useState<ContextItem[]>([]);
  const [contextOpen, setContextOpen] = useState(false);
  // Approval policy quick switch (per session, persisted; next-turn effect).
  const [approvalConfig, setApprovalConfig] = useState<ApprovalConfig>({ mode: 'auto' });

  // Long-term memory has two scopes (global synced / device-local); the
  // brain button reflects either: lit when any scope is injected, amber when
  // any active scope has gone stale (90+ days untouched).
  const refreshMemoryState = useCallback(() => {
    if (!activeSessionId) return;
    Promise.all([
      agentMemoryGet(activeSessionId, 'global'),
      agentMemoryGet(activeSessionId, 'device'),
    ])
      .then((mems) => {
        const injected = mems.filter((m) => m?.active && m.content.trim());
        setMemoryActive(injected.length > 0);
        let worst: number | null = null;
        for (const m of injected) {
          const days = Math.floor((Date.now() - new Date(m!.updated_at).getTime()) / 86400000);
          if (Number.isFinite(days) && days >= 90) worst = Math.max(worst ?? 0, days);
        }
        setMemoryStaleDays(worst);
      })
      .catch(() => {});
  }, [activeSessionId]);

  useEffect(() => {
    setMemoryActive(false);
    setMemoryStaleDays(null);
    refreshMemoryState();
    if (!activeSessionId) return;
    agentGetSession(activeSessionId)
      .then((s) => setApprovalConfig(s.approval_config ?? { mode: 'auto' }))
      .catch(() => {});
  }, [activeSessionId, refreshMemoryState]);

  const pickApprovalMode = (mode: ApprovalConfig['mode']) => {
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

  // Attach entry: both the picker and window drag & drop funnel into the
  // shared dispatcher — each path is routed by extension (images inline as
  // base64, documents extracted to text). cacheDir = session write dir when
  // set, so the agent's file_read can page oversized extractions.
  const attachPaths = async (paths: string[]) => {
    const failures = await dispatchAttachmentPaths(paths, {
      imageExists: (name) => images.some((a) => a.name === name),
      addImage: addImageData,
      addTextPath: (path) => addTextPath(path, activeProject?.path),
    });
    if (failures.length > 0) {
      await alert(failures.join('\n'), '附件读取失败');
    }
  };

  const handleAttach = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({
        multiple: true,
        directory: false,
        defaultPath: activeProject?.path,
        filters: ATTACH_FILTERS,
      });
      const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
      await attachPaths(paths.filter((p): p is string => typeof p === 'string'));
    } catch (err) {
      console.error('Attach failed:', err);
    }
  };

  // OS file drag & drop (Tauri native): anywhere in the window works.
  const { isDragging } = useFileDrop(attachPaths);

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
    const attachBlock = toMessageBlock();
    const skillHint = skillMention.takeFocusHint();
    const content = [skillHint, contextBlock, attachBlock, text].filter(Boolean).join('\n\n');

    const chatAttachments: ChatAttachment[] | undefined =
      images.length > 0
        ? images.map((a) => ({ mime: a.mime, base64: a.base64, name: a.name }))
        : undefined;

    setInput('');
    clearAttachments();
    clearImages();
    setContexts([]);
    setLoading(true);
    resetHeight();

    const tempId = `user_${Date.now()}`;
    addMessage({
      id: tempId,
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
      user_tag: null,
      created_at: new Date().toISOString(),
    });

    try {
      const userMessageId = await agentSendMessage(activeSessionId, content, chatAttachments);
      useChatStore.getState().swapMessageId(tempId, userMessageId);
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
        user_tag: null,
        created_at: new Date().toISOString(),
      });
    }
  };

  // 技能 @ 提及：键入 @（或 /）弹出技能面板，选中按需挂载并插入提示文本。
  const skillMention = useSkillMention({
    input,
    setInput,
    textareaRef,
    sessionId: activeSessionId,
    selectedSkills: activeSession?.selected_skills ?? [],
    onSkillsChange: (next) =>
      setSessions(
        sessions.map((s) => (s.id === activeSessionId ? { ...s, selected_skills: next } : s)),
      ),
    disabled,
  });

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (skillMention.handleKeyDown(e as React.KeyboardEvent<HTMLTextAreaElement>)) return;
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
          className={`relative rounded-xl border p-1 transition-colors focus-within:bg-surface ${
            isDragging ? 'border-primary bg-primary/5' : 'border-surface-hover'
          }`}
        >
          {skillMention.popover}
          {skillMention.chips}
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
                    onClick={() => removeAttachment(a.id)}
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
            onChange={(e) => {
              setInput(e.target.value);
              skillMention.handleChange(e);
            }}
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
              {skillMention.button}
              <button
                type="button"
                disabled={disabled}
                onClick={handleAttach}
                className="w-7 h-7 rounded-md border-0 bg-transparent text-text-secondary/70 flex items-center justify-center hover:bg-surface-hover hover:text-text-primary transition-colors disabled:opacity-50"
                title="附加文件或图片"
              >
                <Paperclip size={15} />
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
                title={shotArmed ? '等待截图…（截完自动附加；再点一次取消）' : '截图（Ctrl+Shift+S，调用系统截图工具，截完自动附加）'}
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
                  memoryStaleDays !== null
                    ? 'text-amber-400 hover:text-amber-300'
                    : memoryActive
                      ? 'text-primary hover:text-primary'
                      : 'text-text-secondary/50 hover:text-text-primary'
                }`}
                title={
                  memoryStaleDays !== null
                    ? `长期记忆已 ${memoryStaleDays} 天未更新，建议点击审查`
                    : memoryActive
                      ? '长期记忆 · 已激活'
                      : '长期记忆 · 已遗忘'
                }
              >
                <Brain size={15} />
              </button>
              <ApprovalPolicySwitch
                mode={approvalConfig.mode}
                onPick={pickApprovalMode}
                disabled={disabled || !activeSessionId}
                titleSuffix="（仅当前会话，下一条消息生效）"
              />
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
          Enter 发送 · Shift + Enter 换行 · @ 使用技能 · Ctrl+Shift+S 截图 · 拖拽文件到窗口即可附加
        </div>
      </div>
      {isDragging && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/70 backdrop-blur-[1px] pointer-events-none">
          <div className="rounded-xl border-2 border-dashed border-primary bg-surface px-8 py-6 text-primary text-sm font-medium">
            松开以添加附件
          </div>
        </div>
      )}
      {memoryOpen && activeSessionId && (
        <AgentMemoryModal
          sessionId={activeSessionId}
          onClose={() => {
            setMemoryOpen(false);
            // Editing inside the modal clears staleness — refresh the brain
            // button so the amber state disappears without a session switch.
            refreshMemoryState();
          }}
          onStateChange={refreshMemoryState}
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
