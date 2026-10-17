import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useNavigate } from '@tanstack/react-router';
import {
  X, Loader2, Sparkles, ArrowLeft, ArrowUp, ArrowDown, Plus, Trash2, Check, Square, BookOpen,
  PanelRight,
} from 'lucide-react';
import { useReviewStore } from '@/stores/reviewStore';
import { useDialog } from '@/hooks/useDialog';
import { openNoteTab } from '@/lib/openNote';
import { ModelOverrideSelect } from './ModelOverrideSelect';
import {
  reviewGenerateOutline,
  reviewGenerateStart,
  reviewGenerateCancel,
  type ReviewSection,
  type ReviewProgress,
} from '@/lib/tauri';

type Step = 'form' | 'outline' | 'run';

const WORD_OPTIONS = [3000, 5000, 8000, 10000];

/** Right drawer showing the model's live output (outline JSON or the section
 *  being written) — a long call never looks like a stuck spinner. */
function ProcessDrawer({
  show,
  busy,
  text,
  reasoning,
  previewRef,
}: {
  show: boolean;
  busy: boolean;
  text: string;
  /** Thinking-model reasoning stream, rendered dimmed above the content. */
  reasoning: string;
  previewRef: React.RefObject<HTMLDivElement | null>;
}) {
  return (
    <aside
      className={`shrink-0 rounded-lg flex flex-col overflow-hidden transition-all duration-200 ${
        show ? 'w-[300px] border border-surface-hover/70' : 'w-0'
      }`}
    >
      <div className="w-[300px] flex flex-col h-full">
        <div className="flex items-center gap-1.5 px-3 py-1.5 border-b border-surface-hover/60 text-[11px] text-text-secondary/60">
          生成过程
          {busy && <Loader2 size={10} className="animate-spin" />}
        </div>
        <div
          ref={previewRef}
          className="flex-1 min-h-0 overflow-y-auto px-3 py-2 whitespace-pre-wrap font-mono text-[11px] leading-relaxed text-text-secondary/85"
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
  const [providerId, setProviderId] = useState('');
  const [streamText, setStreamText] = useState('');
  const [reasoningText, setReasoningText] = useState('');
  const [showPreview, setShowPreview] = useState(true);
  const previewRef = useRef<HTMLDivElement>(null);

  // Keep the live preview pinned to its newest output.
  useEffect(() => {
    previewRef.current?.scrollTo({ top: previewRef.current.scrollHeight });
  }, [streamText, reasoningText]);

  // Reset whenever the dialog is (re)opened with a paper set.
  useEffect(() => {
    if (!open) return;
    setStep('form');
    setOutline([]);
    setBusy(false);
    setProgress(null);
    setNoteId(null);
    setError(null);
  }, [open, paperIds]);

  // Pipeline progress events (active only during the run step).
  useEffect(() => {
    if (!open) return;
    const un = listen<ReviewProgress>('library:review-progress', (e) => {
      const p = e.payload;
      setProgress(p);
      if (p.stage === 'section' && !p.delta && !p.reasoning) {
        // New section starting: reset the live preview.
        setStreamText('');
        setReasoningText('');
      }
      if (p.delta) {
        setStreamText((t) => t + p.delta);
      }
      if (p.reasoning) {
        setReasoningText((t) => t + p.reasoning);
      }
      if (p.stage === 'done') {
        setNoteId(p.noteId);
        setBusy(false);
      } else if (p.stage === 'error') {
        setError(p.message ?? '生成失败');
        setBusy(false);
      } else if (p.stage === 'cancelled') {
        setBusy(false);
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
    setStreamText('');
    setReasoningText('');
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
    setNoteId(null);
    setStreamText('');
    setReasoningText('');
    try {
      await reviewGenerateStart(paperIds, question.trim(), language, targetWords, cleaned, providerId || null);
    } catch (err) {
      setBusy(false);
      setError(`启动失败: ${err}`);
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

  return (
    <div className="fixed inset-0 z-[6000] flex items-center justify-center">
      <div
        className="absolute inset-0 bg-black/50 backdrop-blur-sm"
        onClick={busy && step === 'run' ? undefined : close}
      />
      <div className="relative w-[92vw] max-w-[720px] max-h-[84vh] mx-4 flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-2 px-4 py-3 border-b border-surface-hover shrink-0">
          <Sparkles size={15} className="text-primary" />
          <span className="text-sm font-medium text-text-primary">AI 综述</span>
          <span className="text-[11px] text-text-secondary/60">{paperIds.length} 篇文献</span>
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
              disabled={busy && step === 'run'}
              className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover disabled:opacity-40"
            >
              <X size={14} />
            </button>
          </div>
        </div>

        <div className="flex-1 min-h-0 flex flex-col overflow-hidden px-4 py-3">
          {step === 'form' && (
            <div className="flex gap-3 flex-1 min-h-0">
              <div className="flex-1 min-w-0 overflow-y-auto space-y-3">
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
              {(busy || streamText || reasoningText) && (
                <ProcessDrawer show={showPreview} busy={busy} text={streamText} reasoning={reasoningText} previewRef={previewRef} />
              )}
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
              <div className="flex-1 min-w-0 overflow-y-auto space-y-3">
                <ul className="space-y-1.5">
                  {outline.map((s, i) => {
                    const current = progress?.stage === 'section' && progress.sectionIndex === i + 1;
                    const done =
                      noteId != null ||
                      (progress != null &&
                        (progress.sectionIndex > i + 1 || ['assemble', 'done'].includes(progress.stage)));
                    return (
                      <li key={i} className="flex items-center gap-2 text-[13px]">
                        {done ? (
                          <Check size={13} className="text-emerald-400 shrink-0" />
                        ) : current ? (
                          <Loader2 size={13} className="animate-spin text-primary shrink-0" />
                        ) : (
                          <span className="w-[13px] h-[13px] rounded-full border border-surface-hover shrink-0" />
                        )}
                        <span className={done || current ? 'text-text-primary' : 'text-text-secondary/50'}>
                          {s.title}
                        </span>
                      </li>
                    );
                  })}
                </ul>
                {progress?.stage === 'assemble' && (
                  <p className="text-[12px] text-text-secondary">正在汇总并保存笔记…</p>
                )}
                {noteId && (
                  <p className="text-[12px] text-emerald-400">综述已生成并保存为笔记，引用编号可点击溯源。</p>
                )}
                {error && <p className="text-[12px] text-red-400">{error}</p>}
              </div>

              {/* Right drawer: live stream of the section being written — a
                  long generation no longer looks like a stuck spinner. */}
              <ProcessDrawer show={showPreview} busy={busy} text={streamText} reasoning={reasoningText} previewRef={previewRef} />
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
