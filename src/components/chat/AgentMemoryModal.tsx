import { useState, useEffect, useRef, useCallback } from 'react';
import { Brain, X, Loader2, Check, BookOpen, Pen, MoreVertical, Code, Columns2, Rows2, History, Sparkles } from 'lucide-react';
import { MarkdownEditor } from '@/components/editor/MarkdownEditor';
import { WikiMarkdown } from '@/components/notes/WikiMarkdown';
import { VersionHistoryDialog } from '@/components/notes/VersionHistoryDialog';
import { agentMemoryGet, agentMemorySet, agentMemorySetActive, agentMemoryRestore, agentMemoryReview, type AgentMemoryReview, type MemoryScope } from '@/lib/tauri';
import type { NoteVersion } from '@/lib/types';

interface Props {
  sessionId: string;
  onClose: () => void;
  /** Called whenever the memory state (either scope) may have changed so the
   * input-area brain button can re-query and update its appearance. */
  onStateChange?: () => void;
}

type ViewMode = 'edit' | 'reading' | 'source' | 'split-h' | 'split-v';

const SAVE_DEBOUNCE_MS = 800;
/** Soft cap reminder: the memory is sent with the system prompt on EVERY
 * turn while active, so long memories cost tokens on every message. */
const LONG_MEMORY_CHARS = 2000;

const SCOPE_HINT: Record<MemoryScope, string> = {
  global: '全局记忆跨设备同步：记录用户偏好、项目约定、领域知识、经验法则',
  device: '本机记忆仅此设备：记录本机软件环境、路径、硬件等设备相关事实',
};

/** Long-term memory editor for one agent (chat session), with two scopes:
 * 「全局」(synced across devices) and 「本机」(this device only — for local
 * environment facts that would be false on other machines). Opens as a
 * modal; the memory lives outside the note tree and is only reachable here.
 * The view modes (edit / reading / source / split) mirror the note editor. */
