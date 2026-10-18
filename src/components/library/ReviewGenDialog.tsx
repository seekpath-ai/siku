import { useCallback, useEffect, useRef, useState, type RefObject } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useNavigate } from '@tanstack/react-router';
import {
  AlertTriangle,
  ArrowLeft,
  BookOpen,
  Check,
  Loader2,
  PanelRight,
  Plus,
  RotateCcw,
  Save,
  Sparkles,
  Square,
  X,
} from 'lucide-react';
import { useReviewStore } from '@/stores/reviewStore';
import { useDialog } from '@/hooks/useDialog';
import { openNoteTab } from '@/lib/openNote';
import { ModelOverrideSelect } from './ModelOverrideSelect';
import { ReviewReadingPanel, type ReviewCardDraft } from './ReviewReadingPanel';
import { ReviewOutlineEditor } from './ReviewOutlineEditor';
import {
  reviewCardUpdate,
  reviewCreate,
  reviewGenerateCancel,
  reviewGenerateOutline,
  reviewGenerateRetrySection,
  reviewGenerateSave,
  reviewGenerateStart,
  reviewGet,
  reviewReadRetry,
  reviewReadStart,
  type ReviewAudit,
  type ReviewPaperCard,
  type ReviewProgress,
  type ReviewSection,
  type ReviewSnapshot,
  type ReviewSource,
} from '@/lib/tauri';

type Step = 'form' | 'reading' | 'outline' | 'run';

const WORD_OPTIONS = [3000, 5000, 8000, 10000];
const MAX_OUTPUT_TOKENS = 32768;
const TERMINAL_STAGES = new Set(['done', 'partial', 'cancelled', 'error', 'failed', 'completed', 'succeeded']);
const RUN_ACTIVE_STAGES = new Set(['section', 'write_section', 'writing', 'revision', 'assemble']);

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

