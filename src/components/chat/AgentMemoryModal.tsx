import { useState, useEffect, useRef, useCallback } from 'react';
import { Brain, X, Loader2, Check } from 'lucide-react';
import { MarkdownEditor } from '@/components/editor/MarkdownEditor';
import { agentMemoryGet, agentMemorySet, agentMemorySetActive } from '@/lib/tauri';

interface Props {
  sessionId: string;
  onClose: () => void;
  /** Called whenever the active flag changes so the input-area brain button
   * can update its appearance. */
  onActiveChange?: (active: boolean) => void;
}

const SAVE_DEBOUNCE_MS = 800;
/** Soft cap reminder: the memory is sent with the system prompt on EVERY
 * turn while active, so long memories cost tokens on every message. */
const LONG_MEMORY_CHARS = 2000;

/** Long-term memory editor for one agent (chat session). Opens as a modal;
 * the memory lives outside the note tree and is only reachable here. */
export function AgentMemoryModal({ sessionId, onClose, onActiveChange }: Props) {
  const [content, setContent] = useState('');
  const [active, setActive] = useState(true);
  const [loaded, setLoaded] = useState(false);
  const [saved, setSaved] = useState<'idle' | 'saving' | 'saved'>('idle');
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const latest = useRef(content);
  latest.current = content;

  useEffect(() => {
    let cancelled = false;
    agentMemoryGet(sessionId)
      .then((m) => {
        if (cancelled) return;
        if (m) {
          setContent(m.content);
          setActive(m.active);
          onActiveChange?.(m.active);
        }
        setLoaded(true);
      })
      .catch(() => setLoaded(true));
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  useEffect(() => {
    const onDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onDown);
    return () => window.removeEventListener('keydown', onDown);
  }, [onClose]);

  const flushSave = useCallback(() => {
    if (saveTimer.current) clearTimeout(saveTimer.current);
    saveTimer.current = null;
    setSaved('saving');
    agentMemorySet(sessionId, latest.current)
      .then(() => setSaved('saved'))
      .catch(() => setSaved('idle'));
  }, [sessionId]);

  const handleChange = (value: string) => {
    setContent(value);
    setSaved('idle');
    if (saveTimer.current) clearTimeout(saveTimer.current);
    saveTimer.current = setTimeout(flushSave, SAVE_DEBOUNCE_MS);
  };

  const toggleActive = () => {
    const next = !active;
    setActive(next);
    onActiveChange?.(next);
    agentMemorySetActive(sessionId, next).catch(() => {});
  };

  // Save pending edits when the modal closes.
  const handleClose = () => {
    if (saveTimer.current) flushSave();
    onClose();
  };

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={handleClose} />
      <div className="relative w-[720px] max-w-[92vw] h-[600px] max-h-[86vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-2 px-4 py-2.5 border-b border-surface-hover shrink-0">
          <Brain size={15} className={active ? 'text-primary' : 'text-text-secondary/50'} />
          <span className="text-sm font-medium text-text-primary">长期记忆</span>
          <span className="text-[11px] text-text-secondary/60">
            {saved === 'saving' ? (
              <span className="inline-flex items-center gap-1"><Loader2 size={11} className="animate-spin" />保存中</span>
            ) : saved === 'saved' ? (
              <span className="inline-flex items-center gap-1 text-accent"><Check size={11} />已保存</span>
            ) : null}
          </span>
          <div className="flex-1" />
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
          <button
            onClick={handleClose}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭"
          >
            <X size={14} />
          </button>
        </div>

        <div className="flex-1 min-h-0 overflow-hidden">
          {loaded ? (
            <MarkdownEditor value={content} onChange={handleChange} livePreview={false} />
          ) : (
            <div className="flex items-center justify-center h-full text-sm text-text-secondary/60">加载中…</div>
          )}
        </div>

        <div className="flex items-center justify-between px-4 py-1.5 border-t border-surface-hover shrink-0">
          <span className={`text-[11px] ${content.length > LONG_MEMORY_CHARS ? 'text-amber-400/90' : 'text-text-secondary/50'}`}>
            {content.length > LONG_MEMORY_CHARS
              ? `${content.length} 字 · 记忆较长，每轮对话都会携带，会增加 token 消耗`
              : `${content.length} 字`}
          </span>
          <span className="text-[11px] text-text-secondary/40">
            记录智能体身份、职责、用户偏好、经验法则等；激活时作为系统提示词注入
          </span>
        </div>
      </div>
    </div>
  );
}
