import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { Check, Loader2 } from 'lucide-react';
import { useAiTableStore } from '@/stores/aiTableStore';
import { useReviewStore } from '@/stores/reviewStore';
import { useAiClassifyStore } from '@/stores/aiClassifyStore';
import {
  AI_CLASSIFY_PROGRESS_EVENT,
  type AiTableProgress,
  type ClassifyProgress,
  type ReviewProgress,
} from '@/lib/tauri';

interface ChipState {
  label: string;
  running: boolean;
}

const REVIEW_TERMINAL_STAGES = new Set(['done', 'partial', 'cancelled', 'error']);

/** Global entry point for long-running library AI tasks (综述 / 表格列 /
 *  分类). The dialogs live in AppShell and the backend keeps working after
 *  they are closed, so this strip — mounted in the TitleBar — reports every
 *  in-flight run and reopens its dialog on click. A finished run keeps its
 *  chip ("已完成") until the user opens the dialog: otherwise finishing
 *  while the user is on another page would go unnoticed. */
export function GlobalAiTasks() {
  const [review, setReview] = useState<ChipState | null>(null);
  const [table, setTable] = useState<ChipState | null>(null);
  const [classify, setClassify] = useState<ChipState | null>(null);

  const reviewOpen = useReviewStore((s) => s.open);
  const tableOpen = useAiTableStore((s) => s.open);
  const classifyOpen = useAiClassifyStore((s) => s.open);

  useEffect(() => {
    const unReview = listen<ReviewProgress>('library:review-progress', (e) => {
      const p = e.payload;
      if (REVIEW_TERMINAL_STAGES.has(p.stage)) {
        setReview({
          label:
            p.stage === 'done'
              ? '综述 已完成'
              : p.stage === 'partial'
                ? '综述 部分完成'
                : p.stage === 'cancelled'
                  ? '综述 已取消'
                  : '综述 失败',
          running: false,
        });
        return;
      }
      const done = p.donePapers ?? p.doneSections ?? null;
      const total = p.totalPapers ?? (p.totalSections || null);
      setReview({ label: done != null && total ? `综述 ${done}/${total}` : '综述 进行中', running: true });
    });
    const unTable = listen<AiTableProgress>('library:ai-table-progress', (e) => {
      const p = e.payload;
      // Reasoning beats carry placeholder counters — keep the real progress.
      if (p.reasoning) return;
      if (p.finished) {
        setTable({
          label:
            p.finished === 'done' ? '表格 已完成' : p.finished === 'cancelled' ? '表格 已取消' : '表格 已停止',
          running: false,
        });
        return;
      }
      setTable({ label: `表格 ${p.done + p.failed}/${p.total}`, running: true });
    });
    const unClassify = listen<ClassifyProgress>(AI_CLASSIFY_PROGRESS_EVENT, (e) => {
      const p = e.payload;
      if (p.finished) {
        setClassify({
          label:
            p.finished === 'done'
              ? '分类 已完成 · 待审阅'
              : p.finished === 'cancelled'
                ? '分类 已取消'
                : '分类 失败',
          running: false,
        });
        return;
      }
      const label =
        p.stage === 'assign'
          ? `分类判定 ${p.done}/${p.total}`
          : p.stage === 'merge'
            ? '分类合并类目中'
            : p.stage === 'hierarchize'
              ? '分类归纳层级中'
              : `分类起草 ${p.done}/${p.total}`;
      setClassify({ label, running: true });
    });
    return () => {
      unReview.then((fn) => fn());
      unTable.then((fn) => fn());
      unClassify.then((fn) => fn());
    };
  }, []);

  const openReview = () => {
    setReview(null);
    useReviewStore.getState().reopen();
  };
  const openTable = () => {
    setTable(null);
    useAiTableStore.getState().reopen();
  };
  // The classify dialog owns its own session state; re-open it without
  // touching that state (no openWith — that would need the paper set and
  // reset the run).
  const openClassify = () => {
    setClassify(null);
    useAiClassifyStore.setState({ open: true });
  };

  const chips = [
    review && !reviewOpen ? { key: 'review', chip: review, onClick: openReview } : null,
    table && !tableOpen ? { key: 'table', chip: table, onClick: openTable } : null,
    classify && !classifyOpen ? { key: 'classify', chip: classify, onClick: openClassify } : null,
  ].filter((c): c is NonNullable<typeof c> => c != null);
  if (chips.length === 0) return null;

  return (
    <div
      data-no-drag
      data-tauri-drag-region="false"
      className="flex items-center gap-1 px-1 shrink-0"
      aria-label="AI 任务"
    >
      {chips.map(({ key, chip, onClick }) => (
        <button
          key={key}
          data-testid={`ai-task-chip-${key}`}
          onClick={onClick}
          title="点击打开对应任务窗口"
          className={`flex items-center gap-1 px-2 h-[22px] rounded-full border text-[11px] whitespace-nowrap transition-colors ${
            chip.running
              ? 'border-primary/40 bg-primary/10 text-primary hover:bg-primary/20'
              : 'border-emerald-500/40 bg-emerald-500/10 text-emerald-400 hover:bg-emerald-500/20'
          }`}
        >
          {chip.running ? <Loader2 size={10} className="animate-spin" /> : <Check size={10} />}
          {chip.label}
        </button>
      ))}
    </div>
  );
}
