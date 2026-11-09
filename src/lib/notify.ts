import { useSyncExternalStore } from 'react';

/**
 * 全局轻量通知（toast）。
 *
 * 此前的反馈通道各自为政：AppShell 有一处内联 banner（局部 state，外部够不着）、
 * PetConversation 有自己的 inline toast（桌宠是独立窗口，必须用窗口内的实现，
 * 保留）、若干弹窗内嵌错误条。本模块统一主窗口的瞬态反馈：
 *
 * - `notify(text, kind)`：成功/信息/警告/错误，自动消失；
 * - `notifyError(err, prefix)`：invoke 失败的统一入口；
 * - 需要用户确认或必须阅读的错误仍走 useDialog 的模态对话框，不用 toast。
 *
 * 渲染端是 AppShell 里的 <ToastHost />（仅主窗口）。
 */

export type ToastKind = 'success' | 'info' | 'warning' | 'error';

export interface ToastItem {
  id: number;
  kind: ToastKind;
  text: string;
}

let toasts: ToastItem[] = [];
const listeners = new Set<() => void>();
let seq = 1;

function emit() {
  for (const l of listeners) l();
}

export function dismissToast(id: number): void {
  toasts = toasts.filter((t) => t.id !== id);
  emit();
}

export function notify(text: string, kind: ToastKind = 'info', ms = 2800): void {
  const id = seq++;
  toasts = [...toasts.slice(-3), { id, kind, text }]; // 最多同时 4 条
  emit();
  window.setTimeout(() => dismissToast(id), ms);
}

/** invoke/异步失败的统一上报：error 级 toast + console 留底。 */
export function notifyError(err: unknown, prefix?: string): void {
  const msg = err instanceof Error ? err.message : String(err);
  console.error(prefix ?? 'operation failed', err);
  notify(prefix ? `${prefix}：${msg}` : msg, 'error', 4200);
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

export function useToasts(): ToastItem[] {
  return useSyncExternalStore(subscribe, () => toasts);
}
