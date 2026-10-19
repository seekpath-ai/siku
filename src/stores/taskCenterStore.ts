import { create } from 'zustand';
import { listen } from '@tauri-apps/api/event';
import {
  AI_CLASSIFY_PROGRESS_EVENT,
  type AiTableProgress,
  type ClassifyProgress,
  type ReviewProgress,
} from '@/lib/tauri';

/** 全局任务中心：汇总本次会话的长时间 AI 任务（综述 / 表格列 / 分类）。
 *  监听逻辑从 GlobalAiTasks 提升到本 store，标题栏 chips 与侧栏任务面板
 *  共用同一份状态。会话级内存状态，不持久化。批量导入由消费方直接订阅
 *  useBatchImportStore 合并展示（其 store 已镜像后端进度事件）。 */

export type TaskKind = 'review' | 'table' | 'classify';
export type TaskStatus = 'running' | 'done' | 'partial' | 'cancelled' | 'error' | 'stopped';

export interface TaskCenterItem {
  kind: TaskKind;
  /** 任务名，如 "AI 综述"。 */
  title: string;
  /** 状态行，如 "综述 2/5" / "综述 已完成"。 */
  label: string;
  status: TaskStatus;
  startedAt: number;
  updatedAt: number;
}

interface TaskCenterState {
  items: Partial<Record<TaskKind, TaskCenterItem>>;
  /** 打开对应对话框时移除记录（chip 同时消失）；新进度事件会重新建立记录。 */
  dismiss: (kind: TaskKind) => void;
  clearFinished: () => void;
}

export const useTaskCenterStore = create<TaskCenterState>((set) => ({
  items: {},
  dismiss: (kind) =>
    set((s) => {
      const items = { ...s.items };
      delete items[kind];
      return { items };
    }),
  clearFinished: () =>
    set((s) => {
      const items: typeof s.items = {};
      for (const [kind, item] of Object.entries(s.items)) {
        if (item.status === 'running') items[kind as TaskKind] = item;
      }
      return { items };
    }),
}));

function upsert(kind: TaskKind, title: string, label: string, status: TaskStatus) {
  useTaskCenterStore.setState((s) => {
    const prev = s.items[kind];
    const now = Date.now();
    const continuing = prev?.status === 'running' && status === 'running';
    return {
      items: {
        ...s.items,
        [kind]: {
          kind,
          title,
          label,
          status,
          startedAt: continuing ? prev.startedAt : now,
          updatedAt: now,
        },
      },
    };
  });
}

const REVIEW_TERMINAL_STAGES = new Set(['done', 'partial', 'cancelled', 'error']);

let listenersReady = false;

/** 幂等安装三个 AI 进度事件监听；GlobalAiTasks 与任务面板挂载时各调一次。 */
export function ensureTaskCenterListeners() {
  if (listenersReady) return;
  listenersReady = true;

  listen<ReviewProgress>('library:review-progress', (e) => {
    const p = e.payload;
    if (REVIEW_TERMINAL_STAGES.has(p.stage)) {
      const status: TaskStatus =
        p.stage === 'done' ? 'done' : p.stage === 'partial' ? 'partial' : p.stage === 'cancelled' ? 'cancelled' : 'error';
      upsert(
        'review',
        'AI 综述',
        status === 'done'
          ? '综述 已完成'
          : status === 'partial'
            ? '综述 部分完成'
            : status === 'cancelled'
              ? '综述 已取消'
              : '综述 失败',
        status
      );
      return;
    }
    // Streaming delta/reasoning beats carry no counters (Default::default());
    // keep the last real progress instead of degrading to "综述 进行中" for
    // the whole writing phase.
    if (p.delta || p.reasoning) return;
    const done = p.donePapers ?? p.doneSections ?? null;
    const total = p.totalPapers ?? (p.totalSections || null);
    upsert('review', 'AI 综述', done != null && total ? `综述 ${done}/${total}` : '综述 进行中', 'running');
  }).catch(() => {});

  listen<AiTableProgress>('library:ai-table-progress', (e) => {
    const p = e.payload;
    // Reasoning beats carry placeholder counters — keep the real progress.
    if (p.reasoning) return;
    if (p.finished) {
      const status: TaskStatus = p.finished === 'done' ? 'done' : p.finished === 'cancelled' ? 'cancelled' : 'stopped';
      upsert(
        'table',
        'AI 表格列',
        status === 'done' ? '表格 已完成' : status === 'cancelled' ? '表格 已取消' : '表格 已停止',
        status
      );
      return;
    }
    upsert('table', 'AI 表格列', `表格 ${p.done + p.failed}/${p.total}`, 'running');
  }).catch(() => {});

  listen<ClassifyProgress>(AI_CLASSIFY_PROGRESS_EVENT, (e) => {
    const p = e.payload;
    if (p.finished) {
      const status: TaskStatus = p.finished === 'done' ? 'done' : p.finished === 'cancelled' ? 'cancelled' : 'error';
      upsert(
        'classify',
        'AI 文献分类',
        status === 'done' ? '分类 已完成 · 待审阅' : status === 'cancelled' ? '分类 已取消' : '分类 失败',
        status
      );
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
    upsert('classify', 'AI 文献分类', label, 'running');
  }).catch(() => {});
}
