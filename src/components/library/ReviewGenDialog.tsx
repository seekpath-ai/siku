import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useNavigate } from '@tanstack/react-router';
import {
  X, Loader2, Sparkles, ArrowLeft, ArrowUp, ArrowDown, Plus, Trash2, Check, Square, BookOpen,
  PanelRight, RotateCcw, Save, AlertTriangle,
} from 'lucide-react';
import { useReviewStore } from '@/stores/reviewStore';
import { useDialog } from '@/hooks/useDialog';
import { openNoteTab } from '@/lib/openNote';
import { ModelOverrideSelect } from './ModelOverrideSelect';
import {
  reviewGenerateOutline,
  reviewGenerateStart,
  reviewGenerateCancel,
  reviewGenerateRetrySection,
  reviewGenerateSave,
  type ReviewSection,
  type ReviewProgress,
} from '@/lib/tauri';

type Step = 'form' | 'outline' | 'run';

const WORD_OPTIONS = [3000, 5000, 8000, 10000];
/** Mirrors core::review_gen::MAX_OUTPUT_TOKENS — retry doubling stops here. */
const MAX_OUTPUT_TOKENS = 32768;

/** Per-section run state. The tick marks are derived from THIS, never from the
 *  global progress coordinate (which used to be zeroed by an error event and
 *  wiped every completed check mark). */
interface SectionRun {
  status: 'idle' | 'running' | 'done' | 'failed';
  error?: string;
  attempt: number;
  maxTokens: number;
  text: string;
  reasoning: string;
}

const emptyRun = (): SectionRun => ({
  status: 'idle',
  attempt: 0,
  maxTokens: 0,
  text: '',
  reasoning: '',
});

/** `### 小节标题` headings already written — shows which in-section step the
 *  model is on without splitting the generation into separate calls. */