export function AgentMemoryModal({ sessionId, onClose, onStateChange }: Props) {
  const [scope, setScope] = useState<MemoryScope>('global');
  const [content, setContent] = useState('');
  const [active, setActive] = useState(true);
  const [updatedAt, setUpdatedAt] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [mode, setMode] = useState<ViewMode>('edit');
  const [menuOpen, setMenuOpen] = useState(false);
  const [versionOpen, setVersionOpen] = useState(false);
  const [saved, setSaved] = useState<'idle' | 'saving' | 'saved'>('idle');
  // AI review: the session's own model proposes a revised document; nothing
  // is applied until the user adopts it (adoption goes through the normal
  // save path, so the pre-adoption content lands in the version history).
  const [reviewing, setReviewing] = useState(false);
  const [review, setReview] = useState<AgentMemoryReview | null>(null);
  const [reviewError, setReviewError] = useState<string | null>(null);
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const latest = useRef(content);
  latest.current = content;
  const scopeRef = useRef(scope);
  scopeRef.current = scope;

  useEffect(() => {
    let cancelled = false;
    setLoaded(false);
    setContent('');
    setUpdatedAt(null);
    setReview(null);
    setReviewError(null);
    agentMemoryGet(sessionId, scope)
      .then((m) => {
        if (cancelled) return;
        if (m) {
          setContent(m.content);
          setActive(m.active);
          setUpdatedAt(m.updated_at);
          onStateChange?.();
        }
        setLoaded(true);
      })
      .catch(() => setLoaded(true));
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, scope]);

  useEffect(() => {
    const onDown = (e: KeyboardEvent) => {
      // Let the version-history dialog consume Escape first.
      if (e.key === 'Escape' && !versionOpen) onClose();
    };
    window.addEventListener('keydown', onDown);
    return () => window.removeEventListener('keydown', onDown);
  }, [onClose, versionOpen]);

  // Close the overflow menu on outside click.
  useEffect(() => {
    if (!menuOpen) return;
    const onDown = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenuOpen(false);
      }
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [menuOpen]);

  const flushSave = useCallback((): Promise<void> => {
    if (saveTimer.current) clearTimeout(saveTimer.current);
    saveTimer.current = null;
    setSaved('saving');
    return agentMemorySet(sessionId, latest.current, scopeRef.current)
      .then(() => {
        setSaved('saved');
        setUpdatedAt(new Date().toISOString());
        onStateChange?.();
      })
      .catch(() => setSaved('idle'));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  const handleChange = (value: string) => {
    setContent(value);
    setSaved('idle');
    if (saveTimer.current) clearTimeout(saveTimer.current);
    saveTimer.current = setTimeout(flushSave, SAVE_DEBOUNCE_MS);
  };

  /** Switch the 全局/本机 tab: flush pending edits of the current scope
   * first, so a fast tab switch can never lose or cross-write content. */
  const switchScope = (next: MemoryScope) => {
    if (next === scope) return;
    flushSave().finally(() => setScope(next));
  };

  const toggleActive = () => {
    const next = !active;
    setActive(next);
    agentMemorySetActive(sessionId, next, scope)
      .then(() => onStateChange?.())
      .catch(() => {});
  };

  /** Edit/reading toggle, mirroring the note editor's header button. */
  const toggleReading = () =>
    setMode((m) => (m === 'reading' ? 'edit' : 'reading'));

  const pickMode = (m: ViewMode) => {
    setMenuOpen(false);
    setMode(m);
  };

  const handleReview = async () => {
    if (saveTimer.current) await flushSave();
    setReviewing(true);
    setReviewError(null);
    try {
      setReview(await agentMemoryReview(sessionId, scope));
    } catch (err) {
      setReviewError(String(err instanceof Error ? err.message : err));
    } finally {
      setReviewing(false);
    }
  };

  /** Adopt the revised document: route through the normal save path so the
   * pre-adoption content is snapshotted into the version history. */
  const adoptReview = () => {
    if (!review) return;
    if (saveTimer.current) {
      clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    setContent(review.revised);
    latest.current = review.revised;
    setReview(null);
    flushSave();
  };

  /** Restore a version: cancel any pending autosave first so it cannot
   * overwrite the restored content, then apply and show it. The backend
   * snapshots the pre-restore content itself. */
  const handleVersionRestore = async (version: NoteVersion) => {
    if (saveTimer.current) {
      clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    try {
      await agentMemoryRestore(sessionId, version.id, scope);
      setContent(version.content);
      latest.current = version.content;
      setSaved('saved');
    } catch {
      setSaved('idle');
    }
  };

  // Save pending edits when the modal closes.
  const handleClose = () => {
    if (saveTimer.current) flushSave();
    onClose();
  };

  // Staleness banner: a memory untouched for 90+ days is likely outdated —
  // it still gets injected every turn, so prompt a review.
  const staleDays = (() => {
    if (!updatedAt) return null;
    const days = Math.floor((Date.now() - new Date(updatedAt).getTime()) / 86400000);
    return Number.isFinite(days) && days >= 90 ? days : null;
  })();

  const editorEl = (
    <MarkdownEditor
      value={content}
      onChange={handleChange}
      // Source mode: raw markdown, no live-preview rendering.
      livePreview={mode !== 'source'}
    />
  );
  const previewEl = (
    <div className="h-full overflow-y-auto p-6 prose prose-sm prose-invert max-w-none">
      <WikiMarkdown content={content || ' '} notes={[]} onNavigate={() => {}} />
    </div>
  );

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={handleClose} />
      <div className="relative w-[760px] max-w-[94vw] h-[620px] max-h-[88vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-2 px-4 py-2.5 border-b border-surface-hover shrink-0">
          <Brain size={15} className={active ? 'text-primary' : 'text-text-secondary/50'} />
          <span className="text-sm font-medium text-text-primary">长期记忆</span>
          {/* 全局 / 本机 作用域切换 */}
          <div className="flex items-center rounded-lg border border-surface-hover overflow-hidden">
            {(['global', 'device'] as const).map((s) => (
              <button
                key={s}
                onClick={() => switchScope(s)}
                title={SCOPE_HINT[s]}
                className={`px-2.5 py-1 text-[11px] transition-colors ${
                  scope === s
                    ? 'bg-primary/15 text-primary'
                    : 'text-text-secondary hover:text-text-primary hover:bg-surface-hover'
                }`}
              >
                {s === 'global' ? '全局' : '本机'}
              </button>
            ))}
          </div>
          <span className="text-[11px] text-text-secondary/60">
            {saved === 'saving' ? (
              <span className="inline-flex items-center gap-1"><Loader2 size={11} className="animate-spin" />保存中</span>
            ) : saved === 'saved' ? (
              <span className="inline-flex items-center gap-1 text-accent"><Check size={11} />已保存</span>
            ) : null}
          </span>
          <div className="flex-1" />
          {/* AI 审查：会话自己的模型提出修订稿，用户决定是否采用 */}
          <button
            onClick={handleReview}
            disabled={reviewing || !content.trim()}
            title="AI 审查：由当前会话的模型检查过时/重复/矛盾内容并提出修订稿，采用与否由你决定"
            className="p-1.5 rounded hover:bg-surface-hover text-text-secondary hover:text-primary disabled:opacity-50 flex items-center gap-1"
          >
            {reviewing ? <Loader2 size={13} className="animate-spin" /> : <Sparkles size={13} />}
            <span className="text-[11px]">AI 审查</span>
          </button>
          {/* 编辑/阅读视图切换（与笔记编辑区一致） */}
          <button
            onClick={toggleReading}
            title={mode === 'reading' ? '当前为阅读视图，点击切换至编辑视图' : '当前为编辑视图，点击切换至阅读视图'}
            className="p-1.5 rounded hover:bg-surface-hover text-text-secondary hover:text-text-primary"
          >
            {mode === 'reading' ? <Pen size={13} /> : <BookOpen size={13} />}
          </button>
          {/* 激活/遗忘开关 */}
          <button
            onClick={toggleActive}
            title={active ? '已激活：每轮对话都会注入系统提示词，点击遗忘' : '已遗忘：内容保留但不注入，点击激活'}
            className={`relative w-9 h-5 rounded-full transition-colors ${
              active ? 'bg-primary' : 'bg-surface-hover'
            }`}
          >
            <span
              className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-all ${
                active ? 'left-[18px]' : 'left-0.5'
              }`}
            />
          </button>
          <span className="text-[11px] text-text-secondary/60 w-8">{active ? '激活' : '遗忘'}</span>
          {/* 更多视图选项 */}
          <div className="relative" ref={menuRef}>
            <button
              onClick={() => setMenuOpen((o) => !o)}
              title="更多操作"
              className="p-1.5 rounded hover:bg-surface-hover text-text-secondary hover:text-text-primary"
            >
              <MoreVertical size={13} />
            </button>
            {menuOpen && (
              <div className="absolute right-0 top-full z-50 mt-1 w-44 bg-surface border border-surface-hover rounded-lg shadow-xl py-1">
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => pickMode('source')}
                >
                  <Code size={13} />
                  <span className="flex-1">源码模式</span>
                  {mode === 'source' && <Check size={13} className="text-primary" />}
                </button>
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => pickMode('split-h')}
                >
                  <Columns2 size={13} />
                  <span className="flex-1">左右分屏</span>
                  {mode === 'split-h' && <Check size={13} className="text-primary" />}
                </button>
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => pickMode('split-v')}
                >
                  <Rows2 size={13} />
                  <span className="flex-1">上下分屏</span>
                  {mode === 'split-v' && <Check size={13} className="text-primary" />}
                </button>
                <div className="my-1 h-px bg-surface-hover" />
                <button
                  className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-text-secondary hover:bg-surface-hover hover:text-text-primary w-full text-left"
                  onClick={() => {
                    setMenuOpen(false);
                    setVersionOpen(true);
                  }}
                >
                  <History size={13} />
                  <span className="flex-1">版本历史</span>
                </button>
              </div>
            )}
          </div>
          <button
            onClick={handleClose}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭"
          >
            <X size={14} />
          </button>
        </div>

        {staleDays !== null && (
          <div className="px-4 py-1.5 text-[11px] text-amber-400/90 bg-amber-400/10 border-b border-surface-hover shrink-0">
            这份记忆已 {staleDays} 天未更新，内容可能已过时——建议审查精简；不再适用时可点右上角开关「遗忘」（内容保留但停止注入）
          </div>
        )}
        {reviewError && (
          <div className="px-4 py-1.5 text-[11px] text-red-400/90 bg-red-400/10 border-b border-surface-hover shrink-0">
            AI 审查失败：{reviewError}
          </div>
        )}

        <div className="flex-1 min-h-0 overflow-hidden">
          {review ? (
            <div className="h-full flex flex-col">
              <div className="flex-1 overflow-y-auto p-4 space-y-4">
                <section>
                  <h4 className="text-xs font-medium text-text-secondary mb-1.5">审查意见</h4>
                  <div className="text-sm text-text-primary whitespace-pre-wrap bg-surface-hover/40 rounded-lg p-3">
                    {review.notes || '（模型未给出审查意见）'}
                  </div>
                </section>
                <section>
                  <h4 className="text-xs font-medium text-text-secondary mb-1.5">修订后文档</h4>
                  <div className="text-sm text-text-primary whitespace-pre-wrap font-mono bg-surface-hover/40 rounded-lg p-3">
                    {review.revised}
                  </div>
                </section>
              </div>
              <div className="flex items-center justify-end gap-2 px-4 py-2.5 border-t border-surface-hover shrink-0">
                <span className="flex-1 text-[11px] text-text-secondary/50">
                  采用后旧内容自动存入版本历史，可随时恢复
                </span>
                <button
                  onClick={() => setReview(null)}
                  className="px-3 py-1.5 text-xs rounded-lg border border-surface-hover text-text-secondary hover:text-text-primary hover:border-text-secondary"
                >
                  放弃
                </button>
                <button
                  onClick={adoptReview}
                  className="px-3 py-1.5 text-xs rounded-lg bg-primary text-white hover:opacity-90"
                >
                  采用修订稿
                </button>
              </div>
            </div>
          ) : !loaded ? (
            <div className="flex items-center justify-center h-full text-sm text-text-secondary/60">加载中…</div>
          ) : mode === 'reading' ? (
            previewEl
          ) : mode === 'split-h' || mode === 'split-v' ? (
            <div className={`flex h-full ${mode === 'split-v' ? 'flex-col' : ''}`}>
              <div className={`min-h-0 min-w-0 ${mode === 'split-h' ? 'w-1/2' : 'h-1/2'}`}>{editorEl}</div>
              <div className={mode === 'split-h' ? 'w-px bg-surface-hover' : 'h-px bg-surface-hover'} />
              <div className={`min-h-0 min-w-0 ${mode === 'split-h' ? 'w-1/2' : 'h-1/2'}`}>{previewEl}</div>
            </div>
          ) : (
            editorEl
          )}
        </div>

        <div className="flex items-center justify-between px-4 py-1.5 border-t border-surface-hover shrink-0">
          <span className={`text-[11px] ${content.length > LONG_MEMORY_CHARS ? 'text-amber-400/90' : 'text-text-secondary/50'}`}>
            {content.length > LONG_MEMORY_CHARS
              ? `${content.length} 字 · 记忆较长，每轮对话都会携带，会增加 token 消耗`
              : `${content.length} 字`}
          </span>
          <span className="text-[11px] text-text-secondary/40">
            {SCOPE_HINT[scope]}；激活时作为系统提示词注入
          </span>
        </div>
      </div>

      {/* Version history reuses the note dialog; snapshots are stored in
          note_versions keyed by the scope's versions key. */}
      {versionOpen && (
        <VersionHistoryDialog
          current={{
            id: scope === 'device' ? `${sessionId}:local` : sessionId,
            title: scope === 'device' ? '本机记忆' : '长期记忆',
            content,
          }}
          onRestore={handleVersionRestore}
          onClose={() => setVersionOpen(false)}
        />
      )}
    </div>
  );
}
