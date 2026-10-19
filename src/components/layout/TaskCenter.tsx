import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { CheckCircle2, ListChecks, Loader2, MinusCircle, XCircle } from 'lucide-react';
import {
  ensureTaskCenterListeners,
  useTaskCenterStore,
  type TaskStatus,
} from '@/stores/taskCenterStore';
import { useBatchImportStore } from '@/stores/batchImportStore';
import { useAiTableStore } from '@/stores/aiTableStore';
import { useReviewStore } from '@/stores/reviewStore';
import { useAiClassifyStore } from '@/stores/aiClassifyStore';

type PanelKind = 'review' | 'table' | 'classify' | 'batchImport';

interface PanelEntry {
  kind: PanelKind;
  title: string;
  label: string;
  status: TaskStatus;
  updatedAt: number;
  disabled?: boolean;
}

function StatusIcon({ status }: { status: TaskStatus }) {
  if (status === 'running') return <Loader2 size={14} className="animate-spin text-primary shrink-0" />;
  if (status === 'done' || status === 'partial') return <CheckCircle2 size={14} className="text-emerald-400 shrink-0" />;
  if (status === 'cancelled') return <MinusCircle size={14} className="text-text-secondary shrink-0" />;
  return <XCircle size={14} className="text-red-400 shrink-0" />;
}

/** 侧栏底部的任务中心入口 + 面板。面板列出本次会话的长时间任务
 *  （AI 任务来自 taskCenterStore，批量导入直接订阅其 store 合并进来），
 *  点击条目恢复对应窗口；已结束的记录保留至手动清除。 */
export function TaskCenterButton() {
  const [open, setOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  const items = useTaskCenterStore((s) => s.items);
  const bi = useBatchImportStore();
  const tableRestorable = useAiTableStore((s) => s.paperIds.length > 0);

  useEffect(() => {
    ensureTaskCenterListeners();
  }, []);

  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      const t = e.target as Node;
      if (panelRef.current?.contains(t) || buttonRef.current?.contains(t)) return;
      setOpen(false);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('click', onClick, true);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('click', onClick, true);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [open]);

  const entries: PanelEntry[] = (
    Object.values(items).filter((it): it is NonNullable<typeof it> => it != null) as PanelEntry[]
  )
    .map((it) => (it.kind === 'table' && !tableRestorable ? { ...it, disabled: true } : it))
    .concat(
      bi.total > 0 || bi.running
        ? [
            {
              kind: 'batchImport' as const,
              title: bi.label,
              label: bi.running
                ? `导入 ${bi.current}/${bi.total}`
                : bi.cancelled
                  ? '已取消'
                  : bi.failed > 0
                    ? `已完成 · ${bi.failed} 失败`
                    : '已完成',
              status: bi.running ? ('running' as const) : bi.cancelled ? ('cancelled' as const) : ('done' as const),
              updatedAt: bi.updatedAt,
            },
          ]
        : []
    )
    .sort((a, b) => b.updatedAt - a.updatedAt);

  const runningCount =
    Object.values(items).filter((it) => it?.status === 'running').length + (bi.running ? 1 : 0);
  const hasFinished = entries.some((e) => e.status !== 'running');

  const restore = (entry: PanelEntry) => {
    if (entry.disabled) return;
    setOpen(false);
    if (entry.kind === 'batchImport') {
      useBatchImportStore.getState().reopen();
      return;
    }
    // Same rule as the title-bar chips: only terminal records are dismissed,
    // a running task keeps its record so it reappears the moment the dialog
    // closes again.
    if (entry.status !== 'running') useTaskCenterStore.getState().dismiss(entry.kind);
    if (entry.kind === 'review') useReviewStore.getState().reopen();
    else if (entry.kind === 'table') useAiTableStore.getState().reopen();
    else useAiClassifyStore.setState({ open: true });
  };

  const clearFinished = () => {
    useTaskCenterStore.getState().clearFinished();
    const s = useBatchImportStore.getState();
    if (!s.running && !s.open) s.reset();
  };

  return (
    <>
      <button
        ref={buttonRef}
        data-testid="task-center-button"
        onClick={() => setOpen((o) => !o)}
        title="任务中心"
        className={`relative w-[34px] h-[34px] rounded flex items-center justify-center transition-colors mb-0.5 ${
          open
            ? 'bg-surface-hover text-primary'
            : 'text-text-secondary hover:text-text-primary hover:bg-surface-hover/60'
        }`}
      >
        <ListChecks size={18} />
        {runningCount > 0 && (
          <span
            data-testid="task-center-badge"
            className="absolute -top-1 -right-1 min-w-[14px] h-[14px] px-0.5 rounded-full bg-primary text-white text-[9px] leading-[14px] text-center"
          >
            {runningCount}
          </span>
        )}
      </button>
      {open &&
        createPortal(
          <div
            ref={panelRef}
            data-testid="task-center-panel"
            className="fixed z-[250] w-[300px] max-h-[60dvh] flex flex-col rounded-xl border border-surface-hover bg-surface shadow-2xl overflow-hidden"
            style={{ left: 52, bottom: 12 }}
          >
            <div className="flex items-center px-3 py-2.5 border-b border-surface-hover">
              <span className="text-xs font-medium text-text-primary">任务中心</span>
              {hasFinished && (
                <button
                  data-testid="task-center-clear"
                  onClick={clearFinished}
                  className="ml-auto text-[11px] text-text-secondary hover:text-text-primary transition-colors"
                >
                  清除已结束任务
                </button>
              )}
            </div>
            <div className="overflow-y-auto py-1">
              {entries.length === 0 ? (
                <div className="px-3 py-6 text-center text-xs text-text-secondary/60">
                  暂无任务
                </div>
              ) : (
                entries.map((entry) => (
                  <button
                    key={entry.kind}
                    data-testid={`task-center-item-${entry.kind}`}
                    disabled={entry.disabled}
                    onClick={() => restore(entry)}
                    title={entry.disabled ? '该任务无法恢复（文献集已清空）' : '点击打开对应任务窗口'}
                    className="flex w-full items-center gap-2.5 px-3 py-2 text-left transition-colors hover:bg-surface-hover disabled:opacity-40 disabled:cursor-not-allowed"
                  >
                    <StatusIcon status={entry.status} />
                    <span className="min-w-0 flex-1">
                      <span className="block text-xs text-text-primary truncate">{entry.title}</span>
                      <span className="block text-[11px] text-text-secondary truncate">{entry.label}</span>
                    </span>
                  </button>
                ))
              )}
            </div>
          </div>,
          document.body
        )}
    </>
  );
}