function writtenSubsections(text: string): string[] {
  return text
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l.startsWith('### '))
    .map((l) => l.replace(/^#+\s*/, '').trim())
    .filter(Boolean);
}

function formatTokens(n: number): string {
  return n >= 1000 ? `${Math.round(n / 1000)}k` : String(n);
}

/** Right drawer showing the model's live output (outline JSON or one section) —
 *  a long call never looks like a stuck spinner. */
function ProcessDrawer({
  show,
  busy,
  text,
  reasoning,
  title,
  onFollow,
  previewRef,
}: {
  show: boolean;
  busy: boolean;
  text: string;
  /** Thinking-model reasoning stream, rendered dimmed above the content. */
  reasoning: string;
  title?: string;
  /** Provided when the user pinned a section: jump back to the live one. */
  onFollow?: () => void;
  previewRef: React.RefObject<HTMLDivElement | null>;
}) {
  return (
    <aside
      className={`shrink-0 min-h-0 max-h-full rounded-lg flex flex-col overflow-hidden transition-all duration-200 ${
        show ? 'w-[300px] border border-surface-hover/70' : 'w-0'
      }`}
    >
      <div className="w-[300px] min-h-0 flex flex-col h-full">
        <div className="flex items-center gap-1.5 px-3 py-1.5 border-b border-surface-hover/60 text-[11px] text-text-secondary/60">
          <span className="truncate">{title ?? '生成过程'}</span>
          {busy && <Loader2 size={10} className="animate-spin shrink-0" />}
          {onFollow && (
            <button
              onClick={onFollow}
              className="ml-auto shrink-0 px-1.5 py-0.5 rounded text-[10px] text-primary hover:bg-surface-hover"
            >
              跟随最新
            </button>
          )}
        </div>
        <div
          ref={previewRef}
          className="flex-1 min-h-0 min-w-0 overflow-y-auto px-3 py-2 whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed text-text-secondary/85"
        >
          {reasoning && (
            <div className="italic text-text-secondary/45 border-b border-dashed border-surface-hover/50 pb-1.5 mb-1.5">
              {reasoning}
            </div>
          )}
          {text || (reasoning ? '' : '（等待模型输出…）')}
        </div>
      </div>
    </aside>
  );
}

/** AI 综述: form → editable outline → section-by-section generation with
 *  clickable citation traceability, saved as a note. */
export function ReviewGenDialog() {
  const { open, paperIds, close } = useReviewStore();
  const { alert } = useDialog();
  const navigate = useNavigate();

  const [step, setStep] = useState<Step>('form');
  const [question, setQuestion] = useState('');
  const [scope, setScope] = useState('');
  const [language, setLanguage] = useState('中文');
  const [targetWords, setTargetWords] = useState(5000);
  const [outline, setOutline] = useState<ReviewSection[]>([]);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<ReviewProgress | null>(null);
  const [noteId, setNoteId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [partialInfo, setPartialInfo] = useState<string | null>(null);
  const [providerId, setProviderId] = useState('');
  // Outline generation streams into its own buffers (the run step has per-section
  // buffers, so the two never overwrite each other).
  const [outlineText, setOutlineText] = useState('');
  const [outlineReasoning, setOutlineReasoning] = useState('');
  const [sections, setSections] = useState<SectionRun[]>([]);
  const [selected, setSelected] = useState<number | null>(null);
  const [showPreview, setShowPreview] = useState(true);
  const previewRef = useRef<HTMLDivElement>(null);

  // Which section the drawer is showing: the pinned one, else the live one.
  const runningIndex = (() => {
    const i = sections.findIndex((s) => s.status === 'running');
    return i >= 0 ? i + 1 : null;
  })();
  const drawerSection = selected ?? runningIndex ?? (sections.length > 0 ? sections.length : null);
  const drawerData = drawerSection != null ? sections[drawerSection - 1] : null;
  const drawerTitle =
    step === 'form'
      ? '生成过程 · 大纲'
      : drawerSection != null && outline[drawerSection - 1]
        ? `第 ${drawerSection} 节${drawerData && drawerData.attempt > 0 ? ` · 第 ${drawerData.attempt + 1} 次尝试` : ''}`
        : '生成过程';

  const drawerText = step === 'form' ? outlineText : drawerData?.text ?? '';
  const drawerReasoning = step === 'form' ? outlineReasoning : drawerData?.reasoning ?? '';

  // Keep the live preview pinned to its newest output.
  useEffect(() => {
    previewRef.current?.scrollTo({ top: previewRef.current.scrollHeight });
  }, [drawerText, drawerReasoning]);

  // Reset whenever the dialog is (re)opened with a paper set.
  useEffect(() => {
    if (!open) return;
    setStep('form');
    setOutline([]);
    setBusy(false);
    setProgress(null);
    setNoteId(null);
    setError(null);
    setPartialInfo(null);
    setOutlineText('');
    setOutlineReasoning('');
    setSections([]);
    setSelected(null);
  }, [open, paperIds]);

  // Pipeline progress events.
  useEffect(() => {
    if (!open) return;
    const un = listen<ReviewProgress>('library:review-progress', (e) => {
      const p = e.payload;
      setProgress(p);

      // Stream beats: outline (index 0) vs. a concrete section.
      if (p.delta || p.reasoning) {
        if (p.stage === 'outline' || p.sectionIndex === 0) {
          if (p.delta) setOutlineText((t) => t + p.delta);
          if (p.reasoning) setOutlineReasoning((t) => t + p.reasoning);
        } else {
          const idx = p.sectionIndex - 1;
          setSections((prev) => {
            const cur = prev[idx];
            if (!cur) return prev;
            const next = [...prev];
            next[idx] = {
              ...cur,
              text: p.delta ? cur.text + p.delta : cur.text,
              reasoning: p.reasoning ? cur.reasoning + p.reasoning : cur.reasoning,
            };
            return next;
          });
        }
        return;
      }

      if (p.sectionStatus) {
        const idx = p.sectionIndex - 1;
        setSections((prev) => {
          const cur = prev[idx];
          if (!cur) return prev;
          const attempt = p.attempt ?? cur.attempt;
          const restart = attempt > cur.attempt;
          const next = [...prev];
          next[idx] = {
            ...cur,
            status: p.sectionStatus === 'running' ? 'running' : p.sectionStatus === 'done' ? 'done' : 'failed',
            attempt,
            maxTokens: p.maxTokens ?? cur.maxTokens,
            error: p.sectionStatus === 'failed' ? p.error ?? '生成失败' : undefined,
            // A new attempt starts a fresh buffer for that section only.
            text: restart ? '' : cur.text,
            reasoning: restart ? '' : cur.reasoning,
          };
          return next;
        });
      }

      if (p.stage === 'assemble') {
        setPartialInfo(null);
      } else if (p.stage === 'done') {
        if (p.noteId) setNoteId(p.noteId);
        setError(null);
        setPartialInfo(null);
        setBusy(false);
      } else if (p.stage === 'partial') {
        setPartialInfo(p.message ?? '部分小节未生成');
        setBusy(false);
      } else if (p.stage === 'error') {
        // Pipeline-level failure (no session to retry against).
        setError(p.message ?? '生成失败');
        setBusy(false);
      } else if (p.stage === 'cancelled') {
        setPartialInfo(p.message ?? '已取消');
        setBusy(false);
        // Nothing is running any more: no section may keep a spinner.
        setSections((prev) =>
          prev.map((s) => (s.status === 'running' ? { ...s, status: s.text ? 'failed' : 'idle' } : s))
        );
      }
    });
    return () => {
      un.then((fn) => fn());
    };
  }, [open]);

  if (!open) return null;

  const makeOutline = async () => {
    if (!question.trim()) {
      await alert('请填写研究问题');
      return;
    }
    setBusy(true);
    setError(null);
    setPartialInfo(null);
    setOutlineText('');
    setOutlineReasoning('');
    try {
      const result = await reviewGenerateOutline(paperIds, question.trim(), scope.trim(), language, targetWords, providerId || null);
      setOutline(result);
      setStep('outline');
    } catch (err) {
      await alert(`生成大纲失败: ${err}`);
    } finally {
      setBusy(false);
    }
  };

  const startGeneration = async () => {
    const cleaned = outline
      .map((s) => ({
        title: s.title.trim(),
        points: s.points.trim(),
        subsections: s.subsections.map((x) => x.trim()).filter(Boolean),
      }))
      .filter((s) => s.title);
    if (cleaned.length === 0) {
      await alert('大纲为空，请至少保留一节');
      return;
    }
    setOutline(cleaned);
    setStep('run');
    setBusy(true);
    setProgress(null);
    setError(null);
    setPartialInfo(null);
    setNoteId(null);
    setSelected(null);
    setSections(cleaned.map(() => emptyRun()));
    try {
      await reviewGenerateStart(paperIds, question.trim(), language, targetWords, cleaned, providerId || null);
    } catch (err) {
      setBusy(false);
      setError(`启动失败: ${err}`);
    }
  };

  const retrySection = async (index: number) => {
    setBusy(true);
    setError(null);
    try {
      await reviewGenerateRetrySection(index);
    } catch (err) {
      setBusy(false);
      await alert(`重试失败: ${err}`);
    }
  };

  const savePartial = async () => {
    setBusy(true);
    try {
      const id = await reviewGenerateSave();
      setNoteId(id);
      setPartialInfo(null);
    } catch (err) {
      await alert(`保存失败: ${err}`);
    } finally {
      setBusy(false);
    }
  };

  const moveSection = (i: number, delta: number) => {
    setOutline((prev) => {
      const j = i + delta;
      if (j < 0 || j >= prev.length) return prev;
      const next = [...prev];
      [next[i], next[j]] = [next[j], next[i]];
      return next;
    });
  };

  const openNote = () => {
    if (noteId) openNoteTab(navigate, { id: noteId, title: `综述：${question.slice(0, 20)}` });
    close();
  };

  const doneCount = sections.filter((s) => s.status === 'done').length;
  const failedCount = sections.filter((s) => s.status === 'failed').length;

  return (
    <div className="fixed inset-0 z-[6000] flex items-center justify-center">
      {/* Closing is always allowed — a stuck model call must never trap the
          window. The run keeps going in the backend; 停止 interrupts it now. */}
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={close} />
      {/* Fixed height for every step: the dialog used to grow line by line as
          content streamed in (max-h only caps it, it does not pin it). */}
      <div className="relative w-[92vw] max-w-[760px] h-[84vh] max-h-[84vh] min-h-0 mx-4 flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-2 px-4 py-3 border-b border-surface-hover shrink-0">
          <Sparkles size={15} className="text-primary" />
          <span className="text-sm font-medium text-text-primary">AI 综述</span>
          <span className="text-[11px] text-text-secondary/60">{paperIds.length} 篇文献</span>
          {step === 'run' && sections.length > 0 && (
            <span className="text-[11px] text-text-secondary/60">
              · 完成 {doneCount}/{sections.length}
              {failedCount > 0 && <span className="text-red-400"> · 失败 {failedCount}</span>}
            </span>
          )}
          <div className="ml-auto flex items-center gap-1">
            {(step === 'run' || (step === 'form' && busy)) && (
              <button
                onClick={() => setShowPreview((v) => !v)}
                title="生成过程"
                className={`p-1 rounded hover:bg-surface-hover ${showPreview ? 'text-primary bg-primary/10' : 'text-text-secondary/60'}`}
              >
                <PanelRight size={14} />
              </button>
            )}
            <button
              onClick={close}
              title={busy ? '关闭窗口不会中断生成（可用底部「停止」取消）' : '关闭'}
              className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover"
            >
              <X size={14} />
            </button>
          </div>
        </div>

        <div className="flex-1 min-h-0 flex flex-col overflow-hidden px-4 py-3">
          {step === 'form' && (
            <div className="flex gap-3 flex-1 min-h-0">
              <div className="flex-1 min-w-0 min-h-0 overflow-y-auto space-y-3">
                <div className="space-y-1.5">
                  <label className="text-xs text-text-secondary">研究问题</label>
                  <textarea
                    value={question}
                    onChange={(e) => setQuestion(e.target.value)}
                    rows={3}
                    placeholder="如：大语言模型在软件工程任务中的应用进展如何？"
                    className="w-full bg-background border border-surface-hover rounded-lg px-3 py-2 text-[13px] text-text-primary outline-none focus:border-primary/50 resize-none"
                  />
                </div>
                <div className="space-y-1.5">
                  <label className="text-xs text-text-secondary">范围限定（可选）</label>
                  <input
                    value={scope}
                    onChange={(e) => setScope(e.target.value)}
                    placeholder="如：近五年、实证研究、中文写作"
                    className="w-full bg-background border border-surface-hover rounded-lg px-3 py-2 text-[13px] text-text-primary outline-none focus:border-primary/50"
                  />
                </div>
                <div className="flex gap-3">
                  <div className="space-y-1.5 flex-1">
                    <label className="text-xs text-text-secondary">写作语言</label>
                    <select
                      value={language}
                      onChange={(e) => setLanguage(e.target.value)}
                      className="w-full bg-background border border-surface-hover rounded-lg px-3 py-2 text-[13px] text-text-primary outline-none"
                    >
                      <option>中文</option>
                      <option>English</option>
                    </select>
                  </div>
                  <div className="space-y-1.5 flex-1">
                    <label className="text-xs text-text-secondary">目标字数</label>
                    <select
                      value={targetWords}
                      onChange={(e) => setTargetWords(Number(e.target.value))}
                      className="w-full bg-background border border-surface-hover rounded-lg px-3 py-2 text-[13px] text-text-primary outline-none"
                    >
                      {WORD_OPTIONS.map((w) => (
                        <option key={w} value={w}>
                          约 {w} 字
                        </option>
                      ))}
                    </select>
                  </div>
                </div>
                <div className="space-y-1.5">
                  <label className="text-xs text-text-secondary">模型</label>
                  <ModelOverrideSelect value={providerId} onChange={setProviderId} disabled={busy} className="w-full" />
                </div>
              </div>
              <ProcessDrawer
                show={showPreview && (busy || Boolean(outlineText) || Boolean(outlineReasoning))}
                busy={busy}
                text={outlineText}
                reasoning={outlineReasoning}
                title={drawerTitle}
                previewRef={previewRef}
              />
            </div>
          )}

          {step === 'outline' && (
            <div className="flex-1 min-h-0 overflow-y-auto space-y-2.5">
              <p className="text-[11px] text-text-secondary/60">
                调整目录后再开始：可改标题与要点、增删章节、调整顺序。
              </p>
              <ModelOverrideSelect value={providerId} onChange={setProviderId} className="w-full" />
              {outline.map((s, i) => (
                <div key={i} className="rounded-lg border border-surface-hover/70 p-2.5 space-y-1.5">
                  <div className="flex items-center gap-1.5">
                    <span className="text-[11px] text-text-secondary/50 w-4">{i + 1}.</span>
                    <input
                      value={s.title}
                      onChange={(e) =>
                        setOutline((prev) => prev.map((x, j) => (j === i ? { ...x, title: e.target.value } : x)))
                      }
                      className="flex-1 min-w-0 bg-background border border-surface-hover rounded px-2 py-1 text-[12px] text-text-primary outline-none focus:border-primary/50"
                    />
                    <button onClick={() => moveSection(i, -1)} className="p-1 rounded text-text-secondary/60 hover:bg-surface-hover" title="上移">
                      <ArrowUp size={12} />
                    </button>
                    <button onClick={() => moveSection(i, 1)} className="p-1 rounded text-text-secondary/60 hover:bg-surface-hover" title="下移">
                      <ArrowDown size={12} />
                    </button>
                    <button
                      onClick={() => setOutline((prev) => prev.filter((_, j) => j !== i))}
                      className="p-1 rounded text-text-secondary/60 hover:text-red-400 hover:bg-surface-hover"
                      title="删除该节"
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                  <textarea
                    value={s.points}
                    onChange={(e) =>
                      setOutline((prev) => prev.map((x, j) => (j === i ? { ...x, points: e.target.value } : x)))
                    }
                    rows={2}
                    placeholder="本节要点"
                    className="w-full bg-background border border-surface-hover rounded px-2 py-1 text-[11px] text-text-primary outline-none focus:border-primary/50 resize-none"
                  />
                  {s.subsections.map((sub, k) => (
                    <div key={k} className="flex items-center gap-1.5 pl-5">
                      <span className="text-[11px] text-text-secondary/40">{i + 1}.{k + 1}</span>
                      <input
                        value={sub}
                        onChange={(e) =>
                          setOutline((prev) =>
                            prev.map((x, j) =>
                              j === i ? { ...x, subsections: x.subsections.map((y, m) => (m === k ? e.target.value : y)) } : x
                            )
                          )
                        }
                        placeholder="小节标题"
                        className="flex-1 min-w-0 bg-background border border-surface-hover rounded px-2 py-0.5 text-[11px] text-text-primary outline-none focus:border-primary/50"
                      />
                      <button
                        onClick={() =>
                          setOutline((prev) =>
                            prev.map((x, j) => (j === i ? { ...x, subsections: x.subsections.filter((_, m) => m !== k) } : x))
                          )
                        }
                        className="p-0.5 rounded text-text-secondary/50 hover:text-red-400 hover:bg-surface-hover"
                        title="删除小节"
                      >
                        <Trash2 size={11} />
                      </button>
                    </div>
                  ))}
                  <button
                    onClick={() =>
                      setOutline((prev) => prev.map((x, j) => (j === i ? { ...x, subsections: [...x.subsections, ''] } : x)))
                    }
                    className="ml-5 flex items-center gap-1 px-1.5 py-0.5 rounded border border-dashed border-surface-hover text-[11px] text-text-secondary/70 hover:bg-surface-hover"
                  >
                    <Plus size={10} />
                    添加小节
                  </button>
                </div>
              ))}
              <button
                onClick={() => setOutline((prev) => [...prev, { title: '', points: '', subsections: [] }])}
                className="w-full flex items-center justify-center gap-1 px-2 py-1.5 rounded-lg border border-dashed border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
              >
                <Plus size={12} />
                添加章节
              </button>
            </div>
          )}

          {step === 'run' && (
            <div className="flex gap-3 flex-1 min-h-0">
              <div className="flex-1 min-w-0 overflow-y-auto space-y-2">
                <ul className="space-y-1">
                  {outline.map((s, i) => {
                    const run = sections[i] ?? emptyRun();
                    const isRunning = run.status === 'running';
                    const subs = writtenSubsections(run.text);
                    const nextNeed = Math.min(run.maxTokens * 2 || 0, MAX_OUTPUT_TOKENS);
                    const pinned = selected === i + 1;
                    return (
                      <li key={i} className="space-y-1">
                        <div
                          onClick={() => {
                            if (run.status === 'idle') return;
                            setSelected(pinned ? null : i + 1);
                          }}
                          className={`flex items-start gap-2 text-[13px] rounded px-1.5 py-1 -mx-1.5 transition-colors ${
                            run.status === 'idle' ? '' : 'cursor-pointer hover:bg-surface-hover/50'
                          } ${pinned ? 'bg-surface-hover/60' : ''}`}
                          title={run.status === 'idle' ? undefined : '查看该节的生成过程'}
                        >
                          {run.status === 'done' ? (
                            <Check size={13} className="text-emerald-400 shrink-0 mt-0.5" />
                          ) : isRunning ? (
                            <Loader2 size={13} className="animate-spin text-primary shrink-0 mt-0.5" />
                          ) : run.status === 'failed' ? (
                            <AlertTriangle size={13} className="text-red-400 shrink-0 mt-0.5" />
                          ) : (
                            <span className="w-[13px] h-[13px] rounded-full border border-surface-hover shrink-0 mt-0.5" />
                          )}
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-2">
                              <span
                                className={
                                  run.status === 'done' || isRunning
                                    ? 'text-text-primary'
                                    : run.status === 'failed'
                                      ? 'text-text-primary'
                                      : 'text-text-secondary/50'
                                }
                              >
                                {s.title}
                              </span>
                              {run.attempt > 0 && (
                                <span className="text-[10px] text-text-secondary/40 shrink-0">
                                  第 {run.attempt + 1} 次
                                </span>
                              )}
                            </div>
                            {isRunning && subs.length > 0 && (
                              <p className="text-[10px] text-text-secondary/50 truncate">
                                正在写 {subs[subs.length - 1]}
                              </p>
                            )}
                            {run.status === 'failed' && subs.length > 0 && (
                              <p className="text-[10px] text-text-secondary/50 truncate">
                                已写出：{subs.join('、')}（未完成）
                              </p>
                            )}
                          </div>
                          {!busy && (run.status === 'failed' || run.status === 'idle') && (
                            <button
                              onClick={(e) => {
                                e.stopPropagation();
                                void retrySection(i + 1);
                              }}
                              title={
                                run.maxTokens === 0
                                  ? '生成该节'
                                  : nextNeed > run.maxTokens
                                    ? `重试并把输出额度从 ${formatTokens(run.maxTokens)} 加倍到 ${formatTokens(nextNeed)}`
                                    : '重试该节（输出额度已到上限，建议换模型或调低该节目标字数）'
                              }
                              className="flex items-center gap-1 px-2 py-0.5 rounded border border-surface-hover text-[11px] text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:opacity-40 shrink-0"
                            >
                              <RotateCcw size={10} />
                              {run.maxTokens === 0 ? '生成' : '重试'}
                              {run.maxTokens > 0 && nextNeed > run.maxTokens && (
                                <span className="text-[10px] text-text-secondary/50">
                                  {formatTokens(run.maxTokens)} → {formatTokens(nextNeed)}
                                </span>
                              )}
                            </button>
                          )}
                        </div>
                        {run.status === 'failed' && run.error && (
                          <p className="text-[11px] text-red-400 pl-[21px] leading-snug">{run.error}</p>
                        )}
                      </li>
                    );
                  })}
                </ul>
                {progress?.stage === 'assemble' && (
                  <p className="text-[12px] text-text-secondary">正在汇总并保存笔记…</p>
                )}
                {noteId && (
                  <p className="text-[12px] text-emerald-400">
                    综述已生成并保存为笔记，引用编号可点击溯源。
                  </p>
                )}
                {partialInfo && !noteId && (
                  <p className="text-[12px] text-amber-400">{partialInfo}</p>
                )}
                {error && <p className="text-[12px] text-red-400">{error}</p>}
              </div>

              <ProcessDrawer
                show={showPreview}
                busy={isBusySection(sections, drawerSection)}
                text={drawerText}
                reasoning={drawerReasoning}
                title={drawerTitle}
                onFollow={selected != null ? () => setSelected(null) : undefined}
                previewRef={previewRef}
              />
            </div>
          )}
        </div>

        <div className="flex justify-end gap-2 px-4 py-3 border-t border-surface-hover shrink-0">
          {step === 'form' && (
            <button
              onClick={() => void makeOutline()}
              disabled={busy}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-black text-[13px] font-medium hover:bg-primary/90 disabled:opacity-50"
            >
              {busy && <Loader2 size={13} className="animate-spin" />}
              生成大纲
            </button>
          )}
          {step === 'outline' && (
            <>
              <button
                onClick={() => setStep('form')}
                className="flex items-center gap-1 px-3 py-1.5 rounded-lg border border-surface-hover text-[13px] text-text-secondary hover:bg-surface-hover"
              >
                <ArrowLeft size={13} />
                上一步
              </button>
              <button
                onClick={() => void startGeneration()}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-black text-[13px] font-medium hover:bg-primary/90"
              >
                <Sparkles size={13} />
                开始生成
              </button>
            </>
          )}
          {step === 'run' && (
            <>
              {busy && (
                <button
                  onClick={() => void reviewGenerateCancel()}
                  className="flex items-center gap-1 px-3 py-1.5 rounded-lg border border-surface-hover text-[13px] text-red-400 hover:bg-surface-hover"
                >
                  <Square size={12} />
                  停止
                </button>
              )}
              {!busy && !noteId && failedCount > 0 && doneCount > 0 && (
                <button
                  onClick={() => void savePartial()}
                  className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-surface-hover text-[13px] text-text-secondary hover:bg-surface-hover"
                  title="把已生成的节保存为笔记，缺失的节会写明占位"
                >
                  <Save size={13} />
                  保存已完成部分
                </button>
              )}
              {noteId && !busy && failedCount > 0 && (
                <span className="self-center text-[11px] text-text-secondary/60 mr-1">
                  还有 {failedCount} 节未完成，重试后会自动更新笔记
                </span>
              )}
              {noteId && (
                <button
                  onClick={openNote}
                  className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-black text-[13px] font-medium hover:bg-primary/90"
                >
                  <BookOpen size={13} />
                  打开笔记
                </button>
              )}
              {!busy && !noteId && (
                <button
                  onClick={close}
                  className="px-3 py-1.5 rounded-lg border border-surface-hover text-[13px] text-text-secondary hover:bg-surface-hover"
                >
                  关闭
                </button>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

/** True when the drawer's section is the one currently being written. */
function isBusySection(sections: SectionRun[], index: number | null): boolean {
  if (index == null) return false;
  const s = sections[index - 1];
  return s?.status === 'running';
}
