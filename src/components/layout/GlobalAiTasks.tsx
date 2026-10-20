import { useEffect } from 'react';
import { Check, Loader2 } from 'lucide-react';
import { useAiTableStore } from '@/stores/aiTableStore';
import { useReviewStore } from '@/stores/reviewStore';
import { useAiClassifyStore } from '@/stores/aiClassifyStore';
import { ensureTaskCenterListeners, useTaskCenterStore, type TaskKind } from '@/stores/taskCenterStore';

/** Global entry point for long-running library AI tasks (综述 / 表格列 /
 *  分类). The dialogs live in AppShell and the backend keeps working after
 *  they are closed, so this strip — mounted in the TitleBar — reports every
 *  in-flight run and reopens its dialog on click. A finished run keeps its
 *  chip ("已完成") until the user opens the dialog: otherwise finishing
 *  while the user is on another page would go unnoticed. Progress state lives
 *  in taskCenterStore, shared with the sidebar task-center panel. */
export function GlobalAiTasks() {
  const items = useTaskCenterStore((s) => s.items);

  const reviewOpen = useReviewStore((s) => s.open);
  const tableOpen = useAiTableStore((s) => s.open);
  const classifyOpen = useAiClassifyStore((s) => s.open);

  useEffect(() => {
    ensureTaskCenterListeners();
  }, []);

  const openTask = (kind: TaskKind) => {
    // Only a terminal record is dismissed on open. A running task keeps its
    // record: the chip hides via dialogOpen and must reappear on close without
    // waiting for the next non-reasoning progress beat (thinking models can
    // go a whole paper without one).
    const item = useTaskCenterStore.getState().items[kind];
    if (item && item.status !== 'running') useTaskCenterStore.getState().dismiss(kind);
    if (kind === 'review') useReviewStore.getState().reopen();
    else if (kind === 'table') useAiTableStore.getState().reopen();
    // The classify dialog owns its own session state; re-open it without
    // touching that state (no openWith — that would need the paper set and
    // reset the run).
    else useAiClassifyStore.setState({ open: true });
  };

  const dialogOpen: Record<TaskKind, boolean> = {
    review: reviewOpen,
    table: tableOpen,
    classify: classifyOpen,
  };

  const chips = (Object.entries(items) as [TaskKind, (typeof items)[TaskKind]][])
    .filter((e): e is [TaskKind, NonNullable<(typeof items)[TaskKind]>] => e[1] != null && !dialogOpen[e[0]]);
  if (chips.length === 0) return null;

  return (
    <div
      data-no-drag
      data-tauri-drag-region="false"
      className="flex items-center gap-1 px-1 shrink-0"
      aria-label="AI 任务"
    >
      {chips.map(([key, item]) => (
        <button
          key={key}
          data-testid={`ai-task-chip-${key}`}
          onClick={() => openTask(key)}
          title="点击打开对应任务窗口"
          className={`flex items-center gap-1 px-2 h-[22px] rounded-full border text-[11px] whitespace-nowrap transition-colors ${
            item.status === 'running'
              ? 'border-primary/40 bg-primary/10 text-primary hover:bg-primary/20'
              : 'border-emerald-500/40 bg-emerald-500/10 text-emerald-400 hover:bg-emerald-500/20'
          }`}
        >
          {/* No infinite CSS animation here: in a transparent frameless window
              every spinner frame forces a full-window alpha recomposite, which
              freezes the Win11 DWM path. Static icon + progress text instead. */}
          {item.status === 'running' ? <Loader2 size={10} /> : <Check size={10} />}
          {item.label}
        </button>
      ))}
    </div>
  );
}