function writtenSubsections(text: string): string[] {
  return text
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.startsWith('### '))
    .map((line) => line.replace(/^#+\s*/, '').trim())
    .filter(Boolean);
}

function formatTokens(value: number): string {
  return value >= 1000 ? `${Math.round(value / 1000)}k` : String(value);
}

function stepFromSnapshot(snapshot: ReviewSnapshot): Step {
  const stage = snapshot.stage.toLowerCase();
  if (
    snapshot.noteId ||
    snapshot.audit ||
    snapshot.finalRawMarkdown ||
    snapshot.sections.length > 0 ||
    RUN_ACTIVE_STAGES.has(stage) ||
    stage === 'partial'
  ) {
    return 'run';
  }
  if (snapshot.outline.length > 0 || snapshot.synthesis || stage === 'outline' || stage === 'synthesize') {
    return 'outline';
  }
  return 'reading';
}

function snapshotRunActive(snapshot: ReviewSnapshot): boolean {
  if (stepFromSnapshot(snapshot) !== 'run') return false;
  if (TERMINAL_STAGES.has(snapshot.status.toLowerCase()) || TERMINAL_STAGES.has(snapshot.stage.toLowerCase())) {
    return false;
  }
  return (
    snapshot.status.toLowerCase() === 'running' ||
    snapshot.sections.some((section) => section.status === 'running') ||
    RUN_ACTIVE_STAGES.has(snapshot.stage.toLowerCase())
  );
}

function hydrateSections(snapshot: ReviewSnapshot, previous: SectionRun[]): SectionRun[] {
  if (snapshot.sections.length === 0) return previous;
  const length = Math.max(previous.length, snapshot.outline.length, snapshot.sections.length);
  const next = Array.from({ length }, (_, index) => previous[index] ?? emptyRun());
  const zeroBased = snapshot.sections.some((result) => result.sectionIndex === 0);
  for (const result of snapshot.sections) {
    const index = zeroBased ? result.sectionIndex : result.sectionIndex - 1;
    if (index < 0) continue;
    const current = next[index] ?? emptyRun();
    const status =
      result.status === 'running'
        ? 'running'
        : result.status === 'done'
          ? 'done'
          : result.status === 'failed'
            ? 'failed'
            : current.status === 'running'
              ? 'running'
              : 'idle';
    next[index] = {
      ...current,
      status,
      attempt: Math.max(current.attempt, Math.max(0, result.attempts - 1)),
      error: result.status === 'failed' ? result.error ?? '生成失败' : undefined,
      text: result.status === 'running' ? current.text || result.rawMarkdown : result.rawMarkdown || current.text,
    };
  }
  return next;
}

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
  reasoning: string;
  title?: string;
  onFollow?: () => void;
  previewRef: RefObject<HTMLDivElement | null>;
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
              type="button"
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

function auditPaperLabel(paperId: string, sources: ReviewSource[]): string {
  const source = sources.find((item) => item.paperId === paperId);
  return source ? `#${source.paperIndex} ${source.title}` : paperId;
}

function AuditPaperList({ paperIds, sources, empty }: { paperIds: string[]; sources: ReviewSource[]; empty: string }) {
  if (paperIds.length === 0) return <p className="text-[11px] text-text-secondary/45">{empty}</p>;
  return (
    <ul className="space-y-1">
      {paperIds.map((paperId) => (
        <li key={paperId} className="text-[11px] text-text-secondary/75 break-words">
          {auditPaperLabel(paperId, sources)}
        </li>
      ))}
    </ul>
  );
}

function AuditReport({ audit, sources }: { audit: ReviewAudit; sources: ReviewSource[] }) {
  return (
    <section className="rounded-lg border border-surface-hover/70 p-3 space-y-3">
      <div>
        <h2 className="text-[12px] font-medium text-text-primary">引用与覆盖审计</h2>
        <p className="text-[11px] text-text-secondary/50">仅统计正文中的实际引用，不把已选文献等同于已引用文献。</p>
      </div>
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-2">
        {[
          ['已选文献', audit.selectedCount],
          ['全文来源', audit.fulltextCount],
          ['摘要来源', audit.abstractCount],
          ['不可用', audit.unusableCount],
        ].map(([label, value]) => (
          <div key={label} className="rounded bg-background/60 px-2 py-1.5 text-center">
            <p className="text-[15px] text-text-primary">{value}</p>
            <p className="text-[10px] text-text-secondary/50">{label}</p>
          </div>
        ))}
      </div>
      <div className="grid sm:grid-cols-2 gap-3">
        <div className="space-y-1.5">
          <h3 className="text-[11px] text-emerald-400/80">实际引用（{audit.citedPaperIds.length}）</h3>
          <AuditPaperList paperIds={audit.citedPaperIds} sources={sources} empty="尚未检测到实际引用文献。" />
        </div>
        <div className="space-y-1.5">
          <h3 className="text-[11px] text-amber-400/90">未引用（{audit.uncitedPaperIds.length}）</h3>
          <AuditPaperList paperIds={audit.uncitedPaperIds} sources={sources} empty="无未引用文献。" />
        </div>
      </div>
      <div className="space-y-1.5">
        <h3 className="text-[11px] text-text-secondary">无效引用标记（{audit.invalidMarkers.length}）</h3>
        {audit.invalidMarkers.length === 0 ? (
          <p className="text-[11px] text-text-secondary/45">未检测到无效引用标记。</p>
        ) : (
          <div className="flex flex-wrap gap-1">
            {audit.invalidMarkers.map((marker, index) => (
              <code key={`${marker}-${index}`} className="rounded bg-red-400/10 px-1.5 py-0.5 text-[10px] text-red-300">
                {marker}
              </code>
            ))}
          </div>
        )}
      </div>
      <div className="space-y-1.5">
        <h3 className="text-[11px] text-text-secondary">警告（{audit.warnings.length}）</h3>
        {audit.warnings.length === 0 ? (
          <p className="text-[11px] text-text-secondary/45">无额外警告。</p>
        ) : (
          <ul className="space-y-1">
            {audit.warnings.map((warning, index) => (
              <li key={index} className="flex items-start gap-1.5 text-[11px] leading-relaxed text-amber-300/85">
                <AlertTriangle size={11} className="mt-0.5 shrink-0" />
                {warning}
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

export function ReviewGenDialog() {
  const { open, paperIds, runId, setRunId, resetRun, close } = useReviewStore();
  const { alert } = useDialog();
  const navigate = useNavigate();

  const [step, setStep] = useState<Step>('form');
  const [snapshot, setSnapshot] = useState<ReviewSnapshot | null>(null);
  const [loadingSnapshot, setLoadingSnapshot] = useState(false);
  const [question, setQuestion] = useState('');
  const [scope, setScope] = useState('');
  const [language, setLanguage] = useState('中文');
  const [targetWords, setTargetWords] = useState(5000);
  const [providerId, setProviderId] = useState('');
  const [outline, setOutline] = useState<ReviewSection[]>([]);
  const [busy, setBusy] = useState(false);
  const [runActive, setRunActive] = useState(false);
  const [progress, setProgress] = useState<ReviewProgress | null>(null);
  const [noteId, setNoteId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [partialInfo, setPartialInfo] = useState<string | null>(null);
  const [outlineText, setOutlineText] = useState('');
  const [outlineReasoning, setOutlineReasoning] = useState('');
  const [revisionText, setRevisionText] = useState('');
  const [revisionReasoning, setRevisionReasoning] = useState('');
  const [sections, setSections] = useState<SectionRun[]>([]);
  const [selected, setSelected] = useState<number | null>(null);
  const [showPreview, setShowPreview] = useState(true);
  const [cardDrafts, setCardDrafts] = useState<Record<string, ReviewCardDraft>>({});
  const [expandedPaperId, setExpandedPaperId] = useState<string | null>(null);
  const [retryingPaperId, setRetryingPaperId] = useState<string | null>(null);
  const [savingPaperId, setSavingPaperId] = useState<string | null>(null);
  const outlineDirtyRef = useRef(false);
  const previewRef = useRef<HTMLDivElement>(null);

  const applySnapshot = useCallback((next: ReviewSnapshot, restoreStep = false) => {
    setSnapshot(next);
    setQuestion(next.question);
    setScope(next.scope);
    setLanguage(next.language);
    setTargetWords(next.targetWords);
    setProviderId(next.providerId ?? '');
    if (!outlineDirtyRef.current) setOutline(next.outline);
    setCardDrafts((previous) => {
      const drafts: Record<string, ReviewCardDraft> = {};
      for (const source of next.sources) {
        if (!source.card) continue;
        const current = previous[source.paperId];
        drafts[source.paperId] = current?.dirty ? current : { card: source.card, dirty: false };
      }
      return drafts;
    });
    setSections((previous) => hydrateSections(next, previous));
    setNoteId(next.noteId);
    setError(next.error);

    const stage = next.stage.toLowerCase();
    const status = next.status.toLowerCase();
    if (restoreStep) {
      setStep(stepFromSnapshot(next));
      setRunActive(snapshotRunActive(next));
      setSelected(null);
    } else if (stepFromSnapshot(next) === 'run') {
      if (TERMINAL_STAGES.has(stage) || TERMINAL_STAGES.has(status)) setRunActive(false);
      else if (snapshotRunActive(next)) setRunActive(true);
    }
    if (stage === 'partial' || status === 'partial') {
      setPartialInfo(next.error ?? '部分小节未生成');
    } else if (TERMINAL_STAGES.has(stage) || TERMINAL_STAGES.has(status)) {
      setPartialInfo(null);
    }
  }, []);

  const refreshSnapshot = useCallback(
    async (id: string, restoreStep = false) => {
      const next = await reviewGet(id);
      if (useReviewStore.getState().runId === id) applySnapshot(next, restoreStep);
      return next;
    },
    [applySnapshot]
  );

  const restoreSnapshot = useCallback(async () => {
    if (!runId) return;
    setLoadingSnapshot(true);
    try {
      await refreshSnapshot(runId, true);
    } catch (err) {
      if (useReviewStore.getState().runId === runId) setError(`恢复综述任务失败: ${err}`);
    } finally {
      if (useReviewStore.getState().runId === runId) setLoadingSnapshot(false);
    }
  }, [refreshSnapshot, runId]);

  useEffect(() => {
    if (open && runId) void restoreSnapshot();
  }, [open, restoreSnapshot, runId]);

  useEffect(() => {
    if (!open || !runId) return;
    const unlisten = listen<ReviewProgress>('library:review-progress', (event) => {
      const current = event.payload;
      if (!current.runId || current.runId !== runId) return;
      setProgress(current);

      if (current.delta || current.reasoning) {
        if (current.stage.includes('outline')) {
          if (current.delta) setOutlineText((text) => text + current.delta);
          if (current.reasoning) setOutlineReasoning((text) => text + current.reasoning);
        } else if (current.stage.includes('revision')) {
          if (current.delta) setRevisionText((text) => text + current.delta);
          if (current.reasoning) setRevisionReasoning((text) => text + current.reasoning);
        } else if (current.sectionIndex > 0) {
          const index = current.sectionIndex - 1;
          setSections((previous) => {
            const section = previous[index];
            if (!section) return previous;
            const next = [...previous];
            next[index] = {
              ...section,
              text: current.delta ? section.text + current.delta : section.text,
              reasoning: current.reasoning ? section.reasoning + current.reasoning : section.reasoning,
            };
            return next;
          });
        }
        return;
      }

      if (current.sectionStatus && current.sectionIndex > 0) {
        const index = current.sectionIndex - 1;
        setSections((previous) => {
          const section = previous[index];
          if (!section) return previous;
          const attempt = current.attempt ?? section.attempt;
          const restart = attempt > section.attempt;
          const next = [...previous];
          next[index] = {
            ...section,
            status:
              current.sectionStatus === 'running' ? 'running' : current.sectionStatus === 'done' ? 'done' : 'failed',
            attempt,
            maxTokens: current.maxTokens ?? section.maxTokens,
            error: current.sectionStatus === 'failed' ? current.error ?? '生成失败' : undefined,
            text: restart ? '' : section.text,
            reasoning: restart ? '' : section.reasoning,
          };
          return next;
        });
        if (current.sectionStatus === 'running') setRunActive(true);
      }

      if (current.stage === 'section' || current.stage === 'revision' || current.stage === 'assemble') {
        setRunActive(true);
        setPartialInfo(null);
      } else if (current.stage === 'read_ready') {
        setRunActive(false);
        setPartialInfo(null);
      } else if (current.stage === 'done') {
        if (current.noteId) setNoteId(current.noteId);
        setError(null);
        setPartialInfo(null);
        setRunActive(false);
      } else if (current.stage === 'partial') {
        setPartialInfo(current.message ?? '部分小节未生成');
        setRunActive(false);
      } else if (current.stage === 'error') {
        setError(current.message ?? '任务执行失败');
        setRunActive(false);
      } else if (current.stage === 'cancelled') {
        setPartialInfo(current.message ?? '已取消');
        setRunActive(false);
        setSections((previous) =>
          previous.map((section) =>
            section.status === 'running' ? { ...section, status: section.text ? 'failed' : 'idle' } : section
          )
        );
      }

      void refreshSnapshot(runId).catch(() => undefined);
    });
    return () => {
      unlisten.then((unlistenFn) => unlistenFn());
    };
  }, [open, refreshSnapshot, runId]);

  const runningIndex = (() => {
    const index = sections.findIndex((section) => section.status === 'running');
    return index >= 0 ? index + 1 : null;
  })();
  const drawerSection = selected ?? runningIndex ?? (sections.length > 0 ? sections.length : null);
  const drawerData = drawerSection != null ? sections[drawerSection - 1] : null;
  const revisionPreview = step === 'run' && progress?.stage === 'revision';
  const drawerTitle =
    step === 'outline'
      ? '生成过程 · 大纲'
      : revisionPreview
        ? '生成过程 · 全文修订'
        : drawerSection != null && outline[drawerSection - 1]
          ? `第 ${drawerSection} 节${drawerData && drawerData.attempt > 0 ? ` · 第 ${drawerData.attempt + 1} 次尝试` : ''}`
          : '生成过程';
  const drawerText = step === 'outline' ? outlineText : revisionPreview ? revisionText : drawerData?.text ?? '';
  const drawerReasoning =
    step === 'outline' ? outlineReasoning : revisionPreview ? revisionReasoning : drawerData?.reasoning ?? '';

  useEffect(() => {
    previewRef.current?.scrollTo({ top: previewRef.current.scrollHeight });
  }, [drawerReasoning, drawerText]);

  if (!open) return null;

  const sources = snapshot?.sources ?? [];
  const activePaperIds = snapshot?.paperIds ?? paperIds;
  const settledReading = sources.length > 0 && sources.every((source) => source.status === 'done' || source.status === 'failed');
  const usableCardCount = sources.filter((source) => source.status === 'done' && source.card).length;
  const derivedDonePapers = sources.filter((source) => source.status === 'done' || source.status === 'failed').length;
  const totalPapers = progress?.totalPapers ?? (sources.length || activePaperIds.length);
  const donePapers = Math.min(totalPapers, Math.max(progress?.donePapers ?? 0, derivedDonePapers));
  const canEnterOutline = settledReading && usableCardCount > 0 && Boolean(snapshot?.synthesis);
  const canContinueReading =
    sources.length === 0 ||
    (!settledReading && snapshot?.status !== 'reading' && !sources.some((source) => source.status === 'running')) ||
    (settledReading && usableCardCount > 0 && !snapshot?.synthesis && snapshot?.stage !== 'synthesis');
  const doneCount = sections.filter((section) => section.status === 'done').length;
  const failedCount = sections.filter((section) => section.status === 'failed').length;

  const startReading = async () => {
    if (!question.trim()) {
      await alert('请填写研究问题');
      return;
    }
    if (paperIds.length === 0) {
      await alert('请至少选择一篇文献');
      return;
    }
    setBusy(true);
    setError(null);
    setPartialInfo(null);
    let created: ReviewSnapshot | null = null;
    try {
      created = await reviewCreate(paperIds, question.trim(), scope.trim(), language, targetWords, providerId || null);
      outlineDirtyRef.current = false;
      setCardDrafts({});
      setExpandedPaperId(null);
      setOutlineText('');
      setOutlineReasoning('');
      setRevisionText('');
      setRevisionReasoning('');
      setSections([]);
      setProgress(null);
      setRunActive(false);
      setRunId(created.runId);
      applySnapshot(created, true);
      await reviewReadStart(created.runId);
    } catch (err) {
      if (!created || useReviewStore.getState().runId === created.runId) setError(`启动阅读失败: ${err}`);
    } finally {
      setBusy(false);
    }
  };

  const continueReading = async () => {
    if (!runId) return;
    setBusy(true);
    setError(null);
    try {
      await reviewReadStart(runId);
      await refreshSnapshot(runId);
    } catch (err) {
      setError(`启动阅读失败: ${err}`);
    } finally {
      setBusy(false);
    }
  };

  const retryReading = async (paperId: string) => {
    if (!runId) return;
    setRetryingPaperId(paperId);
    setError(null);
    try {
      await reviewReadRetry(runId, paperId);
      outlineDirtyRef.current = false;
      await refreshSnapshot(runId);
    } catch (err) {
      if (useReviewStore.getState().runId === runId) setError(`重试阅读失败: ${err}`);
    } finally {
      setRetryingPaperId(null);
    }
  };

  const changeCard = (paperId: string, card: ReviewPaperCard) => {
    setCardDrafts((previous) => ({ ...previous, [paperId]: { card, dirty: true } }));
  };

  const saveCard = async (paperId: string) => {
    const draft = cardDrafts[paperId];
    if (!runId || !draft) return;
    setSavingPaperId(paperId);
    setError(null);
    try {
      const next = await reviewCardUpdate(runId, paperId, { ...draft.card, paperId, edited: true });
      if (useReviewStore.getState().runId !== runId) return;
      outlineDirtyRef.current = false;
      applySnapshot(next);
      const savedCard = next.sources.find((source) => source.paperId === paperId)?.card ?? draft.card;
      setCardDrafts((previous) => ({ ...previous, [paperId]: { card: savedCard, dirty: false } }));
    } catch (err) {
      if (useReviewStore.getState().runId === runId) setError(`保存证据卡失败: ${err}`);
    } finally {
      setSavingPaperId(null);
    }
  };

  const makeOutline = async () => {
    if (!runId) return;
    setBusy(true);
    setError(null);
    setPartialInfo(null);
    setOutlineText('');
    setOutlineReasoning('');
    outlineDirtyRef.current = true;
    try {
      const result = await reviewGenerateOutline(runId);
      if (useReviewStore.getState().runId !== runId) return;
      outlineDirtyRef.current = false;
      setOutline(result);
      await refreshSnapshot(runId);
    } catch (err) {
      if (useReviewStore.getState().runId === runId) setError(`生成大纲失败: ${err}`);
    } finally {
      setBusy(false);
    }
  };

  const changeOutline = (next: ReviewSection[]) => {
    outlineDirtyRef.current = true;
    setOutline(next);
  };

  const startGeneration = async () => {
    if (!runId) return;
    const cleaned = outline
      .map((section) => ({
        title: section.title.trim(),
        points: section.points.trim(),
        subsections: section.subsections.map((item) => item.trim()).filter(Boolean),
        paperIds: [...section.paperIds],
      }))
      .filter((section) => section.title);
    if (cleaned.length === 0) {
      await alert('大纲为空，请至少保留一节');
      return;
    }
    outlineDirtyRef.current = true;
    setOutline(cleaned);
    setStep('run');
    setRunActive(true);
    setProgress(null);
    setError(null);
    setPartialInfo(null);
    setNoteId(null);
    setSelected(null);
    setRevisionText('');
    setRevisionReasoning('');
    setSections(cleaned.map(() => emptyRun()));
    try {
      await reviewGenerateStart(runId, cleaned);
      if (useReviewStore.getState().runId === runId) outlineDirtyRef.current = false;
    } catch (err) {
      if (useReviewStore.getState().runId === runId) {
        setRunActive(false);
        setError(`启动生成失败: ${err}`);
      }
    }
  };

  const retrySection = async (sectionIndex: number) => {
    if (!runId) return;
    setRunActive(true);
    setError(null);
    try {
      await reviewGenerateRetrySection(runId, sectionIndex);
    } catch (err) {
      if (useReviewStore.getState().runId === runId) {
        setRunActive(false);
        setError(`重试小节失败: ${err}`);
      }
    }
  };

  const cancelGeneration = async () => {
    if (!runId) return;
    setBusy(true);
    try {
      await reviewGenerateCancel(runId);
      await refreshSnapshot(runId);
    } catch (err) {
      if (useReviewStore.getState().runId === runId) setError(`取消失败: ${err}`);
    } finally {
      setBusy(false);
    }
  };

  const saveReview = async () => {
    if (!runId) return;
    setBusy(true);
    setError(null);
    try {
      const id = await reviewGenerateSave(runId);
      if (useReviewStore.getState().runId !== runId) return;
      setNoteId(id);
      await refreshSnapshot(runId);
    } catch (err) {
      if (useReviewStore.getState().runId === runId) setError(`保存失败: ${err}`);
    } finally {
      setBusy(false);
    }
  };

  const newReview = () => {
    resetRun();
    outlineDirtyRef.current = false;
    setSnapshot(null);
    setStep('form');
    setQuestion('');
    setScope('');
    setLanguage('中文');
    setTargetWords(5000);
    setProviderId('');
    setOutline([]);
    setBusy(false);
    setRunActive(false);
    setProgress(null);
    setNoteId(null);
    setError(null);
    setPartialInfo(null);
    setOutlineText('');
    setOutlineReasoning('');
    setRevisionText('');
    setRevisionReasoning('');
    setSections([]);
    setSelected(null);
    setCardDrafts({});
    setExpandedPaperId(null);
    setRetryingPaperId(null);
    setSavingPaperId(null);
  };

  const openNote = () => {
    if (noteId) openNoteTab(navigate, { id: noteId, title: `综述：${question.slice(0, 20)}` });
    close();
  };

  const stepLabels: Record<Step, string> = {
    form: '设置',
    reading: '阅读与证据卡',
    outline: '综合与大纲',
    run: '写作与审计',
  };

  return (
    <div className="fixed inset-0 z-[6000] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={close} />
      <div className="relative w-[94vw] max-w-[1080px] h-[88vh] max-h-[88vh] min-h-0 mx-4 flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-2 px-4 py-3 border-b border-surface-hover shrink-0">
          <Sparkles size={15} className="text-primary" />
          <span className="text-sm font-medium text-text-primary">AI 综述</span>
          <span className="text-[11px] text-text-secondary/60">{stepLabels[step]}</span>
          <span className="text-[11px] text-text-secondary/45">· {activePaperIds.length} 篇文献</span>
          {step === 'run' && sections.length > 0 && (
            <span className="text-[11px] text-text-secondary/60">
              · 完成 {doneCount}/{outline.length}
              {failedCount > 0 && <span className="text-red-400"> · 失败 {failedCount}</span>}
            </span>
          )}
          <div className="ml-auto flex items-center gap-1">
            {runId && (
              <button
                type="button"
                onClick={newReview}
                className="flex items-center gap-1 px-2 py-1 rounded text-[11px] text-text-secondary/65 hover:text-text-primary hover:bg-surface-hover"
                title="重置当前界面并新建综述"
              >
                <Plus size={12} />
                新建综述
              </button>
            )}
            {(step === 'outline' || step === 'run') && (
              <button
                type="button"
                onClick={() => setShowPreview((value) => !value)}
                title="生成过程"
                className={`p-1 rounded hover:bg-surface-hover ${showPreview ? 'text-primary bg-primary/10' : 'text-text-secondary/60'}`}
              >
                <PanelRight size={14} />
              </button>
            )}
            <button
              type="button"
              onClick={close}
              title={runActive ? '关闭窗口不会中断任务' : '关闭'}
              className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover"
            >
              <X size={14} />
            </button>
          </div>
        </div>

        {error && (
          <div className="flex items-start gap-2 px-4 py-2 border-b border-red-400/20 bg-red-400/5 text-[12px] leading-relaxed text-red-300 shrink-0">
            <AlertTriangle size={13} className="mt-0.5 shrink-0" />
            <span>{error}</span>
          </div>
        )}

        <div className="flex-1 min-h-0 flex flex-col overflow-hidden px-4 py-3">
          {runId && !snapshot ? (
            <div className="flex-1 flex flex-col items-center justify-center gap-3 text-center">
              {loadingSnapshot ? (
                <>
                  <Loader2 size={20} className="animate-spin text-primary" />
                  <p className="text-[12px] text-text-secondary">正在恢复综述任务…</p>
                </>
              ) : (
                <>
                  <p className="text-[12px] text-text-secondary">暂时无法读取任务快照，已保留当前 runId。</p>
                  <button
                    type="button"
                    onClick={() => void restoreSnapshot()}
                    className="px-3 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
                  >
                    重新加载
                  </button>
                </>
              )}
            </div>
          ) : (
            <>
              {step === 'form' && (
                <div className="flex-1 min-h-0 overflow-y-auto space-y-3">
                  <div className="space-y-1.5">
                    <label className="text-xs text-text-secondary">研究问题</label>
                    <textarea
                      value={question}
                      onChange={(event) => setQuestion(event.target.value)}
                      rows={3}
                      placeholder="如：大语言模型在软件工程任务中的应用进展如何？"
                      className="w-full bg-background border border-surface-hover rounded-lg px-3 py-2 text-[13px] text-text-primary outline-none focus:border-primary/50 resize-none"
                    />
                  </div>
                  <div className="space-y-1.5">
                    <label className="text-xs text-text-secondary">范围限定（可选）</label>
                    <input
                      value={scope}
                      onChange={(event) => setScope(event.target.value)}
                      placeholder="如：近五年、实证研究、中文写作"
                      className="w-full bg-background border border-surface-hover rounded-lg px-3 py-2 text-[13px] text-text-primary outline-none focus:border-primary/50"
                    />
                  </div>
                  <div className="flex gap-3">
                    <div className="space-y-1.5 flex-1">
                      <label className="text-xs text-text-secondary">写作语言</label>
                      <select
                        value={language}
                        onChange={(event) => setLanguage(event.target.value)}
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
                        onChange={(event) => setTargetWords(Number(event.target.value))}
                        className="w-full bg-background border border-surface-hover rounded-lg px-3 py-2 text-[13px] text-text-primary outline-none"
                      >
                        {WORD_OPTIONS.map((words) => (
                          <option key={words} value={words}>
                            约 {words} 字
                          </option>
                        ))}
                      </select>
                    </div>
                  </div>
                  <div className="space-y-1.5">
                    <label className="text-xs text-text-secondary">模型</label>
                    <ModelOverrideSelect value={providerId} onChange={setProviderId} disabled={busy} className="w-full" />
                  </div>
                  <div className="rounded-lg border border-surface-hover/60 bg-background/30 px-3 py-2 text-[11px] leading-relaxed text-text-secondary/55">
                    将对 {paperIds.length} 篇文献逐篇预检：优先使用全文，无全文时降级到摘要，无可用内容会明确标记为不可用。
                  </div>
                </div>
              )}

              {step === 'reading' && snapshot && (
                <ReviewReadingPanel
                  sources={sources}
                  drafts={cardDrafts}
                  expandedPaperId={expandedPaperId}
                  retryingPaperId={retryingPaperId}
                  savingPaperId={savingPaperId}
                  donePapers={donePapers}
                  totalPapers={totalPapers}
                  synthesisReady={Boolean(snapshot.synthesis)}
                  progressMessage={progress?.message}
                  onToggle={(paperId) => setExpandedPaperId((current) => (current === paperId ? null : paperId))}
                  onCardChange={changeCard}
                  onSave={(paperId) => void saveCard(paperId)}
                  onRetry={(paperId) => void retryReading(paperId)}
                />
              )}

              {step === 'outline' && snapshot && (
                <div className="flex gap-3 flex-1 min-h-0">
                  <ReviewOutlineEditor outline={outline} synthesis={snapshot.synthesis} sources={sources} onChange={changeOutline} />
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

              {step === 'run' && snapshot && (
                <div className="flex gap-3 flex-1 min-h-0">
                  <div className="flex-1 min-w-0 overflow-y-auto space-y-2 pr-0.5">
                    <ul className="space-y-1">
                      {outline.map((section, index) => {
                        const run = sections[index] ?? emptyRun();
                        const isRunning = run.status === 'running';
                        const subsections = writtenSubsections(run.text);
                        const nextNeed = Math.min(run.maxTokens * 2 || 0, MAX_OUTPUT_TOKENS);
                        const pinned = selected === index + 1;
                        return (
                          <li key={index} className="space-y-1">
                            <div
                              onClick={() => {
                                if (run.status === 'idle') return;
                                setSelected(pinned ? null : index + 1);
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
                                  <span className={run.status === 'idle' ? 'text-text-secondary/50' : 'text-text-primary'}>
                                    {section.title}
                                  </span>
                                  {run.attempt > 0 && (
                                    <span className="text-[10px] text-text-secondary/40 shrink-0">第 {run.attempt + 1} 次</span>
                                  )}
                                </div>
                                {isRunning && subsections.length > 0 && (
                                  <p className="text-[10px] text-text-secondary/50 truncate">正在写 {subsections[subsections.length - 1]}</p>
                                )}
                                {run.status === 'failed' && subsections.length > 0 && (
                                  <p className="text-[10px] text-text-secondary/50 truncate">
                                    已写出：{subsections.join('、')}（未完成）
                                  </p>
                                )}
                              </div>
                              {!runActive && !busy && (run.status === 'failed' || run.status === 'idle') && (
                                <button
                                  type="button"
                                  onClick={(event) => {
                                    event.stopPropagation();
                                    void retrySection(snapshot.sections[index]?.sectionIndex ?? index);
                                  }}
                                  title={
                                    run.maxTokens === 0
                                      ? '生成该节'
                                      : nextNeed > run.maxTokens
                                        ? `重试并把输出额度从 ${formatTokens(run.maxTokens)} 加倍到 ${formatTokens(nextNeed)}`
                                        : '重试该节（输出额度已到上限）'
                                  }
                                  className="flex items-center gap-1 px-2 py-0.5 rounded border border-surface-hover text-[11px] text-text-secondary hover:bg-surface-hover hover:text-text-primary shrink-0"
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

                    {(progress?.stage === 'revision' || snapshot.stage === 'revision') && (
                      <div className="flex items-start gap-2 rounded-lg border border-primary/20 bg-primary/5 px-3 py-2 text-[12px] text-text-secondary">
                        {runActive && <Loader2 size={13} className="animate-spin text-primary mt-0.5 shrink-0" />}
                        <span>{runActive ? '正在进行全文修订：统一术语、过渡与全局引用…' : '全文修订阶段已结束或中断，请结合审计警告检查。'}</span>
                      </div>
                    )}
                    {progress?.stage === 'assemble' && <p className="text-[12px] text-text-secondary">正在汇总并保存笔记…</p>}
                    {snapshot.finalRawMarkdown && !noteId && !runActive && (
                      <p className="text-[12px] text-amber-300">全文修订稿已生成，但尚未保存为笔记。</p>
                    )}
                    {noteId && <p className="text-[12px] text-emerald-400">综述已保存为笔记，引用编号可点击溯源。</p>}
                    {!runActive && failedCount > 0 && (
                      <p className="text-[12px] text-amber-400">仍有 {failedCount} 节未完成，可单独重试；保存时会保留部分完成状态。</p>
                    )}
                    {partialInfo && <p className="text-[12px] text-amber-400">{partialInfo}</p>}

                    {snapshot.audit ? (
                      <AuditReport audit={snapshot.audit} sources={sources} />
                    ) : (
                      (noteId || snapshot.finalRawMarkdown) && (
                        <div className="rounded-lg border border-dashed border-surface-hover px-3 py-3 text-[11px] text-text-secondary/50">
                          暂无可用的覆盖审计报告；这不表示引用检查已通过。
                        </div>
                      )
                    )}
                  </div>

                  <ProcessDrawer
                    show={showPreview}
                    busy={revisionPreview ? runActive : isBusySection(sections, drawerSection)}
                    text={drawerText}
                    reasoning={drawerReasoning}
                    title={drawerTitle}
                    onFollow={selected != null ? () => setSelected(null) : undefined}
                    previewRef={previewRef}
                  />
                </div>
              )}
            </>
          )}
        </div>

        <div className="flex justify-end gap-2 px-4 py-3 border-t border-surface-hover shrink-0">
          {step === 'form' && !runId && (
            <button
              type="button"
              onClick={() => void startReading()}
              disabled={busy}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-black text-[13px] font-medium hover:bg-primary/90 disabled:opacity-50"
            >
              {busy && <Loader2 size={13} className="animate-spin" />}
              开始阅读
            </button>
          )}
          {step === 'reading' && snapshot && (
            <>
              {canContinueReading && (
                <button
                  type="button"
                  onClick={() => void continueReading()}
                  disabled={busy}
                  className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-surface-hover text-[13px] text-text-secondary hover:bg-surface-hover disabled:opacity-50"
                >
                  {busy && <Loader2 size={13} className="animate-spin" />}
                  {sources.length === 0 ? '开始阅读' : settledReading && !snapshot.synthesis ? '生成跨文献综合' : '继续阅读'}
                </button>
              )}
              <button
                type="button"
                onClick={() => {
                  setError(null);
                  setStep('outline');
                }}
                disabled={!canEnterOutline || busy}
                title={canEnterOutline ? '检查或生成大纲' : '阅读、跨文献综合完成且至少有一张证据卡后才能进入大纲'}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-black text-[13px] font-medium hover:bg-primary/90 disabled:opacity-40"
              >
                进入大纲
              </button>
            </>
          )}
          {step === 'outline' && snapshot && (
            <>
              <button
                type="button"
                onClick={() => setStep('reading')}
                disabled={busy}
                className="flex items-center gap-1 px-3 py-1.5 rounded-lg border border-surface-hover text-[13px] text-text-secondary hover:bg-surface-hover disabled:opacity-40"
              >
                <ArrowLeft size={13} />
                阅读检查
              </button>
              <button
                type="button"
                onClick={() => void makeOutline()}
                disabled={busy}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-surface-hover text-[13px] text-text-secondary hover:bg-surface-hover disabled:opacity-50"
              >
                {busy ? <Loader2 size={13} className="animate-spin" /> : <Sparkles size={13} />}
                {outline.length > 0 ? '重新生成大纲' : '生成大纲'}
              </button>
              <button
                type="button"
                onClick={() => void startGeneration()}
                disabled={busy || outline.length === 0}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-black text-[13px] font-medium hover:bg-primary/90 disabled:opacity-40"
              >
                <Sparkles size={13} />
                开始生成
              </button>
            </>
          )}
          {step === 'run' && snapshot && (
            <>
              {runActive && (
                <button
                  type="button"
                  onClick={() => void cancelGeneration()}
                  disabled={busy}
                  className="flex items-center gap-1 px-3 py-1.5 rounded-lg border border-surface-hover text-[13px] text-red-400 hover:bg-surface-hover disabled:opacity-40"
                >
                  <Square size={12} />
                  停止
                </button>
              )}
              {!runActive && !noteId && doneCount > 0 && (
                <button
                  type="button"
                  onClick={() => void saveReview()}
                  disabled={busy}
                  className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-surface-hover text-[13px] text-text-secondary hover:bg-surface-hover disabled:opacity-40"
                  title={failedCount > 0 ? '保存已生成的节，缺失小节会保留为未完成状态' : '将当前综述保存为笔记'}
                >
                  {busy ? <Loader2 size={13} className="animate-spin" /> : <Save size={13} />}
                  {failedCount > 0 ? '保存已完成部分' : '保存综述'}
                </button>
              )}
              {noteId && !runActive && failedCount > 0 && (
                <span className="self-center text-[11px] text-text-secondary/60 mr-1">还有 {failedCount} 节未完成，重试后可再次保存更新</span>
              )}
              {noteId && (
                <button
                  type="button"
                  onClick={openNote}
                  className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-black text-[13px] font-medium hover:bg-primary/90"
                >
                  <BookOpen size={13} />
                  打开笔记
                </button>
              )}
              {!runActive && !noteId && (
                <button
                  type="button"
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

function isBusySection(sections: SectionRun[], index: number | null): boolean {
  if (index == null) return false;
  return sections[index - 1]?.status === 'running';
}
